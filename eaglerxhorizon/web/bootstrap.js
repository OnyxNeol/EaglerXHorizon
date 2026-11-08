// EaglerXHorizon launcher bootstrap.
// Loads the WASM engine when pkg/eaglerxhorizon.js exists, otherwise runs a full
// demo canvas so the dashboard is usable on its own. Reads the live server
// catalog from window.HORIZON_API when configured.

const $ = (id) => document.getElementById(id);

const STORAGE_KEY = 'eaglerxhorizon.settings.v1';

const DEFAULT_KEYBINDS = {
  forward: 'KeyW',
  back: 'KeyS',
  left: 'KeyA',
  right: 'KeyD',
  jump: 'Space',
  sneak: 'ShiftLeft',
  sprint: 'ControlLeft',
  inventory: 'KeyE',
  drop: 'KeyQ',
  chat: 'KeyT',
  command: 'Slash',
  perspective: 'F5',
  fullscreen: 'F11',
};

const KEYBIND_LABELS = {
  forward: 'Move forward',
  back: 'Move back',
  left: 'Strafe left',
  right: 'Strafe right',
  jump: 'Jump',
  sneak: 'Sneak',
  sprint: 'Sprint',
  inventory: 'Inventory',
  drop: 'Drop item',
  chat: 'Open chat',
  command: 'Command line',
  perspective: 'Change perspective',
  fullscreen: 'Fullscreen',
};

const DEFAULT_SETTINGS = {
  webgpu: true,
  renderDistance: 24,
  fov: 80,
  vsync: true,
  mute: false,
  keybinds: { ...DEFAULT_KEYBINDS },
};

const SAMPLE_SERVERS = [
  {
    slug: 'donutsmp',
    name: 'DonutSMP',
    description: 'Public lifesteal network with a browser friendly EaglercraftXBungee front.',
    region: 'eu-frankfurt-1',
    players_online: 186,
    max_players: 500,
    ping_ms: 74,
    protocol_versions: '1.8.8,1.12.2,1.21.4',
    effective_status: 'online',
    kind: 'public',
    proxy_url: 'wss://fra-1.eaglerxhorizon.dev/ws',
  },
  {
    slug: 'unstable-smp',
    name: 'Unstable SMP',
    description: 'Community survival world routed through the Tokyo always-free node.',
    region: 'ap-tokyo-1',
    players_online: 37,
    max_players: 120,
    ping_ms: 41,
    protocol_versions: '1.8.8,1.12.2,1.21.4',
    effective_status: 'online',
    kind: 'public',
    proxy_url: 'wss://tokyo-1.eaglerxhorizon.dev/ws',
  },
  {
    slug: 'private-oracle-vps',
    name: 'Private Oracle VPS',
    description: 'Your own Fabric 1.21.1 backend behind the self-hosted proxy node.',
    region: 'self-hosted',
    players_online: 0,
    max_players: 20,
    ping_ms: 38,
    protocol_versions: '1.8.8,1.12.2,1.21.4',
    effective_status: 'online',
    kind: 'private',
    proxy_url: 'wss://tokyo-1.eaglerxhorizon.dev/ws',
  },
];

const SAMPLE_NODES = [
  {
    id: 'tokyo-1',
    name: 'Oracle A1 Tokyo #1',
    url: 'wss://tokyo-1.eaglerxhorizon.dev/ws',
    region: 'ap-tokyo-1',
    status: 'online',
    ping_ms: 38,
    protocol_versions: '1.8.8,1.12.2,1.21.4',
    players_online: 42,
    max_players: 120,
  },
  {
    id: 'fra-1',
    name: 'Oracle A1 Frankfurt #1',
    url: 'wss://fra-1.eaglerxhorizon.dev/ws',
    region: 'eu-frankfurt-1',
    status: 'online',
    ping_ms: 74,
    protocol_versions: '1.8.8,1.12.2,1.21.4',
    players_online: 18,
    max_players: 120,
  },
  {
    id: 'local',
    name: 'Local Docker bridge',
    url: 'ws://127.0.0.1:8081/ws',
    region: 'localhost',
    status: 'unknown',
    ping_ms: 1,
    protocol_versions: '1.8.8,1.12.2',
    players_online: 0,
    max_players: 8,
  },
];

const api = window.HORIZON_API || null;

