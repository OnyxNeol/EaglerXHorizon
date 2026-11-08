//! WebGPU renderer.
//!
//! `Renderer::new` builds the whole browser pipeline once: instance -> surface ->
//! adapter -> device -> configured surface -> terrain pipeline -> camera uniform.
//! After that the frame loop is just `write_buffer` + `render`.

mod pipeline;

pub use pipeline::{Backend, Renderer, RendererError};
