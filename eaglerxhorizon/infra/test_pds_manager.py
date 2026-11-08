"""Unit tests for infra/pds_manager.py — no Docker daemon required.

Run:
    python3 -m unittest discover -s infra -v
"""

from __future__ import annotations

import time
import unittest

from pds_manager import (
    CPU_QUOTA,
    HANDSHAKE_FAILURE_THRESHOLD,
    LABEL_KEY,
    MEMORY_LIMIT,
    NANO_CPUS,
    PIDS_LIMIT,
    BurnerBox,
    DryRunBackend,
    PdsManager,
    SlotPool,
)


class LoggingBackend(DryRunBackend):
    """DryRun plus per-container log text, so ban detection can be exercised."""

    def __init__(self) -> None:
        super().__init__()
        self.log_text = {}

    def set_log(self, container_id: str, text: str) -> None:
        self.log_text[container_id] = text

    def logs(self, container_id: str, tail: int = 200) -> str:
        return self.log_text.get(container_id, "")


class SlotPoolTests(unittest.TestCase):
    def test_allocate_and_release_round_trip(self):
        pool = SlotPool(["10.0.0.1", "10.0.0.2"])
        first = pool.allocate("box-a")
        second = pool.allocate("box-b")
        self.assertEqual({first, second}, {"10.0.0.1", "10.0.0.2"})
        pool.release(first)
        self.assertIn(first, pool.free)
        self.assertEqual(pool.allocate("box-c"), first)

    def test_exhausted_pool_raises(self):
        pool = SlotPool(["10.0.0.1"])
        pool.allocate("box-a")
        with self.assertRaises(RuntimeError):
            pool.allocate("box-b")


class SpawnTests(unittest.TestCase):
    def setUp(self):
        self.backend = LoggingBackend()
        self.manager = PdsManager(self.backend, slots=["10.0.0.1", "10.0.0.2"], max_age_seconds=600)

    def test_spawn_enforces_the_container_caps(self):
        boxes = self.manager.spawn(region="ap-tokyo-1", count=1)
        self.assertEqual(len(boxes), 1)

        run_call = next(call for call in self.backend.calls if call["action"] == "run")
        self.assertEqual(run_call["mem_limit"], MEMORY_LIMIT, "memory must be capped at 64m")
        self.assertEqual(MEMORY_LIMIT, "64m")
        self.assertEqual(run_call["nano_cpus"], NANO_CPUS)
        self.assertEqual(NANO_CPUS, 250_000_000)
        self.assertEqual(CPU_QUOTA, 0.25)
        self.assertEqual(run_call["pids_limit"], PIDS_LIMIT)
        self.assertEqual(run_call["labels"][LABEL_KEY], "1")

    def test_spawned_box_is_ready_and_public_shape_matches_dashboard(self):
        box = self.manager.spawn(count=1)[0]
        self.assertEqual(box.state, "ready")
        public = box.to_public()
        for key in ("id", "slot", "url", "region", "state", "ping_ms", "memory_mb", "cpus", "uptime_s"):
            self.assertIn(key, public)
        self.assertTrue(public["url"].startswith("wss://"))
        self.assertNotIn("container_id", public, "internal container ids stay server-side")

    def test_two_spawns_take_two_distinct_slots(self):
        boxes = self.manager.spawn(count=2)
        self.assertEqual(len({box.slot for box in boxes}), 2)

    def test_running_out_of_slots_raises_and_leaks_nothing(self):
        self.manager.spawn(count=2)
        with self.assertRaises(RuntimeError):
            self.manager.spawn(count=1)
        self.assertEqual(len(self.manager.boxes), 2)
        self.assertEqual(self.manager.pool.free, [])


class BurnTests(unittest.TestCase):
    def setUp(self):
        self.backend = LoggingBackend()
        self.manager = PdsManager(self.backend, slots=["10.0.0.1"], max_age_seconds=600)
        self.box = self.manager.spawn(region="eu-frankfurt-1")[0]

    def test_burn_forces_removal_and_recycles_the_slot(self):
        freed = self.box.slot
        self.assertTrue(self.manager.burn(self.box.id, "manual burn"))

        rm_call = next(call for call in self.backend.calls if call["action"] == "rm")
        self.assertEqual(rm_call["container_id"], self.box.container_id)
        self.assertTrue(rm_call["force"], "burn must use docker rm -f")
        self.assertNotIn(self.box.id, self.manager.boxes)
        self.assertIn(freed, self.manager.pool.free)

        # the slot goes to the next box, which is the point of the whole exercise
        replacement = self.manager.spawn(region="eu-frankfurt-1")[0]
        self.assertEqual(replacement.slot, freed)

    def test_burn_unknown_box_returns_false(self):
        self.assertFalse(self.manager.burn("pds-nope"))

    def test_burn_log_records_reason(self):
        self.manager.burn(self.box.id, "exploit")
        self.assertEqual(self.manager.burn_log[-1]["reason"], "exploit")
        self.assertEqual(self.manager.burn_log[-1]["slot"], self.box.slot)