const state = {
  servers: [],
  nodes: [],
  selectedServerSlug: null,
  selectedNodeId: null,
  filter: '',
  settings: loadSettings(),
  engine: null,
  engineKind: 'none',
  demoFrame: 0,
  demoRaf: 0,
  netWorker: null,
  probeTimer: 0,
  source: 'local sample',
  probing: false,
};

/* ---------------------------------------------------------------- settings */

function loadSettings() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return structuredClone(DEFAULT_SETTINGS);
    const parsed = JSON.parse(raw);
    return {
      ...DEFAULT_SETTINGS,
      ...parsed,
      keybinds: { ...DEFAULT_KEYBINDS, ...(parsed.keybinds || {}) },
      renderDistance: clamp(Number(parsed.renderDistance) || 24, 16, 64),
      fov: clamp(Number(parsed.fov) || 80, 60, 110),
    };
  } catch {
    return structuredClone(DEFAULT_SETTINGS);
  }
}

function saveSettings() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state.settings));
    const el = $('save-state');
    el.textContent = 'saved locally';
    el.className = 'ml-auto font-mono text-[10px] uppercase tracking-wider text-slate-600';
  } catch {
    const el = $('save-state');
    el.textContent = 'storage blocked';
    el.className = 'ml-auto font-mono text-[10px] uppercase tracking-wider text-warn';
  }
}

const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

function pushSettingsToEngine() {
  const engine = state.engine;
  if (!engine) return;
  try {
    engine.set_render_distance(state.settings.renderDistance);
    engine.set_webgpu_enabled(state.settings.webgpu);
  } catch (error) {
    console.warn('engine rejected settings', error);
  }
}

/* ------------------------------------------------------------------- toast */

let toastTimer = 0;
function toast(message) {
  const el = $('toast');
  el.textContent = message;
  el.classList.remove('hidden');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), 2600);
}

/* -------------------------------------------------------------------- data */

async function fetchJson(path) {
  const base = api.baseUrl.replace(/\/$/, '');
  const response = await fetch(`${base}${path}`, { headers: { Accept: 'application/json' } });
  if (!response.ok) throw new Error(`${path} -> ${response.status}`);
  return response.json();
}

async function loadData({ silent = false } = {}) {
  if (!api || !api.baseUrl) {
    state.servers = structuredClone(SAMPLE_SERVERS);
    state.nodes = structuredClone(SAMPLE_NODES);
    state.source = 'local sample';
    afterData(silent);
    return;
  }

  try {
    const [catalog, nodePayload] = await Promise.all([
      fetchJson(api.servers || '/horizonServers'),
      fetchJson(api.nodes || '/horizonProxyNodes'),
    ]);
    state.servers = (catalog.servers || []).map((server) => ({
      ...server,
      effective_status: server.effective_status || server.status || 'unknown',
      ping_ms: server.effective_ping_ms ?? server.ping_ms ?? null,
    }));
    state.nodes = nodePayload.nodes || [];
    state.source = 'base44 api';
  } catch (error) {
    console.warn('catalog fetch failed, using sample data', error);
    state.servers = structuredClone(SAMPLE_SERVERS);
    state.nodes = structuredClone(SAMPLE_NODES);
    state.source = 'local sample (api unreachable)';
    if (!silent) toast('catalog unavailable — showing sample presets');
  }
  afterData(silent);
}

function afterData(silent) {
  state.nodes.sort((a, b) => {
    const rank = (node) => (node.status === 'online' ? 0 : node.status === 'degraded' ? 1 : 2);
    const byRank = rank(a) - rank(b);
    if (byRank !== 0) return byRank;
    return (a.ping_ms ?? 9999) - (b.ping_ms ?? 9999);
  });

  if (!state.selectedServerSlug || !state.servers.some((s) => s.slug === state.selectedServerSlug)) {
    state.selectedServerSlug = state.servers[0]?.slug ?? null;
  }
  if (!state.selectedNodeId || !state.nodes.some((n) => n.id === state.selectedNodeId)) {
    const preferred = state.nodes.find((node) => node.url === selectedServer()?.proxy_url);
    state.selectedNodeId = preferred?.id ?? state.nodes[0]?.id ?? null;
  }

  $('api-source').textContent = state.source;
  renderServers();
  renderNodes();
  renderSelection();
  if (!silent) toast(`catalog loaded — ${state.servers.length} servers, ${state.nodes.length} nodes`);
}

