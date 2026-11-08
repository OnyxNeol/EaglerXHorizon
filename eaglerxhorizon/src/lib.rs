//! EaglerXHorizon engine entry point.
//!
//! One `wasm-bindgen` type, [`Engine`], owns the whole browser client:
//! the WebGPU renderer, the camera, the chunk mesh uploader and the
//! [`network::ProxySocketBridge`] that talks to an EaglercraftXBungee proxy.

mod engine;
pub mod math;
pub mod network;
pub mod protocol;
pub mod renderer;

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::engine::{EngineConfig, EngineCore, EngineState};
use crate::renderer::{Renderer, RendererError};

/// Shown whenever the browser has no WebGPU adapter. The flag name matters:
/// users copy it verbatim.
pub const WEBGPU_FLAG_HINT: &str = "WebGPU is unavailable. Open chrome://flags/#enable-unsafe-webgpu, set \
\"Unsafe WebGPU\" to Enabled, restart the browser, then reload this page. On Linux you may also need \
--enable-features=Vulkan,UseSkiaRenderer.";

/// Shown when we drop back to the WebGL2 path.
pub const WEBGL2_FALLBACK_NOTICE: &str =
    "WebGPU was not available, so EaglerXHorizon is running on the WebGL2 fallback: reduced render \
distance and no compute shaders. Enable chrome://flags/#enable-unsafe-webgpu for full performance.";

/// Installed automatically when the module is imported.
#[wasm_bindgen(start)]
pub fn main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::default().module_prefix("eaglerxhorizon"));
    Ok(())
}

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[wasm_bindgen]
pub fn webgpu_flag_hint() -> String {
    WEBGPU_FLAG_HINT.to_string()
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = warn)]
    fn console_warn(message: &str);
}

/// The single entry point the dashboard instantiates.
///
/// ```js
/// import init, { Engine } from '../pkg/eaglerxhorizon.js';
/// await init();
/// const engine = new Engine('canvas-target');
/// await engine.start();
/// ```
#[wasm_bindgen]
pub struct Engine {
    core: Rc<RefCell<EngineCore>>,
    renderer: Rc<RefCell<Option<Renderer>>>,
    running: Rc<RefCell<bool>>,
}

#[wasm_bindgen]
impl Engine {
    /// Binds to `canvas_id` and prepares the CPU-side state. No GPU work yet:
    /// `start()` is async because adapter and device requests are async.
    #[wasm_bindgen(constructor)]
    pub fn new(canvas_id: &str) -> Result<Engine, JsValue> {
        let canvas = web_sys::window()
            .ok_or_else(|| JsValue::from_str("no window: is this running in a browser?"))?
            .document()
            .ok_or_else(|| JsValue::from_str("no document"))?
            .get_element_by_id(canvas_id)
            .ok_or_else(|| JsValue::from_str(&format!("canvas #{canvas_id} not found")))?;

        let canvas: web_sys::HtmlCanvasElement = canvas
            .dyn_into()
            .map_err(|_| JsValue::from_str(&format!("#{canvas_id} is not a <canvas> element")))?;

        let config = EngineConfig::from_canvas(&canvas);
        Ok(Engine {
            core: Rc::new(RefCell::new(EngineCore::new(config, canvas))),
            renderer: Rc::new(RefCell::new(None)),
            running: Rc::new(RefCell::new(false)),
        })
    }

    /// Requests a WebGPU adapter + device, builds the pipeline and starts the frame loop.
    pub async fn start(&mut self) -> Result<(), JsValue> {
        {
            let mut core = self.core.borrow_mut();
            core.set_state(EngineState::Initializing);
        }

        let canvas = self.core.borrow().canvas().clone();
        let config = self.core.borrow().config().clone();

        let renderer = match Renderer::new(canvas, &config).await {
            Ok(renderer) => renderer,
            Err(error) => {
                let mut core = self.core.borrow_mut();
                core.set_state(EngineState::Failed(error.to_string()));
                core.report_issue(issue_for(&error));
                return Err(JsValue::from_str(&format!("{} ({})", issue_for(&error), error)));
            }
        };

        self.core.borrow_mut().set_backend(renderer.backend());
        self.core.borrow_mut().set_state(EngineState::Ready);
        *self.renderer.borrow_mut() = Some(renderer);

        let core = Rc::clone(&self.core);
        let renderer_slot = Rc::clone(&self.renderer);
        let running = Rc::clone(&self.running);
        *running.borrow_mut() = true;

        spawn_local(async move {
            // Seed the world once: a flat test world plus a hill ring so the
            // camera has something with real depth to look at.
            let meshes = core.borrow_mut().build_demo_world();
            if let Some(renderer) = renderer_slot.borrow_mut().as_mut() {
                for mesh in &meshes {
                    if let Err(error) = renderer.upload_chunk(mesh) {
                        log::error!("chunk upload failed: {error}");
                    }
                }
                renderer.dirty_all();
            }

            core.borrow_mut().set_state(EngineState::Running);
            loop {
                if !*running.borrow() {
                    core.borrow_mut().set_state(EngineState::Paused);
                    break;
                }
                {
                    let mut core_ref = core.borrow_mut();
                    core_ref.tick();
                    let camera = core_ref.camera_uniform();
                    if let Some(renderer) = renderer_slot.borrow_mut().as_mut() {
                        if let Err(error) = renderer.render(&camera) {
                            core_ref.set_state(EngineState::Failed(error.to_string()));
                            log::error!("frame failed: {error}");
                            break;
                        }
                    }
                    if let Some(fps) = core_ref.take_fps_sample() {
                        if let Some(callback) = core_ref.fps_callback() {
                            let _ = callback.call1(&JsValue::NULL, &JsValue::from_f64(fps as f64));
                        }
                    }
                }
                gloo_timers::future::TimeoutFuture::new(16).await;
            }
        });

        Ok(())
    }

