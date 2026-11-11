// A skinned or still mesh, lit by one sun and by the light of the sky and of the ground. A surface
// reflects as glTF says: the diffuse light of Lambert for what is no metal, and the specular light
// of Cook and Torrance (the spread of GGX, the shadow of Smith and the colour of Schlick) for all.

const PI: f32 = 3.14159265;

struct Globals {
  view_projection: mat4x4<f32>,
  eye: vec4<f32>,
  sun: vec4<f32>,
  sun_color: vec4<f32>,
  sky: vec4<f32>,
  ground: vec4<f32>,
};

struct Surface {
  metallic: f32,
  roughness: f32,
  joints: u32,
  padding: u32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;
@group(1) @binding(0) var colour_map: texture_2d<f32>;
@group(1) @binding(1) var normal_map: texture_2d<f32>;
@group(1) @binding(2) var maps: sampler;
@group(1) @binding(3) var<uniform> surface: Surface;

struct In {
  @builtin(instance_index) instance: u32,
  @location(0) position: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) tangent: vec4<f32>,
  @location(4) joints: vec4<u32>,
  @location(5) weights: vec4<f32>,
};

struct Out {
  @builtin(position) clip: vec4<f32>,
  @location(0) place: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) tangent: vec4<f32>,
};

@vertex
fn vertex(v: In) -> Out {
  // The matrices of an instance start at its index times the count of joints of its mesh.
  let first = v.instance * surface.joints;
  let skin = joints[first + v.joints.x] * v.weights.x + joints[first + v.joints.y] * v.weights.y
    + joints[first + v.joints.z] * v.weights.z + joints[first + v.joints.w] * v.weights.w;
  let place = skin * vec4<f32>(v.position, 1.0);
  var out: Out;
  out.clip = globals.view_projection * place;
  out.place = place.xyz;
  out.normal = (skin * vec4<f32>(v.normal, 0.0)).xyz;
  out.uv = v.uv;
  out.tangent = vec4<f32>((skin * vec4<f32>(v.tangent.xyz, 0.0)).xyz, v.tangent.w);
  return out;
}

@fragment
fn fragment(in: Out) -> @location(0) vec4<f32> {
  let base = textureSample(colour_map, maps, in.uv).rgb;
  let n = normalize(in.normal);
  let t = normalize(in.tangent.xyz - n * dot(n, in.tangent.xyz));
  let b = cross(n, t) * in.tangent.w;
  let relief = textureSample(normal_map, maps, in.uv).xyz * 2.0 - 1.0;
  let normal = normalize(relief.x * t + relief.y * b + relief.z * n);

  let l = normalize(globals.sun.xyz);
  let v = normalize(globals.eye.xyz - in.place);
  let h = normalize(l + v);
  let ndl = max(dot(normal, l), 0.0);
  let ndv = max(dot(normal, v), 1e-4);
  let ndh = max(dot(normal, h), 0.0);
  let vdh = max(dot(v, h), 0.0);

  let rough = clamp(surface.roughness, 0.045, 1.0);
  let a2 = pow(rough, 4.0);
  let spread = a2 / (PI * pow(ndh * ndh * (a2 - 1.0) + 1.0, 2.0));
  let k = (rough + 1.0) * (rough + 1.0) / 8.0;
  let shadow = (ndl / (ndl * (1.0 - k) + k)) * (ndv / (ndv * (1.0 - k) + k));
  // A surface that is no metal reflects 4 percent of the light head-on, and a metal its own colour.
  let f0 = mix(vec3<f32>(0.04), base, surface.metallic);
  let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);

  let diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - surface.metallic) * base / PI;
  let specular = spread * shadow * fresnel / (4.0 * ndl * ndv + 1e-4);
  let direct = (diffuse + specular) * globals.sun_color.rgb * ndl;
  let around = mix(globals.ground.rgb, globals.sky.rgb, normal.y * 0.5 + 0.5);
  let ambient = base * (1.0 - surface.metallic) * around;
  return vec4<f32>(direct + ambient, 1.0);
}