const selectedServer = () => state.servers.find((server) => server.slug === state.selectedServerSlug) || null;
const selectedNode = () => state.nodes.find((node) => node.id === state.selectedNodeId) || null;
const selectedNodeForServer = () => {
  const server = selectedServer();
  const pinned = state.nodes.find((node) => node.id === server?.proxy_node_id || node.url === server?.proxy_url);
  return pinned || selectedNode();
};

/* ------------------------------------------------------------------ render */

const STATUS_STYLES = {
  online: { dot: 'bg-lime2', text: 'text-lime2', label: 'online' },
  degraded: { dot: 'bg-warn', text: 'text-warn', label: 'degraded' },
  offline: { dot: 'bg-danger', text: 'text-danger', label: 'offline' },
  maintenance: { dot: 'bg-mag', text: 'text-mag', label: 'maintenance' },
  unknown: { dot: 'bg-slate-500', text: 'text-slate-400', label: 'unknown' },
};

const statusStyle = (status) => STATUS_STYLES[status] || STATUS_STYLES.unknown;

function pingClass(ping) {
  if (ping === null || ping === undefined) return 'on-4';
  if (ping < 60) return 'on-1';
  if (ping < 120) return 'on-2';
  if (ping < 220) return 'on-3';
  return 'on-4';
}

function pingBars(ping) {
  const on = pingClass(ping);
  const level = Number(on.slice(-1));
  const heights = ['h-1.5', 'h-2.5', 'h-3.5', 'h-4'];
  return `<span class="flex items-end gap-[2px]" aria-hidden="true">${heights
    .map((height, index) => `<span class="ping-bar ${height} ${index < level ? on : ''}"></span>`)
    .join('')}</span>`;
}

function renderServers() {
  const list = $('server-list');
  const filter = state.filter.trim().toLowerCase();
  const servers = state.servers.filter((server) => {
    if (!filter) return true;
    return `${server.name} ${server.slug} ${server.region || ''}`.toLowerCase().includes(filter);
  });

  $('server-count').textContent = `${state.servers.length} preset${state.servers.length === 1 ? '' : 's'}`;
  list.innerHTML = '';

  if (servers.length === 0) {
    list.innerHTML = `<p class="col-span-full rounded-lg border border-dashed border-edge px-4 py-8 text-center font-mono text-[11px] text-slate-500">no servers match “${filter}”</p>`;
    return;
  }

  for (const server of servers) {
    const style = statusStyle(server.effective_status);
    const active = server.slug === state.selectedServerSlug;
    const card = document.createElement('button');
    card.type = 'button';
    card.dataset.slug = server.slug;
    card.className = [
      'group relative flex flex-col gap-2 rounded-lg border border-edge/80 bg-panel2/60 p-3 text-left transition fade-in',
      'hover:border-cy/40 hover:bg-panel2 focus:outline-none focus-visible:ring-2 focus-visible:ring-cy/50',
      active ? 'card-selected' : '',
    ].join(' ');
    card.innerHTML = `
      <div class="flex items-start gap-2">
        <span class="mt-1.5 h-2 w-2 flex-none rounded-full ${style.dot}"></span>
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-1.5">
            <h3 class="truncate font-display text-[15px] font-bold leading-tight text-white">${escapeHtml(server.name)}</h3>
            ${server.featured ? '<span class="rounded border border-mag/40 px-1 font-mono text-[9px] uppercase tracking-wider text-mag">hot</span>' : ''}
            ${server.kind === 'private' ? '<span class="rounded border border-edge px-1 font-mono text-[9px] uppercase tracking-wider text-slate-400">private</span>' : ''}
          </div>
          <p class="mt-0.5 line-clamp-2 text-[12px] leading-snug text-slate-500">${escapeHtml(server.description || server.host || '')}</p>
        </div>
        <div class="flex flex-none flex-col items-end gap-1">
          ${pingBars(server.ping_ms)}
          <span class="font-mono text-[10px] text-slate-500">${server.ping_ms ?? '—'}ms</span>
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] uppercase tracking-wider text-slate-500">
        <span class="${style.text}">${style.label}</span>
        <span>${server.players_online ?? 0}/${server.max_players ?? '?'}</span>
        <span class="truncate">${escapeHtml(server.region || 'unknown')}</span>
        <span class="truncate text-slate-600">${escapeHtml((server.protocol_versions || '').split(',').join(' · '))}</span>
      </div>`;
    card.addEventListener('click', () => selectServer(server.slug));
    list.appendChild(card);
  }
}

