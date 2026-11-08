// EaglerXHorizon network worker.
//
// Owns the WebSocket so the main thread never blocks on socket I/O, parsing or
// reconnect timers. Binary frames are forwarded with a *transfer list*, so the
// ArrayBuffer is moved (not copied) and the main thread can hand it straight to
// WASM's pre-allocated ring buffer. `event.data` is detached after postMessage:
// that detachment is the proof of zero-copy.
//
// Protocol
//   in : {type:'connect', url}            dial a proxy
//        {type:'send', bytes:ArrayBuffer} forward one outbound frame
//        {type:'probe', bytes:ArrayBuffer} send a probe and time the next reply
//        {type:'config', policy:{baseMs,maxMs,jitter,probeMs}}
//        {type:'close', code, reason}
//   out: {type:'open', url}
//        {type:'frame', bytes:ArrayBuffer}   transferred
//        {type:'latency', ms}
//        {type:'close', code, reason}
//        {type:'status', state, attempt}
//        {type:'error', message}

let socket = null;
let targetUrl = null;
let attempt = 0;
let closedByUs = false;
let reconnectTimer = 0;
let probeStartedAt = 0;
let probePending = false;
let probeIntervalMs = 0;
let probePollTimer = 0;
let pendingOutbound = [];
let stats = { framesIn: 0, bytesIn: 0, bytesOut: 0, opened: 0, closes: 0 };

let policy = { baseMs: 400, maxMs: 15000, jitter: 0.35 };

function post(message, transfer) {
  if (transfer && transfer.length) {
    self.postMessage(message, transfer);
  } else {
    self.postMessage(message);
  }
}

function backoffDelay(nextAttempt) {
  const exponential = Math.min(policy.maxMs, policy.baseMs * Math.pow(2, Math.min(nextAttempt, 8)));
  const jitter = exponential * policy.jitter * Math.random();
  return Math.round(exponential + jitter);
}

function dial() {
  teardownSocket();
  if (!targetUrl) return;

  post({ type: 'status', state: 'connecting', attempt, url: targetUrl });

  let ws;
  try {
    ws = new WebSocket(targetUrl);
  } catch (error) {
    post({ type: 'error', message: String(error && error.message ? error.message : error) });
    scheduleReconnect();
    return;
  }

  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    attempt = 0;
    stats.opened += 1;
    post({ type: 'open', url: targetUrl });
    flushOutbound();
    startProbeLoop();
  };

  ws.onmessage = (event) => {
    const data = event.data;

    if (probePending) {
      probePending = false;
      const ms = Math.max(0, performance.now() - probeStartedAt);
      post({ type: 'latency', ms: Math.round(ms * 10) / 10 });
    }

    if (data instanceof ArrayBuffer) {
      stats.framesIn += 1;
      stats.bytesIn += data.byteLength;
      // transfer the buffer: the worker gives up ownership, main thread gets it free
      post({ type: 'frame', bytes: data }, [data]);
      return;
    }

    if (typeof data === 'string') {
      post({ type: 'text', data });
      return;
    }

    // Blob fallback: binaryType can be overridden by a server-side upgrade
    if (typeof Blob !== 'undefined' && data instanceof Blob) {
      data
        .arrayBuffer()
        .then((buffer) => post({ type: 'frame', bytes: buffer }, [buffer]))
        .catch((error) => post({ type: 'error', message: String(error) }));
    }
  };

  ws.onerror = () => {
    post({ type: 'error', message: 'websocket error' });
  };

  ws.onclose = (event) => {
    stopProbeLoop();
    stats.closes += 1;
    post({ type: 'close', code: event.code, reason: event.reason || '', stats });
    socket = null;
    if (!closedByUs) scheduleReconnect();
  };

  socket = ws;
}

function scheduleReconnect() {
  if (closedByUs || !targetUrl) return;
  const delay = backoffDelay(attempt);
  attempt += 1;
  clearTimeout(reconnectTimer);
  post({ type: 'status', state: 'reconnecting', attempt, delay, url: targetUrl });
  reconnectTimer = setTimeout(dial, delay);
}

function teardownSocket() {
  clearTimeout(reconnectTimer);
  stopProbeLoop();
  if (socket) {
    const dying = socket;
    socket = null;
    dying.onopen = null;
    dying.onmessage = null;
    dying.onerror = null;
    dying.onclose = null;
    try {
      dying.close(1000, 'reconnect');
    } catch {
      /* already closing */
    }
  }
}

function flushOutbound() {
  if (!socket || socket.readyState !== 1) return;
  while (pendingOutbound.length) {
    const bytes = pendingOutbound.shift();
    try {
      socket.send(bytes);
      stats.bytesOut += bytes.byteLength;
    } catch (error) {
      post({ type: 'error', message: String(error) });
      pendingOutbound.unshift(bytes);
      break;
    }
  }
}

function startProbeLoop() {
  stopProbeLoop();
  if (probeIntervalMs <= 0) return;
  probePollTimer = setInterval(() => {
    post({ type: 'probe-due' });
  }, probeIntervalMs);
}

function stopProbeLoop() {
  clearInterval(probePollTimer);
  probePollTimer = 0;
  probePending = false;
}

self.addEventListener('message', (event) => {
  const message = event.data || {};

  switch (message.type) {
    case 'connect': {
      targetUrl = message.url;
      attempt = 0;
      closedByUs = false;
      if (message.policy) policy = { ...policy, ...message.policy };
      if (typeof message.probeMs === 'number') probeIntervalMs = message.probeMs;
      dial();
      break;
    }
    case 'send': {
      const bytes = message.bytes;
      if (!bytes) break;
      if (socket && socket.readyState === 1) {
        try {
          socket.send(bytes);
          stats.bytesOut += bytes.byteLength;
        } catch (error) {
          pendingOutbound.push(bytes);
          post({ type: 'error', message: String(error) });
        }
      } else {
        if (pendingOutbound.length > 256) pendingOutbound.shift();
        pendingOutbound.push(bytes);
      }
      break;
    }
    case 'probe': {
      if (!socket || socket.readyState !== 1) {
        post({ type: 'latency', ms: null, reason: 'not-connected' });
        break;
      }
      probePending = true;
      probeStartedAt = performance.now();
      try {
        socket.send(message.bytes);
        stats.bytesOut += message.bytes.byteLength;
      } catch (error) {
        probePending = false;
        post({ type: 'error', message: String(error) });
      }
      break;
    }
    case 'config': {
      if (message.policy) policy = { ...policy, ...message.policy };
      if (typeof message.probeMs === 'number') {
        probeIntervalMs = message.probeMs;
        if (socket && socket.readyState === 1) startProbeLoop();
      }
      break;
    }
    case 'close': {
      closedByUs = true;
      clearTimeout(reconnectTimer);
      stopProbeLoop();
      if (socket) {
        try {
          socket.close(message.code || 1000, message.reason || 'client disconnect');
        } catch {
          /* ignore */
        }
      }
      socket = null;
      post({ type: 'status', state: 'closed', stats });
      break;
    }
    case 'stats': {
      post({ type: 'stats', stats, connected: Boolean(socket && socket.readyState === 1) });
      break;
    }
    default:
      break;
  }
});

// Announce readiness so the main thread never races the first connect message.
post({ type: 'ready' });
