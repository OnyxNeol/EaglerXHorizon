//! Engine state, frame timing and the bridge between JS and the renderer.

use js_sys::Function;
use serde::Serialize;
use wasm_bindgen::JsValue;

use crate::math::{build_world, Camera, CameraUniform, ChunkMesh};
use crate::network::{ProxySocketBridge, DEFAULT_CAPACITY_BYTES};
use crate::renderer::Backend;

pub const MIN_RENDER_DISTANCE: u32 = 16;
pub const MAX_RENDER_DISTANCE: u32 = 64;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    viewport_width: u32,
    viewport_height: u32,
    render_distance: u32,
    webgpu_enabled: bool,
}

impl EngineConfig {
    pub fn from_canvas(canvas: &web_sys::HtmlCanvasElement) -> Self {
        Self {
            viewport_width: canvas.width().max(1),
            viewport_height: canvas.height().max(1),
            render_distance: 24,
            webgpu_enabled: true,
        }
    }

    pub fn webgpu_enabled(&self) -> bool {
        self.webgpu_enabled
    }

    pub fn render_distance(&self) -> u32 {
        self.render_distance
    }

    pub fn viewport(&self) -> (u32, u32) {
        (self.viewport_width, self.viewport_height)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineState {
    Initializing,
    Ready,
    Running,
    Paused,
    Failed(String),
}

impl EngineState {
    pub fn label(&self) -> &'static str {
        match self {
            EngineState::Initializing => "initializing",
            EngineState::Ready => "ready",
            EngineState::Running => "running",
            EngineState::Paused => "paused",
            EngineState::Failed(_) => "failed",
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Stats {
    pub state: String,
    pub detail: Option<String>,
    pub backend: String,
    pub fps: f32,
    pub render_distance: u32,
    pub width: u32,
    pub height: u32,
    pub proxy_connected: bool,
    pub proxy_latency_ms: f64,
    pub ring_capacity: usize,
    pub ring_peak_used: usize,
    pub ring_overflow_drops: u64,
    pub frames: u64,
}

pub struct EngineCore {
    config: EngineConfig,
    canvas: web_sys::HtmlCanvasElement,
    state: EngineState,
    backend: Option<Backend>,
    camera: Camera,
    fps_callback: Option<Function>,
    issue_callback: Option<Function>,
    proxy: Option<ProxySocketBridge>,
    frames_in_window: u32,
    fps: f32,
    last_frame_ms: f64,
    window_started_ms: f64,
    frames_total: u64,
}

impl EngineCore {
    pub fn new(config: EngineConfig, canvas: web_sys::HtmlCanvasElement) -> Self {
        let (width, height) = config.viewport();
        let mut camera = Camera::default();
        camera.aspect = width as f32 / height.max(1) as f32;

        Self {
            config,
            canvas,
            state: EngineState::Initializing,
            backend: None,
            camera,
            fps_callback: None,
            issue_callback: None,
            // Allocate the packet ring once, at boot: 24 MiB reserved so the
            // receive path never reallocates during chunk loading.
            proxy: Some(ProxySocketBridge::fed(DEFAULT_CAPACITY_BYTES, None, None)),
            frames_in_window: 0,
            fps: 0.0,
            last_frame_ms: now_ms(),
            window_started_ms: now_ms(),
            frames_total: 0,
        }
    }

    pub fn canvas(&self) -> &web_sys::HtmlCanvasElement {
        &self.canvas
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn state(&self) -> &EngineState {
        &self.state
    }

    pub fn set_state(&mut self, state: EngineState) {
        self.state = state;
    }

    pub fn set_backend(&mut self, backend: Backend) {
        self.backend = Some(backend);
    }

    pub fn set_fps_callback(&mut self, callback: Function) {
        self.fps_callback = Some(callback);
    }

    pub fn fps_callback(&self) -> Option<&Function> {
        self.fps_callback.as_ref()
    }

    pub fn set_issue_callback(&mut self, callback: Function) {
        self.issue_callback = Some(callback);
    }

    /// Surfaces a human-readable problem to the dashboard banner.
    pub fn report_issue(&self, message: &str) {
        match &self.issue_callback {
            Some(callback) => {
                let _ = callback.call2(&JsValue::NULL, &JsValue::from_str("warn"), &JsValue::from_str(message));
            }
            None => log::warn!("{message}"),
        }
    }

    pub fn set_viewport(&mut self, width: u32, height: u32) {
        self.config.viewport_width = width.max(1);
        self.config.viewport_height = height.max(1);
        self.camera.aspect = self.config.viewport_width as f32 / self.config.viewport_height as f32;
    }

    pub fn set_render_distance(&mut self, chunks: u32) {
        self.config.render_distance = chunks.clamp(MIN_RENDER_DISTANCE, MAX_RENDER_DISTANCE);
    }

    pub fn set_webgpu_enabled(&mut self, enabled: bool) {
        self.config.webgpu_enabled = enabled;
    }

    /// Builds the demo world: `render_distance` is a real budget, so we cap the
    /// radius at what the current setting allows instead of always building all.
    pub fn build_demo_world(&mut self) -> Vec<ChunkMesh> {
        let radius = ((self.config.render_distance / 8).max(2).min(6)) as i32;
        build_world(radius)
    }

    /// Advances the clock and the camera. Called once per frame.
    pub fn tick(&mut self) {
        let now = now_ms();
        let dt = ((now - self.last_frame_ms) / 1000.0) as f32;
        self.last_frame_ms = now;
        self.camera.orbit(dt);
        self.frames_in_window += 1;
        self.frames_total += 1;
    }

    pub fn camera_uniform(&self) -> CameraUniform {
        self.camera.uniform()
    }

    /// Returns a fresh FPS reading at most twice a second.
    pub fn take_fps_sample(&mut self) -> Option<f32> {
        let elapsed = now_ms() - self.window_started_ms;
        if elapsed < 500.0 {
            return None;
        }
        self.fps = (self.frames_in_window as f64 * 1000.0 / elapsed) as f32;
        self.frames_in_window = 0;
        self.window_started_ms = now_ms();
        Some(self.fps)
    }

    pub fn stats(&self) -> Stats {
        let (width, height) = self.config.viewport();
        Stats {
            state: self.state.label().to_string(),
            detail: match &self.state {
                EngineState::Failed(reason) => Some(reason.clone()),
                _ => None,
            },
            backend: self.backend.map(|b| b.as_str().to_string()).unwrap_or_else(|| "pending".to_string()),
            fps: self.fps,
            render_distance: self.config.render_distance,
            width,
            height,
            proxy_connected: self.proxy_connected(),
            proxy_latency_ms: self.proxy_latency(),
            ring_capacity: self.proxy.as_ref().map(|bridge| bridge.ring_stats().capacity).unwrap_or(0),
            ring_peak_used: self.proxy.as_ref().map(|bridge| bridge.ring_stats().peak_len).unwrap_or(0),
            ring_overflow_drops: self.proxy.as_ref().map(|bridge| bridge.ring_stats().overflow_drops).unwrap_or(0),
            frames: self.frames_total,
        }
    }

    /// Points the bridge at `url` and, in direct mode, dials it.
    ///
    /// In worker mode (`web/worker.js` present) the socket belongs to the worker:
    /// we only record the URL, and the dashboard posts `{type:'connect'}`.
    pub async fn connect_proxy(
        &mut self,
        url: &str,
        on_latency: Option<Function>,
        on_frame: Option<Function>,
    ) -> Result<(), JsValue> {
        let mut bridge = match self.proxy.take() {
            Some(bridge) => bridge,
            None => ProxySocketBridge::fed(DEFAULT_CAPACITY_BYTES, None, None),
        };
        bridge.set_callbacks(on_latency, on_frame);
        bridge.set_url(url)?;

        if bridge.is_worker_fed() {
            self.proxy = Some(bridge);
            return Ok(());
        }
        bridge.connect().await?;
        self.proxy = Some(bridge);
        Ok(())
    }

    /// Fallback transport: dial the proxy straight from this thread when the
    /// browser has no `Worker`. Same ring buffer, same decoder, no isolation.
    pub async fn connect_direct(
        &mut self,
        url: &str,
        on_latency: Option<Function>,
        on_frame: Option<Function>,
    ) -> Result<(), JsValue> {
        // build and dial outside the borrow: `connect` awaits
        let mut bridge = ProxySocketBridge::new(url, on_latency, on_frame)?;
        bridge.connect().await?;
        self.proxy = Some(bridge);
        Ok(())
    }

    pub fn replace_proxy(&mut self, bridge: ProxySocketBridge) {
        self.proxy = Some(bridge);
    }

    /// Copies one worker payload into the ring buffer and decodes its frames.
    /// Returns how many complete packets were delivered.
    pub fn ingest_packet(&mut self, bytes: &[u8]) -> u32 {
        match self.proxy.as_mut() {
            Some(bridge) => bridge.ingest(bytes).frames,
            None => 0,
        }
    }

    pub fn set_proxy_callbacks(&mut self, on_latency: Option<Function>, on_frame: Option<Function>) {
        if let Some(bridge) = self.proxy.as_mut() {
            bridge.set_callbacks(on_latency, on_frame);
        }
    }

    pub fn mark_proxy_open(&mut self) {
        if let Some(bridge) = self.proxy.as_mut() {
            bridge.mark_open();
        }
    }

    pub fn mark_proxy_closed(&mut self, code: Option<u16>, reason: Option<String>) {
        if let Some(bridge) = self.proxy.as_mut() {
            bridge.mark_closed(code, reason);
        }
    }

    /// The next latency probe frame for the worker to post. Empty when the bridge
    /// is absent.
    pub fn next_probe_frame(&mut self) -> Vec<u8> {
        match self.proxy.as_mut() {
            Some(bridge) => bridge.probe_latency(),
            None => Vec::new(),
        }
    }

    pub fn record_proxy_latency(&mut self, millis: f64) {
        if let Some(bridge) = self.proxy.as_mut() {
            bridge.record_latency(millis);
        }
    }

    /// Outbound frames the worker should send, in order.
    pub fn drain_outbound(&mut self) -> Vec<Vec<u8>> {
        match self.proxy.as_mut() {
            Some(bridge) => bridge.drain_outbound(),
            None => Vec::new(),
        }
    }

    pub fn ring_stats(&self) -> Option<crate::network::RingStats> {
        self.proxy.as_ref().map(|bridge| bridge.ring_stats())
    }

    pub fn disconnect_proxy(&mut self) {
        if let Some(mut bridge) = self.proxy.take() {
            bridge.close(1000, "client disconnect");
        }
    }

    pub fn proxy_latency(&self) -> f64 {
        self.proxy.as_ref().map(|bridge| bridge.latency_ms()).unwrap_or(-1.0)
    }

    pub fn proxy_connected(&self) -> bool {
        self.proxy.as_ref().map(|bridge| bridge.is_open()).unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn camera(&self) -> &Camera {
        &self.camera
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or_else(js_sys::Date::now)
}