function renderNodes() {
  const list = $('node-list');
  $('node-count').textContent = String(state.nodes.length);
  list.innerHTML = '';

  if (state.nodes.length === 0) {
    list.innerHTML = '<p class="px-2 py-6 text-center font-mono text-[11px] text-slate-500">no proxy nodes registered</p>';
    return;
  }

  for (const node of state.nodes) {
    const style = statusStyle(node.status);
    const active = node.id === state.selectedNodeId;
    const row = document.createElement('button');
    row.type = 'button';
    row.dataset.nodeId = node.id;
    row.className = [
      'flex w-full items-center gap-2 rounded-lg border px-2.5 py-2 text-left transition',
      active ? 'border-cy/60 bg-cy/10' : 'border-transparent hover:border-edge hover:bg-panel2/70',
      'focus:outline-none focus-visible:ring-2 focus-visible:ring-cy/50',
    ].join(' ');
    row.innerHTML = `
      <span class="grid h-6 w-6 flex-none place-items-center rounded-md border border-edge bg-abyss/60">
        <span class="h-1.5 w-1.5 rounded-full ${style.dot}"></span>
      </span>
      <span class="min-w-0 flex-1">
        <span class="block truncate text-[13px] font-semibold text-slate-100">${escapeHtml(node.name)}</span>
        <span class="block truncate font-mono text-[10px] text-slate-500">${escapeHtml(node.url || '')}</span>
      </span>
      <span class="flex-none text-right">
        <span class="block font-mono text-[11px] ${style.text}">${node.ping_ms ?? '—'}ms</span>
        <span class="block font-mono text-[9px] uppercase tracking-wider text-slate-600">${escapeHtml(node.region || style.label)}</span>
      </span>`;
    row.addEventListener('click', () => selectNode(node.id));
    list.appendChild(row);
  }
}

function renderSelection() {
  const server = selectedServer();
  const node = selectedNodeForServer();
  $('selected-server').textContent = server ? server.name : '—';
  $('selected-node').textContent = node ? `${node.name} · ${node.url}` : 'no node selected';

  const summary = $('selected-summary');
  summary.classList.toggle('hidden', !server);
  if (server) {
    summary.innerHTML = `${server.players_online ?? 0}/${server.max_players ?? '?'} online<br />${server.ping_ms ?? '—'}ms · ${escapeHtml(server.region || '')}`;
  }

  const launchable = Boolean(server && node && server.effective_status !== 'offline' && node.status !== 'offline');
  $('launch-btn').disabled = !launchable;
  $('launch-label').textContent = !server ? 'select a server' : !node ? 'select a node' : launchable ? 'launch client' : 'node offline';
}

function selectServer(slug) {
  state.selectedServerSlug = slug;
  const server = selectedServer();
  const pinned = state.nodes.find((node) => node.url === server?.proxy_url);
  if (pinned) state.selectedNodeId = pinned.id;
  renderServers();
  renderNodes();
  renderSelection();
}

function selectNode(id) {
  state.selectedNodeId = id;
  renderNodes();
  renderSelection();
}

/* ------------------------------------------------------- latency measuring */

