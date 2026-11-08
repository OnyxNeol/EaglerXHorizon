//! Chunk geometry and camera math.
//!
//! Everything here is CPU-side and GPU-agnostic: `glam` for vectors and
//! matrices, `bytemuck` so the vertex buffer can be uploaded without a copy,
//! and a small greedy-quad builder that turns a height field into one mesh
//! per 16x16 chunk.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};

pub const CHUNK_SIZE: i32 = 16;
pub const CHUNK_HEIGHT: f32 = 96.0;

/// Vertex layout consumed by `renderer/pipeline.rs`.
///
/// location 0: position, 1: normal, 2: uv, 3: tint
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tint: [f32; 3],
}

impl Vertex {
    pub fn new(position: [f32; 3], normal: [f32; 3], uv: [f32; 2], tint: [f32; 3]) -> Self {
        Self { position, normal, uv, tint }
    }

    /// Stride in bytes, mirrored in the pipeline's `VertexBufferLayout`.
    pub const STRIDE: u64 = std::mem::size_of::<Vertex>() as u64;
}

/// One chunk's worth of geometry, already in world space but tagged with its
/// origin so the renderer can cull and translate cheaply.
#[derive(Clone, Debug, Default)]
pub struct ChunkMesh {
    pub origin: Vec3,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl ChunkMesh {
    pub fn triangle_count(&self) -> u32 {
        (self.indices.len() / 3) as u32
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for vertex in &self.vertices {
            let p = Vec3::from_array(vertex.position);
            min = min.min(p);
            max = max.max(p);
        }
        if self.vertices.is_empty() {
            (self.origin, self.origin)
        } else {
            (min, max)
        }
    }
}

/// Camera as a world position plus yaw/pitch, which is what the client uses.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 3],
    pub _pad: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y_degrees: f32,
    pub aspect: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            position: Vec3::new(8.0, 34.0, 48.0),
            yaw: -0.55,
            pitch: -0.32,
            fov_y_degrees: 80.0,
            aspect: 16.0 / 9.0,
            z_near: 0.1,
            z_far: 1200.0,
        }
    }
}

impl Camera {
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cy * cp, sp, sy * cp).normalize_or_zero()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize_or_zero()
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_to_rh(self.position, self.forward(), Vec3::Y)
    }

    pub fn projection(&self) -> Mat4 {
        // WebGPU clip space is z in [0, 1], which is glam's plain `perspective_rh`
        // (NOT the `_gl` variant, which maps z to [-1, 1] and breaks depth).
        Mat4::perspective_rh(self.fov_y_degrees.to_radians(), self.aspect.max(0.01), self.z_near, self.z_far)
    }

    pub fn view_projection(&self) -> Mat4 {
        self.projection() * self.view()
    }

    pub fn uniform(&self) -> CameraUniform {
        CameraUniform {
            view_proj: self.view_projection().to_cols_array_2d(),
            eye: self.position.to_array(),
            _pad: 0.0,
        }
    }

    pub fn orbit(&mut self, dt_seconds: f32) {
        let dt = dt_seconds.max(0.0).min(0.1);
        self.yaw += dt * 0.06;
        self.pitch = (self.pitch + dt * 0.018).clamp(-0.9, 0.35);
        self.position.x = 8.0 + (self.yaw * 3.0).sin() * 24.0;
        self.position.z = 8.0 + (self.yaw * 3.0).cos() * 24.0;
        self.position.y = 34.0 + self.pitch * 14.0;
    }
}

/// Six frustum planes extracted from a view-projection matrix, for chunk culling.
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_proj(matrix: Mat4) -> Self {
        let r0 = matrix.row(0);
        let r1 = matrix.row(1);
        let r2 = matrix.row(2);
        let r3 = matrix.row(3);

        let raw = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2];
        let mut planes = [Vec4::ZERO; 6];
        for (index, plane) in raw.iter().enumerate() {
            let length = plane.truncate().length().max(f32::EPSILON);
            planes[index] = *plane / length;
        }
        Self { planes }
    }

    /// Sphere test against the planes; cheap and good enough for 16^2 chunks.
    pub fn intersects_sphere(&self, center: Vec3, radius: f32) -> bool {
        self.planes
            .iter()
            .all(|plane| plane.truncate().dot(center) + plane.w >= -radius)
    }

    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        let center = (min + max) * 0.5;
        let radius = (max - min).length() * 0.5;
        self.intersects_sphere(center, radius)
    }
}

/// Deterministic terrain height: two octaves of hashed value noise, no RNG crate.
pub fn terrain_height(world_x: f32, world_z: f32) -> f32 {
    let a = hash2(world_x * 0.045, world_z * 0.045) * 9.0;
    let b = hash2(world_x * 0.11 + 13.7, world_z * 0.11 - 4.2) * 3.5;
    let basin = ((world_x * 0.012).sin() + (world_z * 0.014).cos()) * 2.5;
    24.0 + a + b + basin
}

fn hash2(x: f32, z: f32) -> f32 {
    let mut h = (x * 374_761.0 + z * 668_265.0) as i64;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    let unit = (h & 0xFFFF) as f32 / 65_535.0;
    unit * 2.0 - 1.0
}

fn biome_tint(world_x: f32, world_z: f32, height: f32) -> [f32; 3] {
    let warmth = ((world_x + world_z) * 0.02).sin() * 0.5 + 0.5;
    let altitude = ((height - 20.0) / 14.0).clamp(0.0, 1.0);
    [
        0.16 + warmth * 0.10 + altitude * 0.22,
        0.31 + altitude * 0.30,
        0.22 + (1.0 - warmth) * 0.16 + altitude * 0.20,
    ]
}

