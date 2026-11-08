// Node harness for web/worker.js — proves the message protocol and the
// zero-copy transfer without a browser.
//
//   node tests/worker_protocol_test.mjs
//
// It runs worker.js inside a VM context with a fake `self` and a fake WebSocket.
// When the worker calls postMessage(msg, transfer), the harness performs a real
// structuredClone with that transfer list, so the ArrayBuffer is genuinely
// detached: `byteLength === 0` afterwards is the proof the main thread received
// the bytes by move, not by copy.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import vm from 'node:vm';

const here = path.dirname(fileURLToPath(import.meta.url));
const workerSource = readFileSync(path.join(here, '..', 'web', 'worker.js'), 'utf8');

const clone = (value, transfer) => (transfer && transfer.length ? structuredClone(value, { transfer }) : structuredClone(value));

function createHarness() {
  const received = [];
  const listeners = [];
  const timers = [];
  const sockets = [];

  class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url) {
      this.url = url;
      this.readyState = 0;
      this.binaryType = 'blob';
      this.sent = [];
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this.onclose = null;
      sockets.push(this);
    }

    send(bytes) {
      if (this.readyState !== 1) throw new Error('socket not open');
      this.sent.push(bytes);
    }

    close(code = 1000, reason = '') {
      this.readyState = 3;
      if (this.onclose) this.onclose({ code, reason });
    }

    // --- test helpers -------------------------------------------------------
    open() {
      this.readyState = 1;
      if (this.onopen) this.onopen({});
    }

    deliver(buffer) {
      if (this.onmessage) this.onmessage({ data: buffer });
    }

    drop(code = 1006) {
      this.readyState = 3;
      if (this.onclose) this.onclose({ code, reason: 'abnormal' });
    }
  }

  const self = {
    addEventListener(type, handler) {
      if (type === 'message') listeners.push(handler);
    },
    postMessage(message, transfer) {
      received.push(clone(message, transfer));
    },
  };

  const sandbox = {
    self,
    WebSocket: FakeWebSocket,
    // same-realm intrinsics: worker.js uses `data instanceof ArrayBuffer`
    ArrayBuffer,
    Uint8Array,
    performance: { now: () => Date.now() },
    Blob,
    console,
    setTimeout: (fn, delay) => {
      timers.push({ fn, delay, kind: 'timeout' });
      return timers.length;
    },
    clearTimeout: () => {},
    setInterval: (fn, delay) => {
      timers.push({ fn, delay, kind: 'interval' });
      return timers.length;
    },
    clearInterval: () => {},
  };

  vm.createContext(sandbox);
  vm.runInContext(workerSource, sandbox, { filename: 'worker.js' });

  const send = (message) => listeners.forEach((handler) => handler({ data: message }));
  const last = () => received[received.length - 1];
  const ofType = (type) => received.filter((message) => message.type === type);

  return { received, timers, sockets, send, last, ofType, FakeWebSocket };
}

const results = [];
const check = (name, fn) => {
  try {
    fn();
    results.push({ name, ok: true });
  } catch (error) {
    results.push({ name, ok: false, error: error.message });
  }
};

/* ---------------------------------------------------------------- test cases */

{
  const harness = createHarness();
  check('announces ready on load', () => {
    assert.equal(harness.last().type, 'ready');
  });
}

{
  const harness = createHarness();
  check('connect dials the requested url and reports open', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    const socket = harness.sockets[0];
    assert.equal(socket.url, 'wss://proxy.example/ws');
    assert.equal(socket.binaryType, 'arraybuffer');
    socket.open();
    const open = harness.ofType('open')[0];
    assert.equal(open.url, 'wss://proxy.example/ws');
  });
}

{
  const harness = createHarness();
  check('inbound ArrayBuffer is transferred, leaving the sender detached', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    const socket = harness.sockets[0];
    socket.open();

    const payload = new Uint8Array([0x03, 0x00, 0x01, 0x02]).buffer;
    socket.deliver(payload);

    const frame = harness.ofType('frame')[0];
    assert.ok(frame, 'a frame message must be posted');
    assert.equal(new Uint8Array(frame.bytes).byteLength, 4);
    assert.equal(new Uint8Array(frame.bytes)[2], 0x01);
    assert.equal(payload.byteLength, 0, 'transfer must detach the worker-side buffer');
  });
}

{
  const harness = createHarness();
  check('probe times the next inbound frame and reports latency', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    const socket = harness.sockets[0];
    socket.open();

    harness.send({ type: 'probe', bytes: new Uint8Array([0xfe, 0x00]).buffer });
    assert.equal(socket.sent.length, 1, 'probe frame goes on the wire');

    socket.deliver(new Uint8Array([0xfe]).buffer);
    const latency = harness.ofType('latency')[0];
    assert.ok(latency, 'latency message expected');
    assert.equal(typeof latency.ms, 'number');
    assert.ok(latency.ms >= 0);
  });
}

{
  const harness = createHarness();
  check('send before open is queued and flushed on open', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    const socket = harness.sockets[0];
    const outbound = new Uint8Array([0x01, 0x02]);
    harness.send({ type: 'send', bytes: outbound.buffer });
    assert.equal(socket.sent.length, 0, 'nothing goes out before the socket opens');
    socket.open();
    assert.equal(socket.sent.length, 1, 'queued frame flushed on open');
  });
}

{
  const harness = createHarness();
  check('abnormal close schedules a jittered backoff retry', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws', policy: { baseMs: 400, maxMs: 5000, jitter: 0.35 } });
    harness.sockets[0].open();
    harness.sockets[0].drop(1006);

    const close = harness.ofType('close')[0];
    assert.equal(close.code, 1006);

    const status = harness.ofType('status').find((message) => message.state === 'reconnecting');
    assert.ok(status, 'reconnecting status expected');
    assert.equal(status.attempt, 1);
    assert.ok(status.delay >= 400, `delay ${status.delay} must be at least the base`);
    assert.ok(status.delay <= 540, `delay ${status.delay} must stay inside base + jitter`);

    const timer = harness.timers.find((entry) => entry.kind === 'timeout' && entry.delay === status.delay);
    assert.ok(timer, 'a reconnect timer must be armed');
  });
}

{
  const harness = createHarness();
  check('explicit close does not reconnect', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    harness.sockets[0].open();
    harness.send({ type: 'close', code: 1000, reason: 'client disconnect' });
    const statuses = harness.ofType('status').map((message) => message.state);
    assert.ok(statuses.includes('closed'));
    assert.ok(!statuses.includes('reconnecting'), 'a deliberate close must not retry');
  });
}

{
  const harness = createHarness();
  check('frame counters track traffic without leaks', () => {
    harness.send({ type: 'connect', url: 'wss://proxy.example/ws' });
    const socket = harness.sockets[0];
    socket.open();
    for (let index = 0; index < 5; index += 1) socket.deliver(new Uint8Array([index]).buffer);
    harness.send({ type: 'stats' });
    const stats = harness.ofType('stats')[0];
    assert.equal(stats.stats.framesIn, 5);
    assert.equal(stats.connected, true);
  });
}

/* ------------------------------------------------------------------- report */

const failed = results.filter((result) => !result.ok);
for (const result of results) {
  console.log(`${result.ok ? 'PASS' : 'FAIL'}  ${result.name}${result.ok ? '' : `\n        ${result.error}`}`);
}
console.log(`\n${results.length - failed.length}/${results.length} worker protocol checks passed`);
if (failed.length) process.exit(1);
