//! The wgpu render pipeline, its buffers and its per-frame submission.

use bytemuck::{Pod, Zeroable};
use thiserror::Error;
use wgpu::util::DeviceExt;

use crate::engine::EngineConfig;
use crate::math::{CameraUniform, ChunkMesh, Vertex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    WebGpu,
    WebGl2,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::WebGpu => "webgpu",
            Backend::WebGl2 => "webgl2",
        }
    }
}

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("the browser does not expose navigator.gpu, so WebGPU cannot be requested")]
    NoWebGpu,
    #[error("no compatible GPU adapter was returned")]
    NoAdapter,
    #[error("request_device failed: {0}")]
    Device(String),
    #[error("surface error: {0}")]
    Surface(String),
    #[error("the canvas element is gone")]
    NoCanvas,
}

#[derive(Debug, Default)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniforms {
    camera: CameraUniform,
    // fog colour matches the shader's sky so distant chunks dissolve into it
    fog: [f32; 4],
}

struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    min: [f32; 3],
    max: [f32; 3],
}

pub struct Renderer {
    backend: Backend,
    surface: wgpu::Surface,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    depth_view: wgpu::TextureView,
    scene_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    meshes: Vec<GpuMesh>,
    culling_dirty: bool,
    triangles: u32,
}

impl Renderer {
    /// Boots wgpu against `canvas`.
    ///
    /// The instance is created with `Backends::BROWSER_WEBGPU`, which is the
    /// browser WebGPU backend. When the dashboard turns the hardware toggle off
    /// we ask for `Backends::GL` instead, the reduced-fidelity WebGL2 path.
    pub async fn new(canvas: web_sys::HtmlCanvasElement, engine: &EngineConfig) -> Result<Self, RendererError> {
        let backends = if engine.webgpu_enabled() {
            wgpu::Backends::BROWSER_WEBGPU
        } else {
            wgpu::Backends::GL
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let surface = instance
            .create_surface(canvas.clone())
            .map_err(|error| RendererError::Surface(error.to_string()))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(if engine.webgpu_enabled() {
                RendererError::NoAdapter
            } else {
                RendererError::NoWebGpu
            })?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("eaglerxhorizon-device"),
                    features: wgpu::Features::empty(),
                    limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                },
                None,
            )
            .await
            .map_err(|error| RendererError::Device(error.to_string()))?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or_else(|| capabilities.formats[0]);

        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-bind-group-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let scene_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene-uniforms"),
            size: std::mem::size_of::<SceneUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: Vertex::STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 12,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 24,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 32,
                            shader_location: 3,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let depth_view = create_depth_view(&device, &config);

        Ok(Renderer {
            backend: if engine.webgpu_enabled() { Backend::WebGpu } else { Backend::WebGl2 },
            surface,
            device,
            queue,
            config,
            pipeline,
            depth_view,
            scene_buffer,
            bind_group,
            meshes: Vec::new(),
            culling_dirty: true,
            triangles: 0,
        })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn triangle_count(&self) -> u32 {
        self.triangles
    }

    pub fn chunk_count(&self) -> usize {
        self.meshes.len()
    }

    /// Marks every cached culling decision stale, e.g. after a world rebuild.
    pub fn dirty_all(&mut self) {
        self.culling_dirty = true;
    }

    /// Uploads one chunk's vertices and indices to the GPU.
    pub fn upload_chunk(&mut self, mesh: &ChunkMesh) -> Result<(), RendererError> {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return Ok(());
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("chunk-vertices"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("chunk-indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let (min, max) = mesh.bounds();
        self.meshes.push(GpuMesh {
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            min: min.to_array(),
            max: max.to_array(),
        });
        self.culling_dirty = true;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RendererError> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth_view(&self.device, &self.config);
        Ok(())
    }

    /// Records one frame and presents it.
    pub fn render(&mut self, camera: &CameraUniform) -> Result<(), RendererError> {
        let uniforms = SceneUniforms {
            camera: *camera,
            fog: [0.035, 0.055, 0.094, 0.0],
        };
        self.queue.write_buffer(&self.scene_buffer, 0, bytemuck::bytes_of(&uniforms));

        let frame = self
            .surface
            .get_current_texture()
            .map_err(|error| RendererError::Surface(error.to_string()))?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        let mut triangles = 0u32;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terrain-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.035,
                            g: 0.055,
                            b: 0.094,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);

            // Baseline culling: every chunk is drawn, but the bounds are already
            // available so a real client can swap this for the frustum test in math.
            for mesh in &self.meshes {
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                triangles += mesh.index_count / 3;
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.triangles = triangles;
        self.culling_dirty = false;
        Ok(())
    }

    pub fn culling_dirty(&self) -> bool {
        self.culling_dirty
    }

    #[allow(dead_code)]
    fn bounds_of(&self) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for mesh in &self.meshes {
            for axis in 0..3 {
                min[axis] = min[axis].min(mesh.min[axis]);
                max[axis] = max[axis].max(mesh.max[axis]);
            }
        }
        (min, max)
    }
}

fn create_depth_view(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