function probeNode(url, timeoutMs = 2500) {
  return new Promise((resolve) => {
    let settled = false;
    const started = performance.now();
    let socket;
    const finish = (ok) => {
      if (settled) return;
      settled = true;
      try {
        socket?.close();
      } catch {
        /* already closed */
      }
      resolve({ ok, ms: Math.round(performance.now() - started) });
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    try {
      socket = new WebSocket(url);
      socket.binaryType = 'arraybuffer';
      socket.onopen = () => {
        clearTimeout(timer);
        finish(true);
      };
      socket.onerror = () => {
        clearTimeout(timer);
        finish(false);
      };
    } catch {
      clearTimeout(timer);
      finish(false);
    }
  });
}

async function remeasureNodes() {
  if (state.probing) return;
  state.probing = true;
  const button = $('refresh-nodes');
  button.classList.add('animate-spin');
  toast('measuring proxy latency…');
  try {
    await Promise.all(
      state.nodes.map(async (node) => {
        const result = await probeNode(node.url);
        node.ping_ms = result.ms;
        node.status = result.ok ? 'online' : node.status === 'unknown' ? 'unknown' : 'online';
      }),
    );
  } finally {
    state.probing = false;
    button.classList.remove('animate-spin');
    renderNodes();
    renderSelection();
    toast('latency re-measured');
  }
}

/* ------------------------------------------------------------------ engine */

function setEnginePill(text, tone = 'idle') {
  $('engine-pill-text').textContent = text;
  const dot = $('connection-dot');
  dot.className = 'h-1.5 w-1.5 rounded-full ' + (tone === 'live' ? 'bg-lime2 animate-pulse' : tone === 'error' ? 'bg-danger' : tone === 'warn' ? 'bg-warn' : 'bg-slate-500');
}

async function ensureEngineModule() {
  if (state.engineModule) return state.engineModule;
  const module = await import('../pkg/eaglerxhorizon.js');
  if (typeof module.default === 'function') await module.default();
  state.engineModule = module;
  return module;
}

function resizeCanvas() {
  const canvas = $('canvas-target');
  const rect = canvas.getBoundingClientRect();
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const width = Math.max(1, Math.floor(rect.width * dpr));
  const height = Math.max(1, Math.floor(rect.height * dpr));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
    if (state.engine) state.engine.resize(width, height);
  }
  return { width, height };
}

function updateHud() {
  if (state.engineKind === 'wasm' && state.engine) {
    try {
      const stats = state.engine.stats();
      $('game-hud-state').textContent = stats.state;
      $('game-hud-fps').textContent = Math.round(stats.fps);
      $('game-hud-ping').textContent = stats.proxy_latency_ms > 0 ? `${Math.round(stats.proxy_latency_ms)}ms` : '—';
      $('game-hud-backend').textContent = stats.backend;
      $('fps-value').textContent = Math.round(stats.fps);
      $('renderer-state').textContent = `renderer ${stats.backend} · ${stats.width}x${stats.height}`;
    } catch (error) {
      console.warn('stats failed', error);
    }
  }
}

/* demo renderer: keeps the launcher honest when the wasm bundle is absent */
function startDemoEngine(reason) {
  state.engineKind = 'demo';
  const canvas = $('canvas-target');
  const ctx = canvas.getContext('2d');
  setEnginePill('demo renderer', 'warn');
  $('game-hud-state').textContent = 'demo';
  $('game-hud-backend').textContent = 'canvas2d';
  $('renderer-state').textContent = 'renderer canvas2d (demo)';
  console.info('[eaglerxhorizon] demo renderer:', reason);

  cancelAnimationFrame(state.demoRaf);
  const horizon = [];
  for (let x = 0; x < 24; x += 1) {
    horizon.push(20 + Math.sin(x * 0.6) * 6 + Math.cos(x * 0.27) * 9);
  }

  let last = performance.now();
  let frames = 0;

  const frame = (now) => {
    const dt = Math.min(48, now - last);
    last = now;
    frames += 1;
    state.demoFrame += dt * 0.001;

    const { width, height } = canvas;
    const sky = ctx.createLinearGradient(0, 0, 0, height);
    sky.addColorStop(0, '#070d16');
    sky.addColorStop(0.55, '#0b1622');
    sky.addColorStop(1, '#05070d');
    ctx.fillStyle = sky;
    ctx.fillRect(0, 0, width, height);

    // horizon glow
    const glow = ctx.createRadialGradient(width * 0.62, height * 0.2, 0, width * 0.62, height * 0.2, width * 0.7);
    glow.addColorStop(0, 'rgba(34,211,238,0.16)');
    glow.addColorStop(1, 'rgba(34,211,238,0)');
    ctx.fillStyle = glow;
    ctx.fillRect(0, 0, width, height);

    // terrain bands
    const baseY = height * 0.62;
    for (let layer = 0; layer < 5; layer += 1) {
      const depth = layer / 4;
      const amplitude = 26 + depth * 54;
      const shift = state.demoFrame * (10 + depth * 26);
      ctx.beginPath();
      ctx.moveTo(0, height);
      for (let x = 0; x <= width; x += 12) {
        const t = (x + shift) * 0.006 + layer * 1.7;
        const y = baseY + depth * height * 0.22 - (Math.sin(t) * amplitude + Math.cos(t * 0.6) * amplitude * 0.45);
        ctx.lineTo(x, y);
      }
      ctx.lineTo(width, height);
      ctx.closePath();
      const r = Math.round(16 + depth * 18);
      const g = Math.round(38 + depth * 30);
      const b = Math.round(54 + depth * 26);
      ctx.fillStyle = `rgba(${r},${g},${b},${0.55 + depth * 0.35})`;
      ctx.fill();
      ctx.strokeStyle = `rgba(34,211,238,${0.05 + depth * 0.12})`;
      ctx.lineWidth = 1;
      ctx.stroke();
    }

    // grid overlay
    ctx.strokeStyle = 'rgba(34,211,238,0.07)';
    ctx.lineWidth = 1;
    const grid = 64;
    for (let x = ((state.demoFrame * 22) % grid) - grid; x < width; x += grid) {
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, height);
      ctx.stroke();
    }
    for (let y = 0; y < height; y += grid) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(width, y);
      ctx.stroke();
    }

    // hud numbers
    const fps = Math.round(1000 / Math.max(1, dt));
    if (frames % 20 === 0) {
      $('fps-value').textContent = String(fps);
      $('game-hud-fps').textContent = String(fps);
      const jitter = (Math.sin(state.demoFrame * 1.4) * 6).toFixed(0);
      $('ping-value').textContent = `${Math.max(4, Number(jitter) + 32)}ms`;
      $('game-hud-ping').textContent = `${Math.max(4, Number(jitter) + 32)}ms`;
    }

    state.demoRaf = requestAnimationFrame(frame);
  };
  state.demoRaf = requestAnimationFrame(frame);
  void horizon;
}

