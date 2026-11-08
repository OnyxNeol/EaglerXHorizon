//! `ProxySocketBridge`: the transport between the browser client and an
//! EaglercraftXBungee / EaglerXServer proxy.
//!
//! Two modes, one type:
//!
//! * **Worker-fed** (production): `web/worker.js` owns the `WebSocket`, so a
//!   blocked or slow socket never touches the render thread. Frames arrive as
//!   transferred `ArrayBuffer`s and are copied straight into the pre-allocated
//!   [`ClientPacketRingBuffer`]; outbound frames are queued for the worker to post.
//! * **Direct** (`gloo-net`, fallback): the bridge opens the socket itself. Used
//!   when `Worker` or module workers are unavailable.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gloo_net::websocket::futures::WebSocket;
use js_sys::{Function, Uint8Array};
use thiserror::Error;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use crate::network::buffer::{ClientPacketRingBuffer, RingStats, DEFAULT_CAPACITY_BYTES};
use crate::protocol::build_latency_probe;

#[derive(Debug, Error)]
pub enum SocketError {
    #[error("invalid websocket url: {0}")]
    InvalidUrl(String),
    #[error("websocket transport: {0}")]
    Transport(String),
    #[error("bridge is not connected")]
    NotConnected,
    #[error("worker channel: {0}")]
    Worker(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

impl From<SocketError> for JsValue {
    fn from(error: SocketError) -> Self {
        JsValue::from_str(&error.to_string())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BackoffPolicy {
    pub base_ms: u32,
    pub max_ms: u32,
    pub jitter_fraction: f32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self { base_ms: 400, max_ms: 15_000, jitter_fraction: 0.35 }
    }
}

impl BackoffPolicy {
    /// `base * 2^attempt`, capped, plus jitter so a restarted proxy does not get
    /// every client reconnecting on the same tick.
    pub fn delay_ms(&self, attempt: u32) -> u32 {
        let exponential = self
            .base_ms
            .saturating_mul(1u32.checked_shl(attempt.min(8)).unwrap_or(256))
            .min(self.max_ms);
        let jitter_span = (exponential as f32 * self.jitter_fraction).max(1.0);
        let jitter = (js_sys::Math::random() as f32) * jitter_span;
        (exponential as f32 + jitter) as u32
    }
}

/// Live state the dashboard reads.
#[derive(Clone, Debug, Default)]
pub struct BridgeState {
    pub open: bool,
    pub last_latency_ms: f64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub frames_in: u64,
    pub reconnect_attempts: u32,
    pub close_code: Option<u16>,
    pub close_reason: Option<String>,
    pub pending_probe_at: f64,
    pub worker_fed: bool,
}

/// Result of pushing one worker payload through the bridge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub bytes: usize,
    pub frames: u32,
    pub dropped: bool,
}

pub struct ProxySocketBridge {
    url: Option<String>,
    socket: Option<WebSocket>,
    state: Rc<RefCell<BridgeState>>,
    on_frame: Option<Function>,
    on_latency: Option<Function>,
    policy: BackoffPolicy,
    ring: ClientPacketRingBuffer,
    outbound: VecDeque<Vec<u8>>,
    // closures must outlive the WebSocket handlers
    _handlers: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl ProxySocketBridge {
    /// Worker-fed bridge: allocates the ring buffer up front and never opens a
    /// socket of its own.
    pub fn fed(capacity_bytes: usize, on_latency: Option<Function>, on_frame: Option<Function>) -> Self {
        let mut state = BridgeState::default();
        state.worker_fed = true;
        Self {
            url: None,
            socket: None,
            state: Rc::new(RefCell::new(state)),
            on_frame,
            on_latency,
            policy: BackoffPolicy::default(),
            ring: ClientPacketRingBuffer::preallocate(capacity_bytes),
            outbound: VecDeque::with_capacity(32),
            _handlers: Vec::new(),
        }
    }

    /// Direct bridge (fallback): the ring is still pre-allocated so the packet
    /// path is identical in both modes.
    pub fn new(url: &str, on_latency: Option<Function>, on_frame: Option<Function>) -> Result<Self, JsValue> {
        let trimmed = url.trim();
        if !(trimmed.starts_with("ws://") || trimmed.starts_with("wss://")) {
            return Err(SocketError::InvalidUrl(trimmed.to_string()).into());
        }
        let mut bridge = Self::fed(DEFAULT_CAPACITY_BYTES, on_latency, on_frame);
        bridge.state.borrow_mut().worker_fed = false;
        bridge.url = Some(trimmed.to_string());
        Ok(bridge)
    }

    /// Installs or replaces the JS callbacks (the dashboard attaches them after
    /// constructing the engine, before the worker dials).
    pub fn set_callbacks(&mut self, on_latency: Option<Function>, on_frame: Option<Function>) {
        self.on_latency = on_latency;
        self.on_frame = on_frame;
    }

    pub fn with_policy(mut self, policy: BackoffPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Points the bridge at a proxy URL. In worker-fed mode the worker dials it;
    /// here we only validate and remember it.
    pub fn set_url(&mut self, url: &str) -> Result<(), JsValue> {
        let trimmed = url.trim();
        if !(trimmed.starts_with("ws://") || trimmed.starts_with("wss://")) {
            return Err(SocketError::InvalidUrl(trimmed.to_string()).into());
        }
        self.url = Some(trimmed.to_string());
        Ok(())
    }

    pub fn state(&self) -> BridgeState {
        self.state.borrow().clone()
    }

    pub fn is_open(&self) -> bool {
        self.state.borrow().open
    }

    pub fn latency_ms(&self) -> f64 {
        self.state.borrow().last_latency_ms
    }

    pub fn is_worker_fed(&self) -> bool {
        self.state.borrow().worker_fed
    }

    pub fn ring_stats(&self) -> RingStats {
        self.ring.stats()
    }

    /// Called by the dashboard when `worker.js` reports `{type:'open'}`.
    pub fn mark_open(&mut self) {
        let mut state = self.state.borrow_mut();
        state.open = true;
        state.reconnect_attempts = 0;
        state.close_code = None;
        state.close_reason = None;
    }

    /// Called by the dashboard when `worker.js` reports a close.
    pub fn mark_closed(&mut self, code: Option<u16>, reason: Option<String>) {
        let mut state = self.state.borrow_mut();
        state.open = false;
        state.close_code = code;
        state.close_reason = reason;
    }

    /// Pushes one worker payload through the ring buffer and decodes whatever
    /// complete frames it completed.
    ///
    /// This is the hot path: one bounds-checked copy into pre-allocated memory,
    /// then a zero-copy frame view. No `Vec` growth, no per-message allocation
    /// unless a frame straddles the wrap point.
    pub fn ingest(&mut self, bytes: &[u8]) -> IngestReport {
        let mut report = IngestReport { bytes: bytes.len(), ..Default::default() };
        {
            let mut state = self.state.borrow_mut();
            state.bytes_in += bytes.len() as u64;
            state.frames_in += 1;
        }

        if self.ring.write(bytes).is_err() {
            // Consumer fell behind: drop the payload rather than grow the heap.
            report.dropped = true;
            return report;
        }

        loop {
            match self.ring.consume_frame() {
                Ok(Some((packet_id, payload))) => {
                    report.frames += 1;
                    if let Some(callback) = self.on_frame.as_ref() {
                        let frame_bytes = Uint8Array::new_with_length(payload.len() as u32);
                        frame_bytes.copy_from(&payload);
                        let id = JsValue::from_f64(packet_id as f64);
                        let _ = callback.call2(&JsValue::NULL, &id, &frame_bytes.buffer());
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    log::warn!("frame decode failed: {error}");
                    // A corrupt length prefix would wedge the ring forever.
                    self.ring.reset();
                    report.dropped = true;
                    break;
                }
            }
        }
        report
    }

    /// Queues an outbound frame for `worker.js` to send.
    pub fn queue_outbound(&mut self, payload: &[u8]) {
        if self.outbound.len() >= 512 {
            // The worker is not draining; drop the oldest rather than balloon.
            self.outbound.pop_front();
        }
        self.outbound.push_back(payload.to_vec());
        self.state.borrow_mut().bytes_out += payload.len() as u64;
    }

    /// Hands queued frames to the caller, which posts them to the worker.
    pub fn drain_outbound(&mut self) -> Vec<Vec<u8>> {
        self.outbound.drain(..).collect()
    }

    pub fn queued_outbound(&self) -> usize {
        self.outbound.len()
    }

    /// Starts the latency probe. In worker-fed mode this returns the frame for the
    /// caller to post; in direct mode it sends it immediately.
    pub fn probe_latency(&mut self) -> Vec<u8> {
        let probe = build_latency_probe(now_ms());
        self.state.borrow_mut().pending_probe_at = now_ms();
        if self.state.borrow().worker_fed {
            probe
        } else {
            if let Err(error) = self.send(&probe) {
                log::warn!("probe failed: {error:?}");
            }
            probe
        }
    }

    /// Completes a probe measurement and fires `on_latency`.
    pub fn record_latency(&mut self, millis: f64) {
        {
            let mut state = self.state.borrow_mut();
            state.last_latency_ms = millis;
            state.pending_probe_at = 0.0;
        }
        if let Some(callback) = self.on_latency.as_ref() {
            let _ = callback.call1(&JsValue::NULL, &JsValue::from_f64(millis));
        }
    }

    /// Measures from the last probe departure, if one is in flight.
    pub fn complete_probe(&mut self) {
        let started = self.state.borrow().pending_probe_at;
        if started > 0.0 {
            let elapsed = now_ms() - started;
            self.record_latency(elapsed);
        }
    }

    /// Opens the socket directly (fallback mode only).
    pub async fn connect(&mut self) -> Result<(), JsValue> {
        if self.is_worker_fed() {
            return Err(SocketError::Worker(
                "worker-fed bridge: post {type:'connect'} to web/worker.js instead".to_string(),
            )
            .into());
        }
        let url = self.url.clone().ok_or(SocketError::NotConnected)?;
        let socket = WebSocket::open(&url).map_err(|error| SocketError::Transport(format!("{error:?}")))?;

        let state_open = Rc::clone(&self.state);
        let on_open = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            let mut state = state_open.borrow_mut();
            state.open = true;
            state.reconnect_attempts = 0;
            state.close_code = None;
            state.close_reason = None;
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let state_message = Rc::clone(&self.state);
        let on_message = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let bytes = extract_bytes(&event);
            if bytes.is_empty() {
                return;
            }
            let mut state = state_message.borrow_mut();
            state.bytes_in += bytes.len() as u64;
            state.frames_in += 1;
            state.last_latency_ms = if state.pending_probe_at > 0.0 {
                let elapsed = now_ms() - state.pending_probe_at;
                state.pending_probe_at = 0.0;
                elapsed
            } else {
                state.last_latency_ms
            };
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let state_error = Rc::clone(&self.state);
        let on_error = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            state_error.borrow_mut().open = false;
        });
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let state_close = Rc::clone(&self.state);
        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let mut state = state_close.borrow_mut();
            state.open = false;
            state.close_code = js_sys::Reflect::get(&event, &JsValue::from_str("code"))
                .ok()
                .and_then(|value| value.as_f64())
                .map(|value| value as u16);
            state.close_reason = js_sys::Reflect::get(&event, &JsValue::from_str("reason"))
                .ok()
                .and_then(|value| value.as_string());
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        self._handlers = vec![on_open, on_message, on_error, on_close];
        self.socket = Some(socket);
        self.state.borrow_mut().open = true;
        Ok(())
    }

    /// Reconnects with `base * 2^n` backoff plus jitter (fallback mode).
    pub async fn connect_with_retry(&mut self, max_attempts: u32) -> Result<(), JsValue> {
        let mut attempt = 0u32;
        loop {
            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    attempt += 1;
                    self.state.borrow_mut().reconnect_attempts = attempt;
                    if attempt >= max_attempts.max(1) {
                        return Err(error);
                    }
                    let delay = self.policy.delay_ms(attempt);
                    log::warn!("proxy reconnect {attempt}/{max_attempts} in {delay}ms");
                    gloo_timers::future::TimeoutFuture::new(delay).await;
                }
            }
        }
    }

    /// Sends one binary frame directly (fallback mode).
    pub fn send(&mut self, payload: &[u8]) -> Result<(), JsValue> {
        let socket = self.socket.as_mut().ok_or(SocketError::NotConnected)?;
        if !self.state.borrow().open {
            return Err(SocketError::NotConnected.into());
        }
        socket
            .send_bytes(payload)
            .map_err(|error| SocketError::Transport(format!("{error:?}")))?;
        self.state.borrow_mut().bytes_out += payload.len() as u64;
        Ok(())
    }

    pub fn close(&mut self, code: u16, reason: &str) {
        if let Some(socket) = self.socket.as_mut() {
            socket.close(Some(code), reason);
        }
        self.outbound.clear();
        self.ring.reset();
        let mut state = self.state.borrow_mut();
        state.open = false;
        state.close_code = Some(code);
        state.close_reason = Some(reason.to_string());
        self.socket = None;
    }
}

/// Message payloads arrive as `ArrayBuffer` (gloo-net sets binaryType) but a
/// Blob can show up if something re-opened the socket manually.
pub fn extract_bytes(event: &JsValue) -> Vec<u8> {
    if let Ok(buffer) = event.clone().dyn_into::<js_sys::ArrayBuffer>() {
        return Uint8Array::new(&buffer).to_vec();
    }
    if let Ok(view) = event.clone().dyn_into::<Uint8Array>() {
        return view.to_vec();
    }
    Vec::new()
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or_else(js_sys::Date::now)
}
