# EaglerXHorizon - Base44 backend contract

Everything the launcher needs is served by four backend functions on this app plus five
entities. Reads are public (a launcher has to show the server list before anyone signs in);
writes require an authenticated admin or a write token.

Base URL: `https://superagent-d6c4a61b.base44.app/functions`

---

## 1. Data model

### `User` (built-in, extended)

The platform `User` entity is read-only through the data API, so the columns below document
intent and are safe for admin queries, while `ClientProfile` is where the launcher actually
persists settings.

| Field | Type | Notes |
| --- | --- | --- |
| `role` | `admin` / `user` | built-in |
| `display_name` | string | in-client player name |
| `webgpu_enabled` | boolean | prefer WebGPU over the WebGL2 fallback |
| `render_distance` | number | chunks, 16-64 |
| `fov` | number | 60-110 |
| `vsync`, `muted` | boolean | |
| `master_volume` | number | 0-100 |
| `keybindings` | object | action -> KeyboardEvent.code |
| `client_settings` | object | full settings blob |
| `last_server_slug`, `last_proxy_node_id` | string | resume state |

### `ClientProfile`

Writable twin of the settings above, keyed by `user_email`. One row per player.

### `ProxyNode`

`name`, `url` (ws:// or wss://), `region`, `provider`
(`oracle-always-free` / `self-hosted` / `vps` / `docker` / `local` / `other`),
`status` (`online` / `degraded` / `offline` / `unknown`), `ping_ms`, `protocol_versions`,
`tls`, `max_players`, `players_online`, `last_seen`, `notes`.

### `Server`

`name`, `slug` (unique, launcher id), `description`, `host`, `port`, `proxy_node_id`,
`proxy_url`, `region`, `protocol_versions`, `players_online`, `max_players`, `ping_ms`,
`status` (`online` / `degraded` / `offline` / `maintenance` / `unknown`),
`kind` (`public` / `private` / `local`), `featured`, `icon_url`, `sort_order`.

### `SessionToken`

`user_email`, `provider` (`microsoft` / `minecraft` / `offline`),
`status` (`pending` / `authorized` / `expired` / `denied` / `error`), `device_code` and
`access_token` (server-side only, never returned), `user_code`, `verification_uri`,
`poll_interval`, `expires_at`, `last_polled_at`, `minecraft_name`, `minecraft_uuid`, `error`.

### `HorizonConfig`

`key` / `value` pairs. Recognised keys:

| Key | Purpose |
| --- | --- |
| `microsoft_client_id` | Azure app client id used for the device-code flow |
| `admin_write_token_sha256` | SHA-256 of the write token accepted as `X-Horizon-Token` |
| `client_build`, `proxy_stack` | free-form build metadata shown in the launcher |

---

## 2. `horizonServers`

| Call | Result |
| --- | --- |
| `GET /horizonServers` | `{ ok, servers[], nodes[], count, generated_at }` |
| `GET /horizonServers?slug=donutsmp` | `{ ok, server, nodes }` or 404 |
| `POST { "action": "list" }` | same as GET |
| `POST { "action": "upsert", "server": {...} }` | admin/token only, create or update by `slug` |
| `POST { "action": "remove", "slug": "..." }` | admin/token only |

Each server comes back hydrated:

```json
{
  "slug": "donutsmp",
  "effective_status": "online",
  "effective_ping_ms": 74,
  "playable": true,
  "proxy_url": "wss://fra-1.eaglerxhorizon.dev/ws",
  "proxy_node": { "id": "...", "name": "Oracle A1 Frankfurt #1", "status": "online", "ping_ms": 74 }
}
```

`effective_status` downgrades to `degraded` when the linked node is degraded or offline, and
`playable` is false whenever the node is offline. That is the only rule the launcher needs
before it dials.

## 3. `horizonProxyNodes`

| Call | Result |
| --- | --- |
| `GET /horizonProxyNodes` | `{ ok, nodes[], count }`, sorted by ping |
| `POST { "action": "heartbeat", "node_id", "ping_ms", "players_online" }` | launcher reports real browser RTT |
| `POST { "action": "register", "name", "url", ... }` | admin/token only |
| `POST { "action": "probe" }` | admin/token only, server-side HTTP probe of every node |
| `POST { "action": "remove", "node_id" }` | admin/token only |

`probe` cannot dial a WebSocket from a serverless worker, so it requests the HTTP(S) sibling
of the same host and port (EaglercraftXBungee serves the static client there) and marks the
node `online` / `degraded` / `offline` from that result.

Every node also carries `latency_tier`: `excellent` (<60 ms), `good` (<120), `fair` (<220),
`poor`, `offline`, `unknown`.

## 4. `horizonAuth`

Device-code flow with Microsoft, then the retail chain
Xbox Live -> XSTS -> Minecraft services. Requires a signed-in app user.

| Call | Result |
| --- | --- |
| `POST { "action": "state" }` | `{ session, provider_ready }` |
| `POST { "action": "start" }` | `{ session }` with `user_code` and `verification_uri` |
| `POST { "action": "start", "provider": "offline" }` | instant local session, no Microsoft |
| `POST { "action": "poll" }` | advances `pending` -> `authorized` / `denied` / `expired` / `error` |
| `POST { "action": "cancel" }` | expires every pending row for this user |

Tokens never reach the browser: `publicView()` strips `device_code`, `access_token` and
`refresh_token`.

## 5. `horizonProfile`

| Call | Result |
| --- | --- |
| `GET /horizonProfile` | `{ profile: { email, display_name, settings, keybindings, last_server_slug } }` |
| `POST { "settings": { "render_distance": 32 } }` | clamps to 16-64 and returns the stored profile |
| `POST { "keybindings": { "inventory": "Tab" } }` | merges over the defaults |
| `POST { "reset": true }` | restores defaults |

---

## 6. Calling it from the dashboard

The dashboard reads its base URL from `window.HORIZON_API`:

```js
window.HORIZON_API = {
  baseUrl: 'https://superagent-d6c4a61b.base44.app/functions',
  servers: '/horizonServers',
  nodes: '/horizonProxyNodes',
  auth: '/horizonAuth',
  profile: '/horizonProfile',
};
```

Every endpoint sends permissive CORS headers and answers `OPTIONS` with `204`, so the
dashboard can be hosted anywhere (Base44 file storage, GitHub Pages, the proxy's own static
root).

## 7. Admin bootstrap

```bash
# 1. store the Azure app id so SIGN IN works
curl -X POST 'https://superagent-d6c4a61b.base44.app/functions/horizonProfile' ...
# instead insert a HorizonConfig row: key=microsoft_client_id

# 2. optionally enable token writes
TOKEN=$(openssl rand -hex 24)
printf '%s' "$TOKEN" | sha256sum    # store the hash as admin_write_token_sha256
curl -X POST 'https://superagent-d6c4a61b.base44.app/functions/horizonServers' \
  -H "X-Horizon-Token: $TOKEN" -H 'Content-Type: application/json' \
  -d '{"action":"upsert","server":{"name":"My Box","slug":"my-box","status":"online"}}'
```

## 8. Verification status

Checked live against this app on 2026-11-08:

| Check | Result |
| --- | --- |
| `GET horizonServers` | 200, three servers hydrated with node health |
| `GET horizonProxyNodes` | 200, three nodes with latency tiers |
| `POST heartbeat` | 200, `ping_ms` and `players_online` persisted, `last_seen` refreshed |
| `GET horizonProfile` | 200, defaults merged for a fresh user |
| `POST horizonProfile` | 200, `render_distance: 99` clamped to 64, keybinding merge kept |
| `POST horizonAuth {state}` | 200, `provider_ready: false` until `microsoft_client_id` exists |