/// Builds one chunk with greedy top quads.
///
/// Columns are scanned along X and merged while height and tint match, so a
/// flat 16-wide row becomes a single quad instead of 16. Side skirts are emitted
/// as one quad per column edge, which is enough for the baseline world.
pub fn build_chunk(chunk_x: i32, chunk_z: i32) -> ChunkMesh {
    let base_x = chunk_x * CHUNK_SIZE;
    let base_z = chunk_z * CHUNK_SIZE;
    let mut mesh = ChunkMesh {
        origin: Vec3::new(base_x as f32, 0.0, base_z as f32),
        vertices: Vec::with_capacity(256),
        indices: Vec::with_capacity(384),
    };

    for local_z in 0..CHUNK_SIZE {
        let mut local_x = 0;
        while local_x < CHUNK_SIZE {
            let world_x = (base_x + local_x) as f32;
            let world_z = (base_z + local_z) as f32;
            let height = terrain_height(world_x, world_z);
            let tint = biome_tint(world_x, world_z, height);

            // extend the run while the neighbour is indistinguishable
            let mut run = 1;
            while local_x + run < CHUNK_SIZE {
                let next_x = (base_x + local_x + run) as f32;
                let next_height = terrain_height(next_x, world_z);
                let next_tint = biome_tint(next_x, world_z, next_height);
                if (next_height - height).abs() > 0.05 || tint_diff(tint, next_tint) > 0.02 {
                    break;
                }
                run += 1;
            }

            push_top_quad(&mut mesh, base_x + local_x, base_z + local_z, run, height, tint);
            push_side_quads(&mut mesh, base_x + local_x, base_z + local_z, run, height, tint);
            local_x += run;
        }
    }

    mesh
}

fn tint_diff(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

fn push_top_quad(mesh: &mut ChunkMesh, x: i32, z: i32, run: i32, height: f32, tint: [f32; 3]) {
    let x0 = x as f32;
    let x1 = (x + run) as f32;
    let z0 = z as f32;
    let z1 = (z + 1) as f32;
    let base = mesh.vertices.len() as u32;
    let normal = [0.0, 1.0, 0.0];

    let corners = [
        ([x0, height, z0], [0.0, 0.0]),
        ([x1, height, z0], [run as f32, 0.0]),
        ([x1, height, z1], [run as f32, 1.0]),
        ([x0, height, z1], [0.0, 1.0]),
    ];
    for (position, uv) in corners {
        mesh.vertices.push(Vertex::new(position, normal, uv, tint));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn push_side_quads(mesh: &mut ChunkMesh, x: i32, z: i32, run: i32, height: f32, tint: [f32; 3]) {
    let drop = 1.6_f32;
    let x0 = x as f32;
    let x1 = (x + run) as f32;
    let z0 = z as f32;
    let z1 = (z + 1) as f32;
    let y_top = height;
    let y_bottom = height - drop;
    let shaded = [tint[0] * 0.62, tint[1] * 0.62, tint[2] * 0.62];

    // north (-Z)
    add_quad(
        mesh,
        ([x0, y_top, z0], [x1, y_top, z0], [x1, y_bottom, z0], [x0, y_bottom, z0]),
        [0.0, 0.0, -1.0],
        shaded,
    );
    // south (+Z)
    add_quad(
        mesh,
        ([x1, y_top, z1], [x0, y_top, z1], [x0, y_bottom, z1], [x1, y_bottom, z1]),
        [0.0, 0.0, 1.0],
        shaded,
    );
    // west (-X)
    add_quad(
        mesh,
        ([x0, y_top, z1], [x0, y_top, z0], [x0, y_bottom, z0], [x0, y_bottom, z1]),
        [-1.0, 0.0, 0.0],
        shaded,
    );
    // east (+X)
    add_quad(
        mesh,
        ([x1, y_top, z0], [x1, y_top, z1], [x1, y_bottom, z1], [x1, y_bottom, z0]),
        [1.0, 0.0, 0.0],
        shaded,
    );
}

fn add_quad(mesh: &mut ChunkMesh, corners: ([f32; 3], [f32; 3], [f32; 3], [f32; 3]), normal: [f32; 3], tint: [f32; 3]) {
    let base = mesh.vertices.len() as u32;
    let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    for (index, corner) in [corners.0, corners.1, corners.2, corners.3].iter().enumerate() {
        mesh.vertices.push(Vertex::new(*corner, normal, uvs[index], tint));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Generates the square block of chunks around the origin used by the demo world.
pub fn build_world(radius_chunks: i32) -> Vec<ChunkMesh> {
    let radius = radius_chunks.clamp(1, 16);
    let mut meshes = Vec::with_capacity(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
    for chunk_z in -radius..=radius {
        for chunk_x in -radius..=radius {
            meshes.push(build_chunk(chunk_x, chunk_z));
        }
    }
    meshes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_stride_is_tightly_packed() {
        assert_eq!(Vertex::STRIDE, 11 * 4);
    }

    #[test]
    fn a_chunk_produces_triangles_and_bounds() {
        let mesh = build_chunk(0, 0);
        assert!(mesh.triangle_count() > 0);
        let (min, max) = mesh.bounds();
        assert!(max.y > min.y);
    }

    #[test]
    fn greedy_merging_beats_one_quad_per_column() {
        let mesh = build_chunk(0, 0);
        // 256 columns x 5 quads would be 1280 quads / 2560 triangles; merging must beat that.
        assert!(mesh.triangle_count() < 2560);
    }

    #[test]
    fn frustum_contains_the_camera_cell() {
        let camera = Camera::default();
        let frustum = Frustum::from_view_proj(camera.view_projection());
        assert!(frustum.intersects_sphere(camera.position + camera.forward() * 20.0, 12.0));
    }
}