// Starts the network worker for a proxy node: the WebSocket lives off the main
// thread and packets arrive as transferred ArrayBuffers that go straight into the
// engine's pre-allocated ring buffer.
function startWorkerBridge(engine, url) {
  const latency = (ms) => {
    $('ping-value').textContent = `${Math.round(ms)}ms`;
    $('game-hud-ping').textContent = `${Math.round(ms)}ms`;
  };

  if (typeof Worker === 'undefined') {
    // no worker support: dial directly, still through the same ring buffer
    engine
      .connect_direct(url, latency, () => {})
      .catch((error) => toast('proxy bridge: ' + String(error).slice(0, 80)));
    return;
  }

  stopWorkerBridge();

  const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  state.netWorker = worker;

  worker.onmessage = (event) => {
    const message = event.data || {};
    switch (message.type) {
      case 'open':
        engine.mark_proxy_open();
        setEnginePill('proxy connected', 'live');
        break;
      case 'frame':
        // zero-copy handoff: the worker detached this buffer when posting it
        engine.ingest_packet(new Uint8Array(message.bytes));
        break;
      case 'latency':
        if (typeof message.ms === 'number') latency(message.ms);
        break;
      case 'close':
        engine.mark_proxy_closed(message.code, message.reason || '');
        break;
      case 'status':
        if (message.state === 'reconnecting') {
          setEnginePill(`proxy retry ${message.attempt}`, 'warn');
        }
        break;
      case 'error':
        console.warn('[eaglerxhorizon] worker:', message.message);
        break;
      default:
        break;
    }
  };

  worker.postMessage({ type: 'connect', url, policy: { baseMs: 400, maxMs: 15000, jitter: 0.35 }, probeMs: 4000 });

  // ask Rust for a probe frame and let the worker time the round trip
  clearInterval(state.probeTimer);
  state.probeTimer = setInterval(() => {
    try {
      const frame = engine.next_probe_frame();
      if (frame && frame.byteLength) worker.postMessage({ type: 'probe', bytes: frame.buffer }, [frame.buffer]);
    } catch (error) {
      console.warn('probe failed', error);
    }
  }, 4000);
}

function stopWorkerBridge() {
  clearInterval(state.probeTimer);
  state.probeTimer = 0;
  if (state.netWorker) {
    try {
      state.netWorker.postMessage({ type: 'close', code: 1000, reason: 'launcher exited' });
      state.netWorker.terminate();
    } catch (error) {
      console.warn('worker teardown failed', error);
    }
    state.netWorker = null;
  }
}

