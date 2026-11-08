# EaglerXHorizon

A browser Minecraft client built as three parts that fit together:

- a **Rust core** compiled to WebAssembly (`wasm-bindgen`)
- a **WebGPU renderer** (`wgpu` 0.19, `Backends::BROWSER_WEBGPU`)
- a **WebSocket proxy bridge** that routes the browser to a real Java server through
  EaglercraftXBungee / EaglerXServer

The launcher dashboard is a single static page: dark cyberpunk Tailwind, no build step.

```
                 ┌──────────────────────────────────────────────┐
   dashboard     │  web/index.html + bootstrap.js  (Tailwind)   │
                 │   servers · nodes · settings · canvas        │
                 └───────────────┬──────────────────────────────┘
                                 │ import('../pkg/eaglerxhorizon.js')
                 ┌───────────────▼──────────────────────────────┐
   Rust / WASM   │  Engine (wasm-bindgen)                        │
                 │   engine.rs   state, frame timing             │
                 │   renderer/   wgpu pipeline + WGSL shader     │
                 │   math/       glam chunk meshes, frustum      │
                 │   network/    ProxySocketBridge (gloo-net)    │
                 │   protocol/   VarInt framing, handshake       │
                 └───────┬───────────────────────────┬──────────┘
                         │ wss://binary frames       │ HTTPS JSON
        ┌────────────────▼──────────────┐   ┌────────▼──────────────────┐
        │ EaglercraftXBungee  :8081     │   │ Base44 backend            │
        │  nginx TLS  →  listener       │   │  servers / proxy_nodes /  │
        │        ↓                      │   │  session_tokens / users   │
        │ Fabric 1.21.1  :25566         │   │  horizonServers  etc.     │
        └───────────────────────────────┘   └───────────────────────────┘
```

## Layout

```
eaglerxhorizon/
├── Cargo.toml                     # crate: cdylib + rlib, wgpu 0.19
├── src/
│   ├── lib.rs                     # the single wasm-bindgen Engine entry point
│   ├── engine.rs                  # state machine, frame timing, proxy ownership
│   ├── renderer/
│   │   ├── mod.rs
│   │   ├── pipeline.rs            # instance/surface/adapter/device/pipeline
│   │   └── shader.wgsl            # terrain vertex + fragment shader
│   ├── network/
│   │   ├── mod.rs
│   │   ├── buffer.rs              # ClientPacketRingBuffer: 24 MiB, pre-allocated, cyclic
│   │   └── socket.rs              # ProxySocketBridge: worker-fed + direct, frame decode
│   ├── math/
│   │   └── mod.rs                 # glam camera, greedy chunk meshes, frustum
│   └── protocol/
│       └── mod.rs                 # VarInt/VarLong, handshake/status/login frames
├── web/
│   ├── index.html                 # launcher dashboard + <canvas id="canvas-target">
│   ├── bootstrap.js               # engine loader, demo renderer, data layer, worker bridge
│   ├── worker.js                  # owns the WebSocket, transfers ArrayBuffers
│   ├── pds.js                     # PDS Quick Play panel + burner fleet status
│   └── README.md                  # how to serve it, HORIZON_API contract
├── config/
│   ├── eaglerxbungee.yml          # EaglercraftXBungee v2 template
│   ├── velocity-eagler.toml       # Velocity + EaglerXServer alternative
│   └── server.properties.snippet  # backend keys that make browser login work
├── deploy/
│   ├── oracle-cloud.md            # Always Free runbook, end to end
│   ├── nginx.conf                 # TLS + WebSocket upgrade
│   ├── systemd/eaglerxhorizon-proxy.service
│   ├── docker-compose.yml
│   └── Dockerfile
├── infra/
│   ├── pds_manager.py             # PDS Quick Play burner fleet (Docker SDK)
│   └── test_pds_manager.py        # 17 unit tests, no daemon needed
├── tests/
│   └── worker_protocol_test.mjs   # worker protocol + transfer-list verification
├── scripts/
│   ├── build-wasm.sh              # wasm-pack build → web/pkg
│   └── healthcheck.sh             # TCP, HTTP, WebSocket 101 with exit codes
└── docs/
    └── base44-api.md              # entity schemas + endpoint contract
```

## Zero-allocation packet path

Two pieces keep the render loop off the network path:

**`src/network/buffer.rs` — `ClientPacketRingBuffer`.** 24 MiB is reserved once, in
`EngineCore::new`, and never resized. Inbound worker payloads are copied into it with one
bounds-checked write (split at the wrap point when needed), then frames are decoded
straight out of that storage. A full ring refuses the write and increments
`overflow_drops` instead of growing the heap: back-pressure beats an OOM in a tab. Stats
(`capacity`, `peak_len`, `wraps`, `frames`, `overflow_drops`) are exported through
`Engine.ring_stats()`.

**`web/worker.js` — the network worker.** It owns the `WebSocket`, the reconnect timers and
the probe clock. Binary frames go back to the main thread as
`postMessage({type:'frame', bytes}, [bytes])`, so the `ArrayBuffer` is *transferred*: the
worker's copy is detached the instant it is posted and the main thread receives ownership
without a copy. `tests/worker_protocol_test.mjs` asserts exactly that (`byteLength === 0`
after the post) plus the reconnect backoff and probe timing.

```js
// main thread side, from web/bootstrap.js
worker.onmessage = (event) => {
  const message = event.data;
  if (message.type === 'frame') engine.ingest_packet(new Uint8Array(message.bytes));
  if (message.type === 'open') engine.mark_proxy_open();
  if (message.type === 'latency') hud.ping(message.ms);
};
worker.postMessage({ type: 'connect', url, probeMs: 4000 });
```

