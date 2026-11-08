# PDS Quick Play — ephemeral burner proxies

"PDS" here is the *public docker ship* pattern: instead of one long-lived proxy that
everyone shares (and that a single abusive client can poison), Quick Play spin up a
disposable container per session, cap it hard, and burn it when it goes bad.

```
    web/index.html  ──[ PDS Quick Play ]──►  web/pds.js
                                                 │  new Worker('worker.js')
                                                 ▼
                                        web/worker.js  (owns the WebSocket)
                                                 │  transferred ArrayBuffer
                                                 ▼
                                    Rust Engine.ingest_packet → 24 MiB ring buffer
                                                 ▲
    infra/pds_manager.py  ──docker run/rm -f──►  burner container :8081  ──► Java backend
```

## Why the fleet is shaped this way

| Concern | Mitigation |
| --- | --- |
| A griefer burns one IP | boxes live on separate slots from a pool; `burn` returns the slot to the pool for a fresh container |
| Memory blowups inside the proxy | `mem_limit=64m`, `memswap_limit=64m`, `-Xmx48m` and SerialGC in the JVM args |
| CPU starvation of neighbours | `nano_cpus=250000000` (0.25 CPU) |
| Fork bombs / shell escapes | `pids_limit=64`, `cap_drop=ALL`, `no-new-privileges`, read-only root with an 8 MB tmpfs |
| Stale boxes accumulating | 45 minute TTL; `reap` burns anything older |
| Ban waves | log signatures plus a failed-handshake counter; anything flagged is destroyed immediately |

This is infrastructure hygiene, not evasion of anyone's rules: the containers are yours,
the logs are yours, and burning a box is exactly what you want when your own proxy starts
seeing abuse.

## Running it

```bash
pip install docker          # only needed for the real backend
python3 infra/pds_manager.py spawn --count 3
python3 infra/pds_manager.py list
python3 infra/pds_manager.py burn pds-7f3a --reason "manual burn"
python3 infra/pds_manager.py reap
python3 infra/pds_manager.py status --json > /srv/pds/status.json
python3 infra/pds_manager.py watch --interval 60 --spawn-to 3
```

No Docker daemon? Every command accepts `--dry-run` (and falls back to it automatically
when the daemon is unreachable), which prints the exact `docker run` arguments it would
use. The unit tests run entirely in that mode:

```bash
python3 -m unittest discover -s infra -v      # 17 tests
```

## Wiring the dashboard to the fleet

Serve `status --json` from anywhere the browser can reach and point the panel at it:

```html
<script>
  window.HORIZON_PDS = {
    statusUrl: 'https://your-host/pds/status',   // JSON: { boxes: [...] }
    burnUrl:   'https://your-host/pds/burn',     // POST { box_id, reason }
    refreshMs: 15000,
  };
</script>
```

Without `statusUrl` the panel runs a simulated three-box fleet so the UI is reviewable
offline. Each box object is exactly what `BurnerBox.to_public()` returns:

```json
{
  "id": "pds-7f3a",
  "slot": "217.19.44.12",
  "url": "wss://quickplay.eaglerxhorizon.dev/box/pds-7f3a",
  "region": "ap-tokyo-1",
  "state": "ready",
  "ping_ms": 34,
  "memory_mb": 64,
  "cpus": 0.25,
  "uptime_s": 412
}
```

Quick Play then:

1. picks the `ready` box with the lowest `ping_ms` (or the box you selected),
2. spawns `web/worker.js`, which dials that box's `wss://` URL,
3. hands every inbound `ArrayBuffer` to `Engine.ingest_packet()`, which copies it once
   into the pre-allocated ring buffer,
4. shows live latency from the worker's own probe timing, and the WebGPU badge for the
   renderer state.

## API surface (`infra/pds_manager.py`)

```python
PdsManager(backend, slots=[...], max_age_seconds=2700, regions=[...])
    .spawn(region=None, count=1) -> list[BurnerBox]
    .list_boxes() -> list[BurnerBox]
    .detect_bans() -> list[BurnerBox]
    .burn(box_id, reason='manual burn') -> bool
    .expired() -> list[BurnerBox]
    .reap() -> {'burned': [...], 'remaining': int, 'free_slots': int}
    .status() -> dict          # what pds.js fetches
```

Backends implement `spawn`, `logs`, `remove`, `list_managed`:

* `SdkDockerBackend` — real Docker through the SDK.
* `DryRunBackend` — records calls, feeds the tests.

## Limits worth knowing

* One box per public slot. `spawn` raises when the pool is empty rather than silently
  double-binding an address.
* Universe-scale scaling is out of scope: this manages one host's fleet. Multiple hosts
  mean running the manager per host and merging the `status` JSON.
* Ban detection is heuristic (log signatures plus a handshake-failure threshold). It is
  designed to fail *towards* burning a box, because a fresh box is cheap.