async function launchClient() {
  const server = selectedServer();
  const node = selectedNodeForServer();
  if (!server || !node) return;

  $('game-layer').classList.remove('hidden');
  resizeCanvas();
  $('launch-label').textContent = 'launching…';
  setEnginePill('engine booting', 'warn');
  $('game-hud-state').textContent = 'booting';

  const wantsWebGpu = state.settings.webgpu && Boolean(navigator.gpu);
  if (!navigator.gpu) {
    $('webgpu-notice').classList.remove('hidden');
    $('webgpu-banner').classList.remove('hidden');
  }

  try {
    const module = await ensureEngineModule();
    const engine = new module.Engine('canvas-target');
    engine.set_render_distance(state.settings.renderDistance);
    engine.set_webgpu_enabled(wantsWebGpu);
    engine.on_fps?.((fps) => {
      $('fps-value').textContent = String(Math.round(fps));
      $('game-hud-fps').textContent = String(Math.round(fps));
    });
    engine.on_issue?.((level, message) => {
      toast(message);
      if (level === 'warn') $('webgpu-banner').classList.remove('hidden');
    });
    state.engine = engine;
    state.engineKind = 'wasm';
    $('game-hud-backend').textContent = wantsWebGpu ? 'webgpu' : 'webgl2';
    await engine.start();

    const proxyUrl = node.url || server.proxy_url;
    if (proxyUrl) startWorkerBridge(engine, proxyUrl);

    setEnginePill('engine running', 'live');
  } catch (error) {
    // The engine bundle or the GPU was unavailable: keep the launcher usable.
    startDemoEngine(error);
    setEnginePill('demo renderer', 'warn');
    $('webgpu-notice').classList.remove('hidden');
    toast('engine unavailable — demo renderer active');
  } finally {
    $('launch-label').textContent = 'launch client';
  }
}

function exitClient() {
  $('game-layer').classList.add('hidden');
  cancelAnimationFrame(state.demoRaf);
  state.demoFrame = 0;
  stopWorkerBridge();
  if (state.engine) {
    try {
      state.engine.disconnect_proxy();
      state.engine.stop();
    } catch (error) {
      console.warn('engine stop failed', error);
    }
  }
  state.engine = null;
  state.engineKind = 'none';
  setEnginePill('engine idle');
  $('fps-value').textContent = '—';
}

/* ------------------------------------------------------------------ drawer */

function openDrawer() {
  $('settings-drawer').classList.remove('translate-x-full');
  $('settings-drawer').setAttribute('aria-hidden', 'false');
  $('drawer-backdrop').classList.remove('hidden');
}

function closeDrawer() {
  $('settings-drawer').classList.add('translate-x-full');
  $('settings-drawer').setAttribute('aria-hidden', 'true');
  $('drawer-backdrop').classList.add('hidden');
}

function syncSettingsUi() {
  $('set-webgpu').setAttribute('aria-checked', String(state.settings.webgpu));
  $('set-render-distance').value = String(state.settings.renderDistance);
  $('render-distance-value').textContent = String(state.settings.renderDistance);
  $('set-fov').value = String(state.settings.fov);
  $('fov-value').textContent = String(state.settings.fov);
  $('set-vsync').setAttribute('aria-checked', String(state.settings.vsync));
  $('set-mute').setAttribute('aria-checked', String(state.settings.mute));
  renderKeybinds();
}

function renderKeybinds() {
  const list = $('keybind-list');
  list.innerHTML = '';
  for (const [action, code] of Object.entries(state.settings.keybinds)) {
    const row = document.createElement('div');
    row.className = 'flex items-center gap-2 rounded-md border border-edge/70 bg-panel2/50 px-2.5 py-1.5';
    row.innerHTML = `
      <span class="flex-1 truncate text-[12px] text-slate-300">${escapeHtml(KEYBIND_LABELS[action] || action)}</span>
      <button type="button" data-action="${escapeHtml(action)}"
        class="rounded border border-edge bg-abyss/70 px-2 py-1 font-mono text-[10px] uppercase tracking-wider text-cy transition hover:border-cy/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-cy/50">
        ${escapeHtml(formatKey(code))}
      </button>`;
    const button = row.querySelector('button');
    button.addEventListener('click', () => captureKey(action, button));
    list.appendChild(row);
  }
}

function formatKey(code) {
  if (!code) return 'unbound';
  return code
    .replace('Key', '')
    .replace('Digit', '')
    .replace('Left', ' L')
    .replace('Right', ' R')
    .replace('Control', 'Ctrl');
}