Without `Worker` support the engine falls back to `Engine.connect_direct(url, ...)`, which
runs the same ring buffer on the main thread (`ProxySocketBridge` keeps both transports).

## PDS Quick Play

`infra/pds_manager.py` manages a fleet of disposable, resource-capped proxy containers and
`web/pds.js` renders and drives them from the dashboard: a **PDS Quick Play** button, live
burner boxes with per-box ping, and the WebGPU hardware-acceleration badge.

```bash
python3 infra/pds_manager.py --dry-run spawn --count 3
python3 infra/pds_manager.py reap
python3 -m unittest discover -s infra -v          # 17 tests
```

Caps are not configurable by accident: `mem_limit=64m`, `nano_cpus=250_000_000`,
`pids_limit=64`, `cap_drop=ALL`, read-only root with an 8 MB tmpfs, and a 45 minute TTL.
Banned or abusive boxes are destroyed with `docker rm -f` and their public slot is returned
to the pool for the next container. Full design and the dashboard JSON contract:
`docs/pds-quickplay.md`.

## Verifying the whole tree

```bash
./scripts/build-wasm.sh                     # needs cargo + wasm-pack
node tests/worker_protocol_test.mjs         # 8/8 worker protocol checks
python3 -m unittest discover -s infra -v    # 17/17 fleet tests
node --check web/bootstrap.js web/worker.js web/pds.js
```

## Build the engine

```bash
rustup target add wasm32-unknown-unknown
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
./scripts/build-wasm.sh
```

That writes `web/pkg/eaglerxhorizon.js` and `web/pkg/eaglerxhorizon_bg.wasm`. Optional but
recommended: install binaryen so `wasm-opt -Oz` shrinks the wgpu binary.

## Run the dashboard

```bash
python3 -m http.server 8000 --directory web
# open http://localhost:8000/
```

Without `web/pkg/` the page still runs: `bootstrap.js` detects the missing bundle and
switches to an animated canvas demo renderer, so the UI is always reviewable. With the
bundle present, Launch instantiates the real engine, uploads chunk meshes and connects the
proxy bridge.

To point the dashboard at a live catalogue, set this before `bootstrap.js` loads:

```html
<script>
  window.HORIZON_API = {
    baseUrl: 'https://<your-app>.base44.app/functions',
    servers: '/horizonServers',
    nodes: '/horizonProxyNodes',
    auth: '/horizonAuth',
    profile: '/horizonProfile',
  };
</script>
```

Two sample launcher builds ship with the repo and are served from Base44:
`web/index.html` (source) and a single-file preview build.

## Engine API

```js
const engine = new Engine('canvas-target');
engine.on_fps((fps) => { /* top bar */ });
engine.on_issue((level, message) => { /* banner */ });
engine.set_render_distance(32);      // clamped to 16-64
engine.set_webgpu_enabled(true);     // false → WebGL2 path
await engine.start();                // adapter + device + pipeline + frame loop
await engine.connect_proxy('wss://proxy.example.com/ws', onLatency, onFrame);
engine.resize(w, h);                 // from a ResizeObserver
engine.stats();                      // { state, backend, fps, renderDistance, ... }
engine.stop();
```

`Engine::new` only binds the canvas. Everything GPU is in `start()`, which returns a
`JsValue` whose message names `chrome://flags/#enable-unsafe-webgpu` when WebGPU is
missing, so the dashboard can show the exact fix instead of a stack trace.

## Network path

`ProxySocketBridge` opens a binary-only WebSocket through `gloo-net`, extracts
`ArrayBuffer` payloads, splits VarInt-length frames into `(packet_id, payload)` and hands
each frame to JS. `protocol/mod.rs` builds the handshake, status and login-start packets,
so a real client only has to add the gameplay layer. Reconnects use
`base * 2^n` with jitter, capped at 15 s.

## Stand up the proxy

Read `deploy/oracle-cloud.md`. In short:

```bash
sudo apt install openjdk-21-jre-headless nginx
sudo cp deploy/systemd/eaglerxhorizon-proxy.service /etc/systemd/system/
sudo systemctl enable --now eaglerxhorizon-proxy
sudo cp deploy/nginx.conf /etc/nginx/sites-available/eaglerxhorizon
sudo certbot --nginx -d proxy.example.com
./scripts/healthcheck.sh proxy.example.com 443   # expect "healthy"
```

## Backend

The Base44 app behind this repository already holds the catalogue: `Server`,
`ProxyNode`, `SessionToken`, `ClientProfile`, `HorizonConfig`, plus the extended `User`
client-profile fields. Four functions serve it: `horizonServers`, `horizonProxyNodes`,
`horizonAuth` (Microsoft device-code + Xbox Live → XSTS → Minecraft) and
`horizonProfile`. Field-by-field detail and live verification results are in
`docs/base44-api.md`.

## Status of this baseline

Working: data model and endpoints (verified live), dashboard UI and interactions, chunk
meshing and packet framing logic, deployment assets.

Scaffolding: gameplay packets, world saving, audio. The renderer draws a generated
world, not a downloaded one. Treat the Rust side as a correct skeleton with real
infrastructure rather than a playable client.

Verification is structural and behavioural, not a wasm build: no Rust toolchain runs in
this environment, so the crate is checked for module resolution, cross-module item
existence and delimiter balance, while the JS/Python sides have executable tests.

## License

MIT. Not affiliated with Mojang, Microsoft, Eaglercraft or Oracle.
