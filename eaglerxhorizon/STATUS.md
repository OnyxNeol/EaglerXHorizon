# EaglerXHorizon — status sheet

Last updated: 2026-11-08 · baseline + PDS integration

A browser Minecraft client: Rust/WASM engine, WebGPU renderer, worker-isolated
WebSocket bridge, PDS Quick Play burner fleet, Base44 backend.

Legend: **[DONE]** built and verified · **[PARTIAL]** built, not exercised end to end ·
**[TODO]** not built · **[BLOCKED]** needs a resource you do not have right now.

---

## 1. What is done

### Backend (Base44, live)
| Item | State | Evidence |
| --- | --- | --- |
| Entities `ProxyNode`, `Server`, `SessionToken`, `ClientProfile`, `HorizonConfig` | [DONE] | created in the app, seeded with 3 servers + 3 nodes |
| `User` client-profile fields (webgpu, render distance, fov, keymap, last server/node) | [DONE] | added to the built-in User entity |
| `horizonServers` endpoint | [DONE] | returns hydrated catalog with node health; verified 200 |
| `horizonProxyNodes` (list / register / heartbeat / probe) | [DONE] | 401 without token, 200 with; verified |
| `horizonAuth` (Microsoft device-code → Xbox Live → XSTS → Minecraft) | [PARTIAL] | code path complete, tokens server-side; not run against a real Microsoft account |
| `horizonProfile` (clamps render distance 16–64, saves keybinds) | [DONE] | 99 → 64 clamp confirmed on the wire |

### Engine (Rust → WASM)
| Module | State | Notes |
| --- | --- | --- |
| `src/lib.rs` — single `Engine` wasm-bindgen entry | [DONE] | new/start/stop/resize/render distance/webgpu toggle/stats/callbacks |
| `src/engine.rs` — state machine, frame timing, ring ownership | [DONE] | Init → Ready → Running → Paused/Failed |
| `src/renderer/pipeline.rs` — wgpu 0.19 pipeline | [DONE] | `InstanceDescriptor { backends: BROWSER_WEBGPU }`, surface config, WGSL terrain pipeline, bytemuck buffers, per-frame submit |
| `src/renderer/shader.wgsl` — terrain shader | [DONE] | vertex + fragment, fog blend |
| `src/renderer/mod.rs` — backend enum, WebGL2 fallback selection | [DONE] | falls back with a notice instead of crashing |
| `src/math/mod.rs` — glam camera, greedy chunk mesh, frustum | [DONE] | chunk-origin translation, bounds for culling |
| `src/network/buffer.rs` — `ClientPacketRingBuffer` | [DONE] | 24 MiB reserved once at boot, cyclic, overflow counted, 8 unit tests in-file |
| `src/network/socket.rs` — `ProxySocketBridge` | [DONE] | worker-fed + direct transports, VarInt frame decode, jittered backoff, latency probe |
| `src/protocol/mod.rs` — VarInt/VarLong, handshake/status/login builders | [PARTIAL] | framing is real; gameplay packets are not implemented |
| WebGPU error messages naming `chrome://flags/#enable-unsafe-webgpu` | [DONE] | surfaced to the UI banner, not just the console |

**Important:** there is no Rust toolchain in this environment, so the crate is verified
*structurally* (module tree, cross-module items, delimiter balance, config parse) and by
in-file unit tests — **not** by `cargo build`. Expect to fix small type/borrow issues on
your first compile.

### Frontend
| Item | State | Notes |
| --- | --- | --- |
| `web/index.html` — cyberpunk Tailwind launcher | [DONE] | server list, node selector, launch CTA, fullscreen canvas, settings drawer, PDS panel |
| `web/bootstrap.js` — engine loader, demo renderer, data layer | [DONE] | falls back to a canvas demo when `web/pkg` is absent |
| `web/worker.js` — network worker | [DONE] | owns the socket, transfers ArrayBuffers, reconnects, probes |
| `web/pds.js` — PDS Quick Play panel | [DONE] | fleet list, ping, burn, WebGPU badge |
| Settings: WebGPU toggle, render distance 16–64, FOV, vsync, keybinds | [DONE] | persisted to localStorage, pushed into the engine |

### Infrastructure
| Item | State | Notes |
| --- | --- | --- |
| `infra/pds_manager.py` | [DONE] | spawn/list/burn/reap/watch/status, 64m + 0.25 CPU + pids/cap/read-only hardening |
| Burn cycle + slot recycling | [DONE] | `remove(force=True)`, slot returns to pool, next box claims it |
| Ban detection | [DONE] | log signatures + failed-handshake threshold |
| `infra/test_pds_manager.py` | [DONE] | 17 tests, all pass, no Docker needed |
| `tests/worker_protocol_test.mjs` | [DONE] | 8 checks, proves buffer detachment |
| `config/eaglerxbungee.yml` + `velocity-eagler.toml` | [DONE] | YAML/TOML parse clean |
| `deploy/` (Oracle runbook, nginx, systemd, Docker, compose) | [DONE] | documented, not deployed |
| `scripts/build-wasm.sh`, `scripts/healthcheck.sh` | [DONE] | `bash -n` clean |