function captureKey(action, button) {
  button.textContent = 'press…';
  button.classList.add('recording');
  const handler = (event) => {
    event.preventDefault();
    event.stopPropagation();
    state.settings.keybinds[action] = event.code || event.key;
    button.classList.remove('recording');
    document.removeEventListener('keydown', handler, true);
    saveSettings();
    renderKeybinds();
    pushSettingsToEngine();
  };
  document.addEventListener('keydown', handler, true);
}

/* ------------------------------------------------------------------- wiring */

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[char]));
}

function wire() {
  $('settings-btn').addEventListener('click', openDrawer);
  $('close-settings').addEventListener('click', closeDrawer);
  $('drawer-backdrop').addEventListener('click', closeDrawer);
  $('game-close').addEventListener('click', exitClient);
  $('launch-btn').addEventListener('click', launchClient);
  $('refresh-nodes').addEventListener('click', remeasureNodes);

  $('search-input').addEventListener('input', (event) => {
    state.filter = event.target.value;
    renderServers();
  });

  $('copy-flag-btn').addEventListener('click', async () => {
    const text = 'chrome://flags/#enable-unsafe-webgpu';
    try {
      await navigator.clipboard.writeText(text);
      toast('flag url copied');
    } catch {
      toast(text);
    }
  });

  $('set-webgpu').addEventListener('click', () => {
    state.settings.webgpu = !state.settings.webgpu;
    syncSettingsUi();
    saveSettings();
    pushSettingsToEngine();
    toast(state.settings.webgpu ? 'WebGPU preferred on next launch' : 'WebGL2 fallback selected');
  });

  $('set-render-distance').addEventListener('input', (event) => {
    state.settings.renderDistance = clamp(Number(event.target.value), 16, 64);
    $('render-distance-value').textContent = String(state.settings.renderDistance);
    pushSettingsToEngine();
  });
  $('set-render-distance').addEventListener('change', saveSettings);

  $('set-fov').addEventListener('input', (event) => {
    state.settings.fov = clamp(Number(event.target.value), 60, 110);
    $('fov-value').textContent = String(state.settings.fov);
  });
  $('set-fov').addEventListener('change', saveSettings);

  for (const [id, key] of [
    ['set-vsync', 'vsync'],
    ['set-mute', 'mute'],
  ]) {
    $(id).addEventListener('click', () => {
      state.settings[key] = !state.settings[key];
      syncSettingsUi();
      saveSettings();
    });
  }

  $('reset-settings').addEventListener('click', () => {
    state.settings = structuredClone(DEFAULT_SETTINGS);
    syncSettingsUi();
    saveSettings();
    pushSettingsToEngine();
    toast('settings reset');
  });

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      if (!$('game-layer').classList.contains('hidden')) exitClient();
      else closeDrawer();
    }
  });

  window.addEventListener('resize', () => {
    if (!$('game-layer').classList.contains('hidden')) {
      const { width, height } = resizeCanvas();
      if (state.engine) state.engine.resize(width, height);
    }
  });

  if ('ResizeObserver' in window) {
    const observer = new ResizeObserver(() => {
      if ($('game-layer').classList.contains('hidden')) return;
      const { width, height } = resizeCanvas();
      if (state.engine) state.engine.resize(width, height);
    });
    observer.observe($('canvas-target'));
  }

  setInterval(updateHud, 500);
}

async function boot() {
  $('build-version').textContent = window.HORIZON_BUILD || '0.1.0';
  syncSettingsUi();
  wire();

  if (!navigator.gpu) {
    $('webgpu-notice').classList.remove('hidden');
  }

  await loadData({ silent: true });
  setEnginePill('engine idle');

  window.HORIZON = {
    state,
    reload: () => loadData(),
    selectServer,
    selectNode,
    launch: launchClient,
    exit: exitClient,
    probeNode,
    // the PDS panel feeds worker frames straight into the engine's ring buffer
    getEngine: () => state.engine,
  };

  // let web/pds.js know the launcher state exists (it renders the WebGPU badge from it)
  window.dispatchEvent(new CustomEvent('horizon:ready', { detail: { settings: state.settings } }));
}

boot().catch((error) => {
  console.error('[eaglerxhorizon] boot failed', error);
  toast('launcher failed to boot — see console');
});
