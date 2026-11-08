#!/usr/bin/env python3
"""PDS Quick Play manager — ephemeral "burner box" proxy fleet.

Each Quick Play user gets a throwaway container running the WebSocket proxy bridge.
The boxes are deliberately tiny (64 MB, 0.25 CPU), deliberately disposable, and
deliberately short-lived: if a box is banned, exploited or simply old, it is
destroyed with ``docker rm -f`` and its public slot is handed to a fresh container.

Commands
    spawn [--region R] [--count N]   start burner container(s)
    list                             show the fleet with resource caps
    burn <box_id> [--reason TEXT]    destroy a box and recycle its slot
    reap                             detect banned/expired boxes and burn them
    watch [--interval S]             loop reap + report (default 60s)
    status [--json]                  machine-readable fleet state (feed the dashboard)

Examples
    ./pds_manager.py spawn --count 3
    ./pds_manager.py reap
    ./pds_manager.py status --json > /srv/pds/status.json

The Docker daemon is optional: pass ``--dry-run`` (or simply have no daemon) and
every action is printed and recorded in memory instead of touching Docker. That
mode is what the unit tests exercise, and it is what the dashboard sees on a
laptop with no Docker installed.

Note on scope: this manages *infrastructure you own*. Detecting abuse means reading
your own proxy's logs; nothing here scans or interferes with third-party services.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import string
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Dict, Iterable, List, Optional, Protocol

# --- hardened defaults ------------------------------------------------------

MEMORY_LIMIT = "64m"
CPU_QUOTA = 0.25
NANO_CPUS = int(CPU_QUOTA * 1_000_000_000)  # 250_000_000
PIDS_LIMIT = 64
CONTAINER_PORT = "8081/tcp"                 # proxy listener inside the box
IMAGE = os.environ.get("PDS_IMAGE", "eaglerxhorizon/pds-burner:latest")
NETWORK = os.environ.get("PDS_NETWORK", "pds-quickplay")
LABEL_KEY = "run.place.eaglerxhorizon.pds"

DEFAULT_MAX_AGE_SECONDS = int(os.environ.get("PDS_MAX_AGE_SECONDS", 45 * 60))
HANDSHAKE_FAILURE_THRESHOLD = int(os.environ.get("PDS_FAILURE_THRESHOLD", 8))

# Log fragments that mean "this box is now radioactive".
BAN_SIGNATURES = (
    "banned",
    "blacklisted",
    "autoban",
    "grief",
    "exploit attempt",
    "invalid session (banned)",
    "security violation",
)

DEFAULT_SLOTS: List[str] = [
    "217.19.44.12",
    "217.19.44.13",
    "217.19.44.14",
    "217.19.44.15",
    "217.19.44.16",
    "217.19.44.17",
]


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def short_id() -> str:
    return "pds-" + "".join(random.choices(string.hexdigits.lower()[:16], k=4))


@dataclass
class BurnerBox:
    """One ephemeral proxy container."""

    id: str
    slot: str
    region: str
    state: str = "starting"          # starting | ready | draining | burned | banned
    host: str = "quickplay.eaglerxhorizon.dev"
    port: int = 8081
    memory_mb: int = 64
    cpus: float = CPU_QUOTA
    created_at: float = field(default_factory=lambda: time.time())
    ping_ms: Optional[int] = None
    failed_handshakes: int = 0
    banned_reason: Optional[str] = None
    container_id: Optional[str] = None

    @property
    def url(self) -> str:
        scheme = "wss" if os.environ.get("PDS_TLS", "1") != "0" else "ws"
        return f"{scheme}://{self.host}/box/{self.id}"

    def age_seconds(self, now: Optional[float] = None) -> int:
        return int((now or time.time()) - self.created_at)

    def to_public(self, now: Optional[float] = None) -> Dict[str, Any]:
        """Shape consumed by web/pds.js (see window.HORIZON_PDS.statusUrl)."""
        return {
            "id": self.id,
            "slot": self.slot,
            "url": self.url,
            "region": self.region,
            "state": self.state,
            "ping_ms": self.ping_ms,
            "memory_mb": self.memory_mb,
            "cpus": self.cpus,
            "uptime_s": self.age_seconds(now),
            "failed_handshakes": self.failed_handshakes,
            "banned_reason": self.banned_reason,
        }


class SlotPool:
    """Public addresses that can be handed to a burner box."""

    def __init__(self, slots: Optional[Iterable[str]] = None) -> None:
        self._free: List[str] = list(slots if slots is not None else DEFAULT_SLOTS)
        self._in_use: Dict[str, str] = {}

    def allocate(self, box_id: str) -> str:
        if not self._free:
            raise RuntimeError("no free PDS slots: burn a box or add slots to DEFAULT_SLOTS")
        slot = self._free.pop(0)
        self._in_use[slot] = box_id
        return slot

    def release(self, slot: str) -> None:
        self._in_use.pop(slot, None)
        if slot not in self._free:
            self._free.append(slot)

    @property
    def free(self) -> List[str]:
        return list(self._free)

    @property
    def in_use(self) -> Dict[str, str]:
        return dict(self._in_use)


class ContainerBackend(Protocol):
    """The slice of Docker this manager needs, so tests can fake it."""

    name: str

    def spawn(self, box: BurnerBox) -> str: ...
    def logs(self, container_id: str, tail: int = 200) -> str: ...
    def remove(self, container_id: str, force: bool = True) -> None: ...
    def list_managed(self) -> List[Dict[str, Any]]: ...


class DryRunBackend:
    """Records every action instead of calling Docker."""

    name = "dry-run"

    def __init__(self) -> None:
        self.calls: List[Dict[str, Any]] = []
        self._logs: Dict[str, str] = {}

    def spawn(self, box: BurnerBox) -> str:
        container_id = "fake" + box.id.replace("pds-", "")
        self.calls.append(
            {
                "action": "run",
                "container_id": container_id,
                "image": IMAGE,
                "mem_limit": MEMORY_LIMIT,
                "nano_cpus": NANO_CPUS,
                "pids_limit": PIDS_LIMIT,
                "labels": {LABEL_KEY: "1", "pds.slot": box.slot, "pds.id": box.id},
                "ports": {CONTAINER_PORT: None},
            }
        )
        return container_id

    def logs(self, container_id: str, tail: int = 200) -> str:
        return self._logs.get(container_id, "")

    def remove(self, container_id: str, force: bool = True) -> None:
        self.calls.append({"action": "rm", "container_id": container_id, "force": force})

    def list_managed(self) -> List[Dict[str, Any]]:
        return [call for call in self.calls if call["action"] == "run"]


class SdkDockerBackend:
    """Real Docker via the official SDK (`pip install docker`)."""

    name = "docker"

    def __init__(self) -> None:
        try:
            import docker  # imported lazily so --dry-run needs nothing installed
        except ImportError as error:  # pragma: no cover - environment dependent
            raise RuntimeError("docker SDK missing: pip install docker") from error
        self._docker = docker
        self.client = docker.from_env()
        try:
            self.client.ping()
        except Exception as error:  # pragma: no cover - environment dependent
            raise RuntimeError(f"cannot reach the Docker daemon: {error}") from error
        self._ensure_network()

    def _ensure_network(self) -> None:  # pragma: no cover - needs a daemon
        try:
            self.client.networks.get(NETWORK)
        except Exception:
            self.client.networks.create(NETWORK, driver="bridge", internal=False)

    def spawn(self, box: BurnerBox) -> str:  # pragma: no cover - needs a daemon
        container = self.client.containers.run(
            IMAGE,
            detach=True,
            name=box.id,
            mem_limit=MEMORY_LIMIT,
            memswap_limit=MEMORY_LIMIT,   # no swap escape hatch
            nano_cpus=NANO_CPUS,
            pids_limit=PIDS_LIMIT,
            network=NETWORK,
            read_only=True,
            tmpfs={"/tmp": "size=8m"},     # writable scratch, still capped
            cap_drop=["ALL"],
            security_opt=["no-new-privileges:true"],
            environment={
                "PDS_BOX_ID": box.id,
                "PDS_SLOT": box.slot,
                "PDS_REGION": box.region,
                "JAVA_TOOL_OPTIONS": "-Xmx48m -XX:+UseSerialGC",
            },
            labels={LABEL_KEY: "1", "pds.slot": box.slot, "pds.id": box.id},
            ports={CONTAINER_PORT: None},  # ephemeral host port
        )
        return container.id

    def logs(self, container_id: str, tail: int = 200) -> str:  # pragma: no cover
        container = self.client.containers.get(container_id)
        raw = container.logs(tail=tail, stdout=True, stderr=True)
        return raw.decode("utf-8", "replace") if isinstance(raw, bytes) else str(raw)

    def remove(self, container_id: str, force: bool = True) -> None:  # pragma: no cover
        container = self.client.containers.get(container_id)
        container.remove(force=force, v=True)

    def list_managed(self) -> List[Dict[str, Any]]:  # pragma: no cover
        containers = self.client.containers.list(all=True, filters={"label": f"{LABEL_KEY}=1"})
        return [
            {
                "container_id": container.id,
                "name": container.name,
                "status": container.status,
                "labels": container.labels,
            }
            for container in containers
        ]


class PdsManager:
    """Owns the fleet: slots, containers, ban detection and the burn cycle."""

    def __init__(
        self,
        backend: ContainerBackend,
        slots: Optional[Iterable[str]] = None,
        max_age_seconds: int = DEFAULT_MAX_AGE_SECONDS,
        regions: Optional[Iterable[str]] = None,
    ) -> None:
        self.backend = backend
        self.pool = SlotPool(slots)
        self.max_age_seconds = max_age_seconds
        self.regions = list(regions or ["ap-tokyo-1", "eu-frankfurt-1", "us-ashburn-1"])
        self.boxes: Dict[str, BurnerBox] = {}
        self.burn_log: List[Dict[str, Any]] = []

    # ---------------------------------------------------------------- spawning
    def spawn(self, region: Optional[str] = None, count: int = 1) -> List[BurnerBox]:
        spawned: List[BurnerBox] = []
        for _ in range(max(1, count)):
            box = BurnerBox(id=short_id(), slot="", region=region or random.choice(self.regions))
            box.slot = self.pool.allocate(box.id)
            try:
                box.container_id = self.backend.spawn(box)
                box.state = "ready"
            except Exception as error:
                self.pool.release(box.slot)
                raise RuntimeError(f"failed to start {box.id} on {box.slot}: {error}") from error
            self.boxes[box.id] = box
            spawned.append(box)
        return spawned

    # ---------------------------------------------------------------- inspection
    def list_boxes(self) -> List[BurnerBox]:
        return sorted(self.boxes.values(), key=lambda box: box.created_at)

    def get(self, box_id: str) -> Optional[BurnerBox]:
        return self.boxes.get(box_id)

    # ------------------------------------------------------------ ban detection
    def detect_bans(self) -> List[BurnerBox]:
        """Flags boxes whose logs show a ban signature or too many failed handshakes."""
        flagged: List[BurnerBox] = []
        for box in self.list_boxes():
            if box.container_id is None or box.state in {"burned", "banned"}:
                continue
            log = self.backend.logs(box.container_id, tail=200).lower()
            box.failed_handshakes = log.count("handshake failed") + log.count("invalid username")

            reason = next((signature for signature in BAN_SIGNATURES if signature in log), None)
            if reason is not None:
                box.banned_reason = f"log signature: {reason}"
                flagged.append(box)
            elif box.failed_handshakes >= HANDSHAKE_FAILURE_THRESHOLD:
                box.banned_reason = f"{box.failed_handshakes} failed handshakes"
                flagged.append(box)
        return flagged

    # ------------------------------------------------------------- burn cycle
    def burn(self, box_id: str, reason: str = "manual burn") -> bool:
        """docker rm -f the container and put its public slot back in the pool."""
        box = self.boxes.get(box_id)
        if box is None:
            return False
        if box.container_id is not None:
            self.backend.remove(box.container_id, force=True)
        box.state = "burned" if reason == "manual burn" else "banned"
        box.banned_reason = None if reason == "manual burn" else reason
        self.pool.release(box.slot)
        self.burn_log.append(
            {
                "box_id": box.id,
                "slot": box.slot,
                "reason": reason,
                "at": utcnow().isoformat(),
                "container_id": box.container_id,
            }
        )
        del self.boxes[box_id]
        return True

    def expired(self) -> List[BurnerBox]:
        now = time.time()
        return [box for box in self.list_boxes() if box.age_seconds(now) >= self.max_age_seconds]

    def reap(self) -> Dict[str, Any]:
        """One maintenance pass: burn anything banned or older than the TTL."""
        now = time.time()
        burned: List[Dict[str, str]] = []

        for box in self.detect_bans():
            if self.burn(box.id, box.banned_reason or "banned"):
                burned.append({"box_id": box.id, "reason": box.banned_reason or "banned"})

        for box in list(self.boxes.values()):
            if box.age_seconds(now) >= self.max_age_seconds:
                if self.burn(box.id, f"ttl exceeded ({box.age_seconds(now)}s)"):
                    burned.append({"box_id": box.id, "reason": "ttl"})

        return {
            "burned": burned,
            "remaining": len(self.boxes),
            "free_slots": len(self.pool.free),
            "checked_at": utcnow().isoformat(),
        }

    # ------------------------------------------------------------------ status
    def status(self) -> Dict[str, Any]:
        now = time.time()
        return {
            "backend": self.backend.name,
            "boxes": [box.to_public(now) for box in self.list_boxes()],
            "free_slots": self.pool.free,
            "max_age_seconds": self.max_age_seconds,
            "burn_log": self.burn_log[-10:],
            "caps": {"memory_limit": MEMORY_LIMIT, "cpus": CPU_QUOTA, "pids_limit": PIDS_LIMIT},
            "generated_at": utcnow().isoformat(),
        }


# --------------------------------------------------------------------- CLI


def make_backend(dry_run: bool) -> ContainerBackend:
    if dry_run:
        return DryRunBackend()
    try:
        return SdkDockerBackend()
    except Exception as error:
        print(f"[pds] docker unavailable ({error}); falling back to --dry-run", file=sys.stderr)
        return DryRunBackend()


def build_manager(args: argparse.Namespace) -> PdsManager:
    backend = make_backend(args.dry_run)
    slots = [slot.strip() for slot in args.slots.split(",")] if args.slots else None
    return PdsManager(
        backend,
        slots=slots,
        max_age_seconds=args.max_age,
    )


def format_table(boxes: Iterable[BurnerBox]) -> str:
    rows = ["ID        SLOT             REGION           STATE     AGE   MEM   CPU"]
    for box in boxes:
        rows.append(
            f"{box.id:<9} {box.slot:<16} {box.region:<16} {box.state:<9} "
            f"{box.age_seconds():<5} {box.memory_mb:<5} {box.cpus}"
        )
    if len(rows) == 1:
        rows.append("(no burner boxes — spawn one)")
    return "\n".join(rows)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="PDS Quick Play burner fleet manager")
    parser.add_argument("--dry-run", action="store_true", help="never touch Docker; print actions")
    parser.add_argument("--slots", default=os.environ.get("PDS_SLOTS", ""), help="comma separated public slots")
    parser.add_argument("--max-age", type=int, default=DEFAULT_MAX_AGE_SECONDS, help="box TTL in seconds")
    sub = parser.add_subparsers(dest="command", required=True)

    spawn = sub.add_parser("spawn", help="start burner containers")
    spawn.add_argument("--region", default=None)
    spawn.add_argument("--count", type=int, default=1)

    burn = sub.add_parser("burn", help="destroy a box and recycle its slot")
    burn.add_argument("box_id")
    burn.add_argument("--reason", default="manual burn")

    sub.add_parser("list", help="show the fleet")
    sub.add_parser("reap", help="burn banned or expired boxes")

    status = sub.add_parser("status", help="fleet state as JSON")
    status.add_argument("--json", action="store_true", help="pretty JSON (default: summary)")

    watch = sub.add_parser("watch", help="reap on an interval")
    watch.add_argument("--interval", type=int, default=60)
    watch.add_argument("--spawn-to", type=int, default=0, help="keep at least N ready boxes")

    args = parser.parse_args(argv)

    manager = build_manager(args)

    if args.command == "spawn":
        boxes = manager.spawn(region=args.region, count=args.count)
        for box in boxes:
            print(f"[pds] spawned {box.id} on {box.slot} ({box.region}) {MEMORY_LIMIT}/{CPU_QUOTA}cpu -> {box.url}")
        print(format_table(manager.list_boxes()))
        return 0

    if args.command == "list":
        print(format_table(manager.list_boxes()))
        return 0

    if args.command == "burn":
        if manager.burn(args.box_id, args.reason):
            print(f"[pds] burned {args.box_id} ({args.reason}) and recycled its slot")
            return 0
        print(f"[pds] no such box: {args.box_id}", file=sys.stderr)
        return 1

    if args.command == "reap":
        report = manager.reap()
        for entry in report["burned"]:
            print(f"[pds] burned {entry['box_id']}: {entry['reason']}")
        print(f"[pds] {report['remaining']} boxes alive, {report['free_slots']} free slots")
        return 0

    if args.command == "status":
        payload = manager.status()
        if args.json:
            print(json.dumps(payload, indent=2))
        else:
            print(format_table(manager.list_boxes()))
            print(f"free slots: {len(payload['free_slots'])} · caps: {payload['caps']}")
        return 0

    if args.command == "watch":
        print(f"[pds] watching every {args.interval}s (ttl {args.max_age}s)")
        try:
            while True:
                report = manager.reap()
                for entry in report["burned"]:
                    print(f"[pds] burned {entry['box_id']}: {entry['reason']}")
                if args.spawn_to and len(manager.boxes) < args.spawn_to:
                    wanted = args.spawn_to - len(manager.boxes)
                    for box in manager.spawn(count=wanted):
                        print(f"[pds] replenished {box.id} on {box.slot}")
                print(f"[pds] tick: {report['remaining']} alive, {report['free_slots']} free")
                time.sleep(max(1, args.interval))
        except KeyboardInterrupt:
            print("[pds] watch stopped")
            return 0

    parser.error(f"unknown command {args.command}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