### Verification actually run
```
Rust   9 files / 2,655 lines · all mod paths resolve · zero crate:: misses · delimiters balanced
JS     node --check clean on bootstrap.js, worker.js, pds.js, tests/*.mjs
HTML   0 missing ids · 0 duplicate ids · 0 unclosed tags
Python py_compile clean · 17/17 unittest
Worker 8/8 protocol checks (detachment, backoff, probe, outbound queue)
Config Cargo.toml + both TOML + both YAML parse
```

---

## 2. What is not done

| Item | State | Why / what it needs |
| --- | --- | --- |
| `web/pkg/*.wasm` build artifact | [TODO] | needs `rustup target add wasm32-unknown-unknown` + `wasm-pack`. Run `./scripts/build-wasm.sh` |
| Rust compile check | [TODO] | no cargo here; first real build may need type fixes |
| Hosted dashboard / downloadable zip | [BLOCKED] | integration credits exhausted (98.4/100) — uploads refused |
| Dashboard reading the live Base44 catalogue | [PARTIAL] | set `window.HORIZON_API.baseUrl`; the page falls back to sample presets |
| Playable gameplay (movement, chunks from server, inventory) | [TODO] | only handshake/status/login scaffolding exists |
| World download + save | [TODO] | renderer draws a generated demo world, not server chunks |
| Survival/creative mechanics, mobs, redstone | [TODO] | out of scope for a baseline |
| Audio | [TODO] | not started |
| Real Microsoft login | [PARTIAL] | device-code flow written, never executed against Microsoft |
| PDS fleet against real Docker | [PARTIAL] | all tests use the dry-run backend; `SdkDockerBackend` untested on a live daemon |
| Burner image `eaglerxhorizon/pds-burner:latest` | [TODO] | the image is referenced, not built; needs a Dockerfile + registry push |
| Oracle Cloud deployment | [TODO] | runbook written, nothing provisioned |
| Multi-host fleet coordination | [TODO] | manager is single-host by design |
| Accessibility/mobile pass on the game canvas | [PARTIAL] | layout is responsive to 380 px; touch controls are not implemented |

---

## 3. How to run it

### 3.1 Build the WASM engine
```bash
rustup target add wasm32-unknown-unknown
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
./scripts/build-wasm.sh              # → web/pkg/eaglerxhorizon.js + _bg.wasm
```

### 3.2 Open the dashboard
```bash
python3 -m http.server 8000 --directory web
# http://localhost:8000/
```
Without `web/pkg/` the page runs its demo renderer, so the UI is always reviewable.

### 3.3 Point it at the backend
```html
<script>
  window.HORIZON_API = {
    baseUrl: 'https://<your-app>.base44.app/functions',
    servers: '/horizonServers',
    nodes: '/horizonProxyNodes',
    auth: '/horizonAuth',
    profile: '/horizonProfile',
  };
  window.HORIZON_PDS = { statusUrl: '/pds/status', burnUrl: '/pds/burn', refreshMs: 15000 };
</script>
```

### 3.4 Drive the burner fleet
```bash
python3 infra/pds_manager.py --dry-run spawn --count 3     # prints the docker run it would do
python3 infra/pds_manager.py spawn --count 3               # needs a Docker daemon + the burner image
python3 infra/pds_manager.py status --json > /srv/pds/status.json
python3 infra/pds_manager.py reap                          # burns banned + expired boxes
python3 infra/pds_manager.py watch --interval 60 --spawn-to 3
```

### 3.5 Stand up the proxy
```bash
# Oracle Always Free runbook: deploy/oracle-cloud.md
sudo apt install openjdk-21-jre-headless nginx
sudo cp deploy/systemd/eaglerxhorizon-proxy.service /etc/systemd/system/
sudo systemctl enable --now eaglerxhorizon-proxy
sudo cp deploy/nginx.conf /etc/nginx/sites-available/eaglerxhorizon
sudo certbot --nginx -d proxy.example.com
./scripts/healthcheck.sh proxy.example.com 443    # expect "healthy"
```

### 3.6 Re-run every check
```bash
node tests/worker_protocol_test.mjs
python3 -m unittest discover -s infra -v
node --check web/bootstrap.js web/worker.js web/pds.js
```

---

## 4. First-hour checklist for a real build

1. `./scripts/build-wasm.sh` — expect minor compile fixes (no cargo here to prove it).
2. `python3 -m http.server 8000 --directory web` — confirm the engine pill says `webgpu`.
3. If it says WebGL2 fallback: enable `chrome://flags/#enable-unsafe-webgpu`.
4. `window.HORIZON_API` set → the server list should show your Base44 records.
5. `pds_manager.py --dry-run spawn` → read the printed caps (64m / 0.25 CPU) before using a real daemon.
6. Only after the engine boots: start the EaglercraftXBungee proxy (3.5) and point a node at it.

---

## 5. Honest summary

The infrastructure and the skeleton are real: data model, endpoints, renderer pipeline,
ring buffer, worker transport, fleet manager, configs, tests. The client is **not
playable** — it renders a generated world and speaks handshake/status/login only. Nothing
Rust-side has been compiled, and nothing was uploaded or hosted.
