// EaglerXHorizon — PDS Quick Play panel.
//
// Drives the "burner fleet": ephemeral, resource-capped proxy containers managed by
// infra/pds_manager.py. This module renders their live state, runs Quick Play (dial
// the fastest healthy box through web/worker.js) and shows the WebGPU badge.
//
// Fleet source:
//   window.HORIZON_PDS = { statusUrl: 'https://host/pds/status', burnUrl: '...' }
// Without it the panel runs on a simulated fleet so the UI is reviewable offline.

const $ = (id) => document.getElementById(id);

const SAMPLE_FLEET = [
  {
    id: 'pds-7f3a',
    slot: '217.19.44.12',
    url: 'wss://quickplay-1.eaglerxhorizon.dev/ws',
    region: 'ap-tokyo-1',
    state: 'ready',
    ping_ms: 34,
    memory_mb: 64,
    cpus: 0.25,
    uptime_s: 412,
  },
  {
    id: 'pds-b91c',
    slot: '217.19.44.13',
    url: 'wss://quickplay-2.eaglerxhorizon.dev/ws',
    region: 'eu-frankfurt-1',
    state: 'ready',
    ping_ms: 71,
    memory_mb: 64,
    cpus: 0.25,
    uptime_s: 96,
  },
  {
    id: 'pds-2c08',
    slot: '217.19.44.14',
    url: 'wss://quickplay-3.eaglerxhorizon.dev/ws',
    region: 'us-ashburn-1',
    state: 'burned',
    ping_ms: null,
    memory_mb: 64,
    cpus: 0.25,
    uptime_s: 0,
  },
];

const STATE_STYLE = {
  ready: { dot: 'bg-lime2', text: 'text-lime2', label: 'ready' },
  starting: { dot: 'bg-warn', text: 'text-warn', label: 'starting' },
  draining: { dot: 'bg-mag', text: 'text-mag', label: 'draining' },
  burned: { dot: 'bg-danger', text: 'text-danger', label: 'burned' },
  banned: { dot: 'bg-danger', text: 'text-danger', label: 'banned' },
};

const config = window.HORIZON_PDS || {};

const state = {
  fleet: [],
  selectedId: null,
  worker: null,
  workerReady: false,
  connected: false,
  latency: null,
  frames: 0,
  dropped: 0,
  lastStatus: 'fleet idle',
  burnCount: 0,
};

function style(stateName) {
  return STATE_STYLE[stateName] || { dot: 'bg-slate-500', text: 'text-slate-400', label: stateName || 'unknown' };
}

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[char]));
}