    /// Stops the loop, keeps the device alive so `start()` can resume.
    pub fn stop(&mut self) {
        *self.running.borrow_mut() = false;
    }

    /// Resizes the drawing surface. Called from a ResizeObserver in the dashboard.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        self.core.borrow_mut().set_viewport(width, height);
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            if let Err(error) = renderer.resize(width, height) {
                log::warn!("resize rejected: {error}");
            }
        }
    }

    /// Render distance in chunks, clamped to 16-64 by the dashboard and here.
    pub fn set_render_distance(&mut self, chunks: u32) {
        self.core.borrow_mut().set_render_distance(chunks);
    }

    /// Toggling this off asks the renderer for the WebGL2 path on next start.
    pub fn set_webgpu_enabled(&mut self, enabled: bool) {
        self.core.borrow_mut().set_webgpu_enabled(enabled);
    }

    /// `{ state, backend, fps, renderDistance, drawCalls, triangles }`
    pub fn stats(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.core.borrow().stats()).unwrap_or(JsValue::NULL)
    }

    /// Registers `(fps: number) => void` so the top bar can show live frames.
    pub fn on_fps(&mut self, callback: js_sys::Function) {
        self.core.borrow_mut().set_fps_callback(callback);
    }

    /// `(level: string, message: string) => void`
    pub fn on_issue(&mut self, callback: js_sys::Function) {
        self.core.borrow_mut().set_issue_callback(callback);
    }

    /// Connects the ProxySocketBridge. `on_latency(ms)` and `on_frame(Uint8Array)` are optional.
    pub async fn connect_proxy(
        &mut self,
        url: &str,
        on_latency: Option<js_sys::Function>,
        on_frame: Option<js_sys::Function>,
    ) -> Result<(), JsValue> {
        self.core
            .borrow_mut()
            .connect_proxy(url, on_latency, on_frame)
            .await
    }

    pub fn disconnect_proxy(&mut self) {
        self.core.borrow_mut().disconnect_proxy();
    }
    /// Fallback when `Worker` is unavailable: dial from the main thread.
    pub async fn connect_direct(
        &mut self,
        url: &str,
        on_latency: Option<js_sys::Function>,
        on_frame: Option<js_sys::Function>,
    ) -> Result<(), JsValue> {
        let mut bridge = crate::network::ProxySocketBridge::new(url, on_latency, on_frame)?;
        bridge.connect().await?;
        self.core.borrow_mut().replace_proxy(bridge);
        Ok(())
    }

    /// Attaches the JS callbacks the worker bridge needs. Call this before
    /// posting `{type:'connect'}` to `web/worker.js`.
    pub fn attach_worker(&mut self, on_latency: Option<js_sys::Function>, on_frame: Option<js_sys::Function>) {
        self.core.borrow_mut().set_proxy_callbacks(on_latency, on_frame);
    }

    /// Feeds one transferred `ArrayBuffer` from the worker into the pre-allocated
    /// ring buffer. Returns the number of complete packets decoded.
    pub fn ingest_packet(&mut self, data: &[u8]) -> u32 {
        self.core.borrow_mut().ingest_packet(data)
    }

    /// Worker said the socket opened.
    pub fn mark_proxy_open(&mut self) {
        self.core.borrow_mut().mark_proxy_open();
    }

    /// Worker said the socket closed (code 1006 for an abnormal drop).
    pub fn mark_proxy_closed(&mut self, code: Option<u16>, reason: Option<String>) {
        self.core.borrow_mut().mark_proxy_closed(code, reason);
    }

    /// Next latency probe frame for the worker to send (empty when unsupported).
    pub fn next_probe_frame(&mut self) -> Vec<u8> {
        self.core.borrow_mut().next_probe_frame()
    }

    /// Worker froze a round trip, in milliseconds.
    pub fn record_proxy_latency(&mut self, millis: f64) {
        self.core.borrow_mut().record_proxy_latency(millis);
    }

    /// Frames queued for the worker to send, oldest first.
    pub fn drain_outbound(&mut self) -> js_sys::Array {
        let frames = js_sys::Array::new();
        for payload in self.core.borrow_mut().drain_outbound() {
            let view = js_sys::Uint8Array::new_with_length(payload.len() as u32);
            view.copy_from(&payload);
            frames.push(&view.buffer());
        }
        frames
    }

    /// Ring buffer counters: capacity, peak usage, overflow drops, wraps.
    pub fn ring_stats(&self) -> JsValue {
        match self.core.borrow().ring_stats() {
            Some(stats) => serde_wasm_bindgen::to_value(&stats).unwrap_or(JsValue::NULL),
            None => JsValue::NULL,
        }
    }


    pub fn proxy_latency(&self) -> f64 {
        self.core.borrow().proxy_latency()
    }

    pub fn is_connected(&self) -> bool {
        self.core.borrow().proxy_connected()
    }
}

fn issue_for(error: &RendererError) -> String {
    match error {
        RendererError::NoWebGpu => WEBGPU_FLAG_HINT.to_string(),
        RendererError::NoAdapter => WEBGPU_FLAG_HINT.to_string(),
        RendererError::Device(message) => format!("{WEBGPU_FLAG_HINT} (device request failed: {message})"),
        RendererError::Surface(message) => format!("Could not create a drawing surface: {message}"),
        RendererError::NoCanvas => "The canvas element was released before the engine started.".to_string(),
    }
}

#[allow(dead_code)]
fn warn_once(message: &str) {
    console_warn(message);
}
