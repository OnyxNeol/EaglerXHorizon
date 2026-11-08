# EaglerXHorizon dashboard

A single static page. No bundler, no framework, no build step.

```
web/
├── index.html     # shell: top bar, server list, nodes, PDS panel, canvas, settings drawer
├── bootstrap.js   # engine loader, data layer, interactions, demo renderer, worker bridge
├── worker.js      # network worker: owns the WebSocket, transfers ArrayBuffers
└── pds.js         # PDS Quick Play panel + burner fleet
```

## Serve it

```bash
python3 -m http.server 8000 --directory web
# or
npx serve web
```

ES modules need a real HTTP origin: opening `index.html` from `file://` blocks the
`import()` of the engine bundle.

## Place the WASM bundle

```bash
../scripts/build-wasm.sh          # writes web/pkg/eaglerxhorizon.js + _bg.wasm
```

`bootstrap.js` imports `../pkg/eaglerxhorizon.js` relative to itself, so the bundle lives
in `web/pkg/` and the page path does not change. If the import fails (no bundle, no
WebGPU, an exception during `start()`), the page falls back to `startDemoEngine()`, an
animated canvas renderer, and shows the reason in the WebGPU notice bar. The UI is
therefore always usable, including on a phone with no WebGPU at all.

## `web/worker.js` and `web/pds.js`

`worker.js` is loaded as a module worker and owns the socket:

| in | out |
| --- | --- |
| `{type:'connect', url, policy?, probeMs?}` | `{type:'open', url}` |
| `{type:'send', bytes}` | `{type:'frame', bytes}` (transferred) |
| `{type:'probe', bytes}` | `{type:'latency', ms}` |
| `{type:'close', code, reason}` | `{type:'close', code, reason}` |
| `{type:'stats'}` | `{type:'status'\|'stats'\|'error'}` |

Frames are posted with a transfer list, so `event.data` is detached after the post and the
main thread takes ownership without copying. `node tests/worker_protocol_test.mjs` proves
it outside a browser.

`pds.js` renders the Quick Play panel from `window.HORIZON_PDS.statusUrl` (falling back to a
simulated fleet), dials the fastest `ready` box through its own worker, and shows the WebGPU
badge. Set the endpoints before either module loads:

```html
<script>
  window.HORIZON_PDS = { statusUrl: '/pds/status', burnUrl: '/pds/burn', refreshMs: 15000 };
</script>
```

## `window.HORIZON_API`

Set this before `bootstrap.js` runs (a `<script>` in `<head>`, or inline in `index.html`)
to read the live catalogue instead of the built-in sample presets:

```html
<script>
  window.HORIZON_API = {
    baseUrl: 'https://<your-app>.base44.app/functions',
    servers: '/horizonServers',   // optional, these are the defaults
    nodes:   '/horizonProxyNodes',
    auth:    '/horizonAuth',
    profile: '/horizonProfile',
  };
</script>
```

The data layer expects:

| Response | Shape |
| --- | --- |
| `GET {baseUrl}{servers}` | `{ ok, servers: [{ slug, name, description, region, players_online, max_players, ping_ms, effective_status, protocol_versions, proxy_url, featured, kind }] }` |
| `GET {baseUrl}{nodes}` | `{ ok, nodes: [{ id, name, url, region, status, ping_ms, protocol_versions, players_online, max_players }] }` |

If either request fails, the page logs a warning, keeps the sample presets and shows
`local sample (api unreachable)` in the footer, so a broken backend never leaves a blank
screen.

## What is wired

| Element | Behaviour |
| --- | --- |
| `#server-list` | one card per server: status dot, players, region, protocol versions, ping bars; click selects (and follows the server's pinned node) |
| `#search-input` | filters by name, slug or region |
| `#node-list` | sorted online-first then by latency; click selects |
| `#refresh-nodes` | opens a real WebSocket to each node URL and measures time-to-open (2.5 s cap) |
| `#launch-btn` | boots the engine on `#canvas-target`, then connects the proxy bridge |
| `#game-layer` | fullscreen canvas takeover, HUD with fps/ping/backend, ESC exits |
| `#settings-drawer` | WebGPU toggle, render distance 16-64, FOV 60-110, vsync, mute, keybinding capture |
| `#copy-flag-btn` | copies `chrome://flags/#enable-unsafe-webgpu` for users without WebGPU |

Settings persist to `localStorage` under `eaglerxhorizon.settings.v1` and are pushed into
the running engine (`set_render_distance`, `set_webgpu_enabled`) as they change.

## Demo mode

Triggered automatically when any of these is true:

1. `web/pkg/eaglerxhorizon.js` is missing (the normal state before a wasm build)
2. `Engine.start()` throws, e.g. no adapter and the WebGL2 path is unavailable
3. the user turns WebGPU off in settings and no GL adapter is offered

The footer and the HUD both say `demo` / `canvas2d` so it is never mistaken for the real
renderer. Console output keeps the underlying reason:

```
[eaglerxhorizon] demo renderer: Error: Failed to fetch dynamically imported module: .../pkg/eaglerxhorizon.js
```

## Responsive layout

One column under 1024 px (servers then nodes then launch), two columns above. The drawer
is full-width below 420 px. Verified down to 380 px.

## Accessibility notes

Every control is a real `<button>` or `<input>`, toggles use `role="switch"` with
`aria-checked`, focus rings are visible (`focus-visible:ring-*`), and animations are
disabled under `prefers-reduced-motion`.