function formatUptime(seconds) {
  if (!seconds) return '—';
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/* ------------------------------------------------------------------ loading */

async function loadFleet() {
  if (!config.statusUrl) {
    state.fleet = SAMPLE_FLEET.map((box) => ({ ...box }));
    setStatus('simulated fleet (no infra/pds_manager.py endpoint configured)');
    render();
    return;
  }
  try {
    const response = await fetch(config.statusUrl, { headers: { Accept: 'application/json' } });
    if (!response.ok) throw new Error(`status ${response.status}`);
    const payload = await response.json();
    state.fleet = Array.isArray(payload) ? payload : payload.boxes || [];
    setStatus(`live fleet · ${state.fleet.length} containers`);
  } catch (error) {
    state.fleet = SAMPLE_FLEET.map((box) => ({ ...box }));
    setStatus(`pds status unreachable (${String(error).slice(0, 40)}) — simulated fleet`);
  }
  render();
}

/* ------------------------------------------------------------------ render */

function render() {
  const list = $('pds-list');
  const ready = state.fleet.filter((box) => box.state === 'ready');
  $('pds-count').textContent = `${ready.length}/${state.fleet.length} boxes`;
  $('pds-state').textContent = state.lastStatus;

  list.innerHTML = '';
  if (state.fleet.length === 0) {
    list.innerHTML = '<p class="rounded-lg border border-dashed border-edge px-3 py-4 text-center font-mono text-[11px] text-slate-500">no burner boxes — run infra/pds_manager.py spawn</p>';
    return;
  }

  for (const box of state.fleet) {
    const look = style(box.state);
    const selected = box.id === state.selectedId;
    const row = document.createElement('button');
    row.type = 'button';
    row.dataset.boxId = box.id;
    row.className = [
      'flex w-full items-center gap-2 rounded-lg border px-2.5 py-2 text-left transition',
      selected ? 'border-mag/60 bg-mag/10' : 'border-transparent hover:border-edge hover:bg-panel2/70',
      'focus:outline-none focus-visible:ring-2 focus-visible:ring-mag/50',
    ].join(' ');
    const ping = box.id === state.selectedId && state.latency !== null ? `${Math.round(state.latency)}ms` : box.ping_ms === null || box.ping_ms === undefined ? '—' : `${box.ping_ms}ms`;
    row.innerHTML = `
      <span class="h-1.5 w-1.5 flex-none rounded-full ${look.dot}"></span>
      <span class="min-w-0 flex-1">
        <span class="block truncate font-mono text-[11px] text-slate-200">${escapeHtml(box.id)} <span class="text-slate-600">·</span> ${escapeHtml(box.slot)}</span>
        <span class="block truncate font-mono text-[10px] text-slate-500">${escapeHtml(box.region || '')} · ${box.memory_mb || 64}m/${box.cpus || 0.25}cpu · ${formatUptime(box.uptime_s)}</span>
      </span>
      <span class="flex-none text-right">
        <span class="block font-mono text-[10px] ${look.text}">${look.label}</span>
        <span class="block font-mono text-[10px] text-slate-500">${ping}</span>
      </span>`;
    row.addEventListener('click', () => {
      state.selectedId = box.id;
      render();
    });
    list.appendChild(row);
  }
}

function setStatus(message) {
  state.lastStatus = message;
  const el = $('pds-state');
  if (el) el.textContent = message;
}

function updatePing() {
  const el = $('pds-ping');
  if (!el) return;
  if (state.latency === null || !state.connected) {
    el.textContent = state.connected ? 'ping …' : 'ping —';
    return;
  }
  el.textContent = `ping ${Math.round(state.latency)}ms`;
}

/* ------------------------------------------------------------- webgpu badge */

function renderWebGpuBadge() {
  const badge = $('webgpu-badge');
  if (!badge) return;
  const supported = typeof navigator !== 'undefined' && Boolean(navigator.gpu);
  const preference = window.HORIZON?.state?.settings?.webgpu;

  let label = 'webgpu ready';
  let className = 'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider border-cy/40 bg-cy/10 text-cy';
  if (!supported) {
    label = 'webgl2 fallback';
    className = 'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider border-warn/40 bg-warn/10 text-warn';
  } else if (preference === false) {
    label = 'webgpu off';
    className = 'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider border-edge bg-panel2 text-slate-400';
  }
  badge.textContent = label;
  badge.className = className;
  badge.title = supported
    ? 'navigator.gpu is available: the engine will request a BROWSER_WEBGPU adapter'
    : 'Enable chrome://flags/#enable-unsafe-webgpu for hardware acceleration';
}

/* --------------------------------------------------------------- quick play */

function ensureWorker() {
  if (state.worker) return state.worker;
  const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });

  worker.addEventListener('message', (event) => {
    const message = event.data || {};
    switch (message.type) {
      case 'open':
        state.connected = true;
        setStatus(`burner ${state.selectedId} connected · worker owns the socket`);
        updatePing();
        render();
        break;
      case 'latency':
        state.latency = typeof message.ms === 'number' ? message.ms : null;
        updatePing();
        if (state.selectedId) {
          const box = state.fleet.find((entry) => entry.id === state.selectedId);
          if (box && state.latency !== null) box.ping_ms = Math.round(state.latency);
          render();
        }
        break;
      case 'frame': {
        state.frames += 1;
        const bytes = new Uint8Array(message.bytes);
        const engine = window.HORIZON?.getEngine ? window.HORIZON.getEngine() : null;
        if (engine && typeof engine.ingest_packet === 'function') {
          // straight into the pre-allocated 24 MiB ring buffer
          engine.ingest_packet(bytes);
        }
        break;
      }
      case 'close':
        state.connected = false;
        state.latency = null;
        setStatus(`burner closed (${message.code || 1006})${message.reason ? ` · ${message.reason}` : ''}`);
        updatePing();
        render();
        break;
      case 'status':
        setStatus(message.state === 'reconnecting' ? `reconnect attempt ${message.attempt} in ${message.delay}ms` : `worker ${message.state}`);
        break;
      case 'error':
        setStatus(`worker error: ${String(message.message).slice(0, 60)}`);
        break;
      default:
        break;
    }
  });

  worker.addEventListener('error', (error) => {
    setStatus(`worker failed to start: ${String(error.message || error).slice(0, 60)}`);
  });

  state.worker = worker;
  return worker;
}

