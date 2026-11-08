// Terrain shader for EaglerXHorizon.
// Vertex: world position, normal, uv, biome tint.
// Fragment: cheap directional lighting, a 2px checker derived from uv so greedy
// merged quads still read as blocks, and distance fog to the clear colour.

struct Scene {
  view_proj: mat4x4<f32>,
  eye: vec3<f32>,
  _pad: f32,
  fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> scene: Scene;

struct VertexIn {
  @location(0) position: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) tint: vec3<f32>,
};

struct VertexOut {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) normal: vec3<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) tint: vec3<f32>,
  @location(3) world_position: vec3<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
  var out: VertexOut;
  out.clip_position = scene.view_proj * vec4<f32>(input.position, 1.0);
  out.normal = input.normal;
  out.uv = input.uv;
  out.tint = input.tint;
  out.world_position = input.position;
  return out;
}

const SUN_DIRECTION: vec3<f32> = normalize(vec3<f32>(0.42, 0.86, 0.28));

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
  let diffuse = max(dot(normalize(input.normal), SUN_DIRECTION), 0.0);
  let ambient = 0.34;

  // block grid: alternate slightly every 2 blocks of uv so merged runs stay legible
  let cell = floor(input.uv * 0.5);
  let checker = select(1.0, 0.94, (cell.x + cell.y) % 2.0 < 0.5);

  var colour = input.tint * (ambient + diffuse * 0.78) * checker;

  // distance fog toward the clear colour
  let distance = length(input.world_position - scene.eye);
  let fog_amount = clamp((distance - 120.0) / 320.0, 0.0, 1.0);
  colour = mix(colour, scene.fog.rgb, fog_amount);

  return vec4<f32>(colour, 1.0);
}