class BanDetectionTests(unittest.TestCase):
    def setUp(self):
        self.backend = LoggingBackend()
        self.manager = PdsManager(self.backend, slots=["10.0.0.1", "10.0.0.2"], max_age_seconds=600)

    def test_log_signature_flags_a_box(self):
        box = self.manager.spawn(count=1)[0]
        self.backend.set_log(box.container_id, "player joined\nAUTOBAN triggered for Griefer42\n")
        flagged = self.manager.detect_bans()
        self.assertEqual([entry.id for entry in flagged], [box.id])
        self.assertIn("autoban", flagged[0].banned_reason)

    def test_failed_handshake_threshold_flags_a_box(self):
        box = self.manager.spawn(count=1)[0]
        self.backend.set_log(box.container_id, "handshake failed\n" * HANDSHAKE_FAILURE_THRESHOLD)
        flagged = self.manager.detect_bans()
        self.assertEqual(len(flagged), 1)
        self.assertIn("failed handshakes", flagged[0].banned_reason)

    def test_clean_log_is_not_flagged(self):
        box = self.manager.spawn(count=1)[0]
        self.backend.set_log(box.container_id, "handshake failed\nplayer joined\n")
        self.assertEqual(self.manager.detect_bans(), [])

    def test_reap_burns_banned_boxes(self):
        box = self.manager.spawn(count=1)[0]
        self.backend.set_log(box.container_id, "security violation detected\n")
        report = self.manager.reap()
        self.assertEqual(report["burned"][0]["box_id"], box.id)
        self.assertEqual(report["remaining"], 0)
        self.assertEqual(report["free_slots"], 2, "both slots must be back in the pool")


class ExpiryTests(unittest.TestCase):
    def test_reap_burns_expired_boxes_only(self):
        backend = LoggingBackend()
        manager = PdsManager(backend, slots=["10.0.0.1", "10.0.0.2"], max_age_seconds=60)
        old, fresh = manager.spawn(count=2)
        old.created_at = time.time() - 3600

        report = manager.reap()
        self.assertEqual([entry["box_id"] for entry in report["burned"]], [old.id])
        self.assertEqual([box.id for box in manager.list_boxes()], [fresh.id])

    def test_age_seconds_uses_the_injected_clock(self):
        box = BurnerBox(id="pds-test", slot="10.0.0.9", region="test", created_at=1000.0)
        self.assertEqual(box.age_seconds(now=1060.0), 60)


class StatusTests(unittest.TestCase):
    def test_status_payload_is_json_serialisable_and_complete(self):
        import json

        backend = LoggingBackend()
        manager = PdsManager(backend, slots=["10.0.0.1"])
        manager.spawn(count=1)
        payload = manager.status()

        round_tripped = json.loads(json.dumps(payload))
        self.assertEqual(round_tripped["caps"]["memory_limit"], "64m")
        self.assertEqual(round_tripped["caps"]["cpus"], 0.25)
        self.assertEqual(len(round_tripped["boxes"]), 1)
        self.assertIn("free_slots", round_tripped)
        self.assertIn("burn_log", round_tripped)


class CliTests(unittest.TestCase):
    def test_dry_run_cli_spawn_and_status(self):
        import io
        from contextlib import redirect_stdout

        from pds_manager import main

        buffer = io.StringIO()
        with redirect_stdout(buffer):
            code = main(["--dry-run", "--slots", "10.0.0.1,10.0.0.2", "spawn", "--count", "2"])
        self.assertEqual(code, 0)
        self.assertIn("spawned", buffer.getvalue())

        buffer = io.StringIO()
        with redirect_stdout(buffer):
            code = main(["--dry-run", "--slots", "10.0.0.1", "status", "--json"])
        self.assertEqual(code, 0)
        self.assertIn("\"caps\"", buffer.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