function pickFastestBox() {
  const ready = state.fleet.filter((box) => box.state === 'ready');
  if (ready.length === 0) return null;
  return ready.slice().sort((a, b) => (a.ping_ms ?? 9999) - (b.ping_ms ?? 9999))[0];
}

function quickPlay() {
  const box = state.selectedId ? state.fleet.find((entry) => entry.id === state.selectedId) : pickFastestBox();
  if (!box || box.state !== 'ready') {
    setStatus('no ready burner box — spawn one with infra/pds_manager.py spawn');
    return;
  }

  state.selectedId = box.id;
  state.latency = null;
  setStatus(`dialing burner ${box.id} (${box.slot})…`);

  const worker = ensureWorker();
  worker.postMessage({ type: 'connect', url: box.url, policy: { baseMs: 400, maxMs: 15000, jitter: 0.35 }, probeMs: 4000 });

  const probe = window.HORIZON?.getEngine ? window.HORIZON.getEngine() : null;
  if (probe && typeof probe.next_probe_frame === 'function') {
    const frame = probe.next_probe_frame();
    if (frame && frame.length) worker.postMessage({ type: 'probe', bytes: frame });
  }
  render();
}

function burnSelected() {
  const box = state.selectedId ? state.fleet.find((entry) => entry.id === state.selectedId) : null;
  if (!box) {
    setStatus('select a burner box first');
    return;
  }
  state.burnCount += 1;
  box.state = 'burned';
  box.ping_ms = null;
  if (state.worker) state.worker.postMessage({ type: 'close', code: 1000, reason: 'burned by operator' });
  state.connected = false;
  state.latency = null;
  setStatus(`docker rm -f ${box.id} · slot ${box.slot} queued for recycling`);
  updatePing();
  render();

  if (config.burnUrl) {
    fetch(config.burnUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ box_id: box.id, reason: 'manual burn' }),
    }).catch(() => {
      setStatus('burn request failed — container may still be running');
    });
  }

  // simulate the replacement box claiming the freed slot
  setTimeout(() => {
    if (state.fleet.some((entry) => entry.id === box.id && entry.state === 'burned')) {
      state.fleet = state.fleet.filter((entry) => entry.id !== box.id);
      state.fleet.push({
        id: `pds-${Math.random().toString(16).slice(2, 6)}`,
        slot: box.slot,
        url: box.url,
        region: box.region,
        state: 'starting',
        ping_ms: null,
        memory_mb: 64,
        cpus: 0.25,
        uptime_s: 0,
      });
      setStatus(`fresh burner on slot ${box.slot} starting (64m / 0.25 cpu)`);
      render();
      setTimeout(() => {
        const fresh = state.fleet[state.fleet.length - 1];
        if (fresh && fresh.state === 'starting') {
          fresh.state = 'ready';
          render();
        }
      }, 1500);
    }
  }, 900);
}

/* -------------------------------------------------------------------- boot */

function wire() {
  $('pds-launch')?.addEventListener('click', quickPlay);
  $('pds-burn')?.addEventListener('click', burnSelected);

  if (window.HORIZON_PDS?.refreshMs) {
    setInterval(loadFleet, Math.max(5000, window.HORIZON_PDS.refreshMs));
  }

  renderWebGpuBadge();
  window.addEventListener('horizon:ready', renderWebGpuBadge);
  // the settings toggle can change the preference after boot
  setInterval(renderWebGpuBadge, 2000);

  window.HORIZON_PDS_API = {
    quickPlay,
    burnSelected,
    loadFleet,
    state,
    get workerStats() {
      return { frames: state.frames, dropped: state.dropped, latency: state.latency, connected: state.connected };
    },
  };
}

wire();
loadFleet().catch((error) => setStatus(`fleet load failed: ${String(error).slice(0, 60)}`));
