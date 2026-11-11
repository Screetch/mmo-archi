//! The drawing of the ground and of the characters, to a texture.

use core::f32::consts::FRAC_PI_2;

use bytemuck::{Pod, Zeroable, bytes_of, cast_slice};
use glam::{Mat4, Quat, Vec3};
use mmo_collide::Solid;
use wgpu::util::{BufferInitDescriptor, DeviceExt, TextureDataOrder};
use wgpu::{
  AddressMode, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
  BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, Buffer,
  BufferBindingType, BufferDescriptor, BufferUsages, Color, CommandEncoderDescriptor,
  CompareFunction, DepthBiasState, DepthStencilState, Extent3d, Face, FilterMode, FragmentState,
  IndexFormat, LoadOp, Operations, PipelineLayoutDescriptor, PrimitiveState,
  RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor,
  RenderPipeline, RenderPipelineDescriptor, SamplerBindingType, SamplerDescriptor,
  ShaderModuleDescriptor, ShaderSource, ShaderStages, StencilState, StoreOp, TextureDescriptor,
  TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureView,
  TextureViewDescriptor, TextureViewDimension, VertexAttribute, VertexBufferLayout, VertexFormat,
  VertexState, VertexStepMode,
};
use wgpu::{MultisampleState, PipelineCompilationOptions};

use crate::gpu::Gpu;
use crate::model::{Image, Mesh, Model, Vertex};
use crate::shape;

/// The count of characters that one frame draws. A frame that holds more draws the first ones.
pub const MAX_CHARACTERS: usize = 256;

const DEPTH: TextureFormat = TextureFormat::Depth32Float;

/// The colour that the target takes where nothing is drawn, in linear light.
const SKY: Color = Color { r: 0.35, g: 0.55, b: 0.85, a: 1.0 };

/// The light of the day: one sun, and the light that the sky and the ground send everywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
  /// The direction toward the sun.
  pub direction: Vec3,
  /// The light of the sun, in linear light.
  pub sun: Vec3,
  /// The light that comes from above, in linear light.
  pub sky: Vec3,
  /// The light that comes from below, in linear light.
  pub ground: Vec3,
}

impl Light {
  /// A sun that stands high over the shoulder of the player.
  pub const DAY: Self = Self {
    direction: Vec3::new(0.4, 0.8, 0.3),
    sun: Vec3::new(3.0, 2.85, 2.6),
    sky: Vec3::new(0.35, 0.45, 0.6),
    ground: Vec3::new(0.12, 0.14, 0.1),
  };
}

/// A character to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
  /// The place of its feet, in metres.
  pub position: Vec3,
  /// The direction in which it looks, in radians, as `mmo-collide` gives a heading: 0 looks along
  /// the axis x, and a quarter turn looks along the axis z.
  pub yaw: f32,
  /// The time of its clip Idle, in seconds.
  pub seconds: f32,
}

/// What one frame shows.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
  /// The matrix that takes a point of the world to the clip space of wgpu.
  pub view_projection: Mat4,
  /// The place of the eye, in metres.
  pub eye: Vec3,
  /// The light of the frame.
  pub light: Light,
  /// The characters.
  pub characters: &'a [Placed],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
  view_projection: [f32; 16],
  eye: [f32; 4],
  sun: [f32; 4],
  sun_color: [f32; 4],
  sky: [f32; 4],
  ground: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Surface {
  metallic: f32,
  roughness: f32,
  joints: u32,
  padding: u32,
}

/// A mesh on the device, with its material.
struct Part {
  vertices: Buffer,
  indices: Buffer,
  count: u32,
  material: BindGroup,
}

/// The depth of the target of one size.
struct Depth {
  size: [u32; 2],
  view: TextureView,
}

/// Draws the ground and the characters of a frame.
pub struct Renderer {
  model: Model,
  pipeline: RenderPipeline,
  globals: Buffer,
  joints: Buffer,
  frame_group: BindGroup,
  ground: Part,
  character: Part,
  depth: Option<Depth>,
}

#[expect(clippy::cast_possible_truncation, reason = "a mesh holds fewer than 2^32 points")]
fn count(len: usize) -> u32 {
  len as u32
}

/// The matrix that takes a model, which looks along the axis z, to its place and its direction.
fn world(placed: &Placed) -> Mat4 {
  let turn = Quat::from_rotation_y(FRAC_PI_2 - placed.yaw);
  Mat4::from_rotation_translation(turn, placed.position)
}

fn texture(gpu: &Gpu, image: &Image, format: TextureFormat) -> TextureView {
  let size = Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
  let texture = gpu.device.create_texture_with_data(
    &gpu.queue,
    &TextureDescriptor {
      label: None,
      size,
      mip_level_count: 1,
      sample_count: 1,
      dimension: TextureDimension::D2,
      format,
      usage: TextureUsages::TEXTURE_BINDING,
      view_formats: &[],
    },
    TextureDataOrder::LayerMajor,
    &image.rgba,
  );
  texture.create_view(&TextureViewDescriptor::default())
}

fn entry(binding: u32, visibility: ShaderStages, ty: BindingType) -> BindGroupLayoutEntry {
  BindGroupLayoutEntry { binding, visibility, ty, count: None }
}

fn buffer_binding(kind: BufferBindingType) -> BindingType {
  BindingType::Buffer { ty: kind, has_dynamic_offset: false, min_binding_size: None }
}

fn map_binding() -> BindingType {
  BindingType::Texture {
    sample_type: TextureSampleType::Float { filterable: true },
    view_dimension: TextureViewDimension::D2,
    multisampled: false,
  }
}

fn part(gpu: &Gpu, layout: &BindGroupLayout, mesh: &Mesh, joints: usize) -> Part {
  let init = |contents: &[u8], usage| {
    gpu.device.create_buffer_init(&BufferInitDescriptor { label: None, contents, usage })
  };
  let sampler = gpu.device.create_sampler(&SamplerDescriptor {
    address_mode_u: AddressMode::Repeat,
    address_mode_v: AddressMode::Repeat,
    mag_filter: FilterMode::Nearest,
    min_filter: FilterMode::Linear,
    ..Default::default()
  });
  let surface = Surface {
    metallic: mesh.material.metallic,
    roughness: mesh.material.roughness,
    joints: count(joints),
    padding: 0,
  };
  let uniform = init(bytes_of(&surface), BufferUsages::UNIFORM);
  let colour = texture(gpu, &mesh.material.base_color, TextureFormat::Rgba8UnormSrgb);
  let normal = texture(gpu, &mesh.material.normal, TextureFormat::Rgba8Unorm);
  let material = gpu.device.create_bind_group(&BindGroupDescriptor {
    label: None,
    layout,
    entries: &[
      BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&colour) },
      BindGroupEntry { binding: 1, resource: BindingResource::TextureView(&normal) },
      BindGroupEntry { binding: 2, resource: BindingResource::Sampler(&sampler) },
      BindGroupEntry { binding: 3, resource: uniform.as_entire_binding() },
    ],
  });
  Part {
    vertices: init(cast_slice(&mesh.vertices), BufferUsages::VERTEX),
    indices: init(cast_slice(&mesh.indices), BufferUsages::INDEX),
    count: count(mesh.indices.len()),
    material,
  }
}

impl Renderer {
  /// Makes what draws to a target of `format`: the pipeline, and the meshes of `model` and of the
  /// solids of the `ground`.
  pub fn new(gpu: &Gpu, format: TextureFormat, model: &Model, ground: &[Solid]) -> Self {
    let device = &gpu.device;
    let both = ShaderStages::VERTEX | ShaderStages::FRAGMENT;
    let frame_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
      label: None,
      entries: &[
        entry(0, both, buffer_binding(BufferBindingType::Uniform)),
        entry(
          1,
          ShaderStages::VERTEX,
          buffer_binding(BufferBindingType::Storage { read_only: true }),
        ),
      ],
    });
    let material_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
      label: None,
      entries: &[
        entry(0, ShaderStages::FRAGMENT, map_binding()),
        entry(1, ShaderStages::FRAGMENT, map_binding()),
        entry(2, ShaderStages::FRAGMENT, BindingType::Sampler(SamplerBindingType::Filtering)),
        entry(3, both, buffer_binding(BufferBindingType::Uniform)),
      ],
    });
    let globals = device.create_buffer(&BufferDescriptor {
      label: None,
      size: size_of::<Globals>() as u64,
      usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
      mapped_at_creation: false,
    });
    let slots = (1 + MAX_CHARACTERS) * model.joints() * size_of::<Mat4>();
    let joints = device.create_buffer(&BufferDescriptor {
      label: None,
      size: slots as u64,
      usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
      mapped_at_creation: false,
    });
    let frame_group = device.create_bind_group(&BindGroupDescriptor {
      label: None,
      layout: &frame_layout,
      entries: &[
        BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
        BindGroupEntry { binding: 1, resource: joints.as_entire_binding() },
      ],
    });
    let pipeline = pipeline(gpu, format, &[&frame_layout, &material_layout]);
    Self {
      model: model.clone(),
      pipeline,
      globals,
      joints,
      frame_group,
      ground: part(gpu, &material_layout, &shape::ground(ground), 1),
      character: part(gpu, &material_layout, &model.mesh, model.joints()),
      depth: None,
    }
  }

  /// Draws `frame` to `target`, whose size is `size` in pixels. It clears the target first.
  pub fn draw(&mut self, gpu: &Gpu, target: &TextureView, size: [u32; 2], frame: &Frame<'_>) {
    self.fit(gpu, size);
    let light = &frame.light;
    let globals = Globals {
      view_projection: frame.view_projection.to_cols_array(),
      eye: frame.eye.extend(1.0).to_array(),
      sun: light.direction.extend(0.0).to_array(),
      sun_color: light.sun.extend(0.0).to_array(),
      sky: light.sky.extend(0.0).to_array(),
      ground: light.ground.extend(0.0).to_array(),
    };
    gpu.queue.write_buffer(&self.globals, 0, bytes_of(&globals));
    let characters = &frame.characters[..frame.characters.len().min(MAX_CHARACTERS)];
    let mut matrices = vec![Mat4::IDENTITY; self.model.joints()];
    for placed in characters {
      let world = world(placed);
      matrices.extend(self.model.pose(placed.seconds).iter().map(|joint| world * *joint));
    }
    gpu.queue.write_buffer(&self.joints, 0, cast_slice(&matrices));
    let mut encoder = gpu.device.create_command_encoder(&CommandEncoderDescriptor::default());
    {
      let depth = self.depth.as_ref().map(|depth| &depth.view);
      let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(RenderPassColorAttachment {
          view: target,
          depth_slice: None,
          resolve_target: None,
          ops: Operations { load: LoadOp::Clear(SKY), store: StoreOp::Store },
        })],
        depth_stencil_attachment: depth.map(|view| RenderPassDepthStencilAttachment {
          view,
          depth_ops: Some(Operations { load: LoadOp::Clear(1.0), store: StoreOp::Store }),
          stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
      });
      pass.set_pipeline(&self.pipeline);
      pass.set_bind_group(0, &self.frame_group, &[]);
      for (part, instances) in
        [(&self.ground, 0..1), (&self.character, 1..count(characters.len()) + 1)]
      {
        pass.set_bind_group(1, &part.material, &[]);
        pass.set_vertex_buffer(0, part.vertices.slice(..));
        pass.set_index_buffer(part.indices.slice(..), IndexFormat::Uint32);
        for instance in instances {
          pass.draw_indexed(0..part.count, 0, instance..instance + 1);
        }
      }
    }
    gpu.queue.submit([encoder.finish()]);
  }

  /// Makes the depth of the size of the target, unless it stands already.
  fn fit(&mut self, gpu: &Gpu, size: [u32; 2]) {
    if self.depth.as_ref().is_some_and(|depth| depth.size == size) {
      return;
    }
    let texture = gpu.device.create_texture(&TextureDescriptor {
      label: None,
      size: Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
      mip_level_count: 1,
      sample_count: 1,
      dimension: TextureDimension::D2,
      format: DEPTH,
      usage: TextureUsages::RENDER_ATTACHMENT,
      view_formats: &[],
    });
    self.depth = Some(Depth { size, view: texture.create_view(&TextureViewDescriptor::default()) });
  }
}

fn pipeline(gpu: &Gpu, format: TextureFormat, layouts: &[&BindGroupLayout]) -> RenderPipeline {
  let module = gpu.device.create_shader_module(ShaderModuleDescriptor {
    label: None,
    source: ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
  });
  let layout = gpu.device.create_pipeline_layout(&PipelineLayoutDescriptor {
    label: None,
    bind_group_layouts: &layouts.iter().map(|layout| Some(*layout)).collect::<Vec<_>>(),
    immediate_size: 0,
  });
  let attributes = [
    (VertexFormat::Float32x3, 0),
    (VertexFormat::Float32x3, 12),
    (VertexFormat::Float32x2, 24),
    (VertexFormat::Float32x4, 32),
    (VertexFormat::Uint16x4, 48),
    (VertexFormat::Float32x4, 56),
  ]
  .into_iter()
  .zip(0..)
  .map(|((format, offset), shader_location)| VertexAttribute { format, offset, shader_location })
  .collect::<Vec<_>>();
  gpu.device.create_render_pipeline(&RenderPipelineDescriptor {
    label: None,
    layout: Some(&layout),
    vertex: VertexState {
      module: &module,
      entry_point: Some("vertex"),
      compilation_options: PipelineCompilationOptions::default(),
      buffers: &[Some(VertexBufferLayout {
        array_stride: size_of::<Vertex>() as u64,
        step_mode: VertexStepMode::Vertex,
        attributes: &attributes,
      })],
    },
    fragment: Some(FragmentState {
      module: &module,
      entry_point: Some("fragment"),
      compilation_options: PipelineCompilationOptions::default(),
      targets: &[Some(format.into())],
    }),
    primitive: PrimitiveState { cull_mode: Some(Face::Back), ..Default::default() },
    depth_stencil: Some(DepthStencilState {
      format: DEPTH,
      depth_write_enabled: Some(true),
      depth_compare: Some(CompareFunction::Less),
      stencil: StencilState::default(),
      bias: DepthBiasState::default(),
    }),
    multisample: MultisampleState::default(),
    multiview_mask: None,
    cache: None,
  })
}

#[cfg(test)]
mod tests {
  use core::f32::consts::{FRAC_PI_2, FRAC_PI_3};

  use glam::camera::rh::{proj::directx, view};
  use glam::{Mat4, Vec3};
  use mmo_collide::Solid;

  use super::{Frame, Light, MAX_CHARACTERS, Placed, Renderer, world};
  use crate::CHARACTER;
  use crate::gpu::Gpu;
  use crate::model::{Image, Model};
  use crate::testing::{Capture, FORMAT, gpu};

  const SIZE: [u32; 2] = [96, 96];
  const EYE: Vec3 = Vec3::new(0.0, 1.2, 4.0);
  const AIM: Vec3 = Vec3::new(0.0, 0.9, 0.0);

  fn floor() -> Vec<Solid> {
    vec![Solid::Cuboid { center: [0.0, -0.5, 0.0], half_extents: [50.0, 0.5, 50.0] }]
  }

  fn view_projection() -> Mat4 {
    directx::perspective(FRAC_PI_3, 1.0, 0.1, 200.0) * view::look_at_mat4(EYE, AIM, Vec3::Y)
  }

  struct Scene {
    gpu: &'static Gpu,
    renderer: Renderer,
  }

  fn scene(model: &Model, solids: &[Solid]) -> Scene {
    let gpu = gpu();
    let renderer = Renderer::new(gpu, FORMAT, model, solids);
    Scene { gpu, renderer }
  }

  fn character() -> Model {
    Model::from_glb(CHARACTER).expect("the character loads")
  }

  impl Scene {
    fn shoot(&mut self, characters: &[Placed], light: Light) -> Vec<[u8; 4]> {
      let capture = Capture::new(self.gpu, SIZE);
      let frame = Frame { view_projection: view_projection(), eye: EYE, light, characters };
      self.renderer.draw(self.gpu, &capture.view, SIZE, &frame);
      capture.pixels(self.gpu)
    }
  }

  fn standing(x: f32) -> Placed {
    Placed { position: Vec3::new(x, 0.0, 0.0), yaw: 0.0, seconds: 0.0 }
  }

  fn luminance(pixel: [u8; 4]) -> f32 {
    f32::from(pixel[0]) + f32::from(pixel[1]) + f32::from(pixel[2])
  }

  /// The places where the two images differ.
  fn differing(one: &[[u8; 4]], other: &[[u8; 4]]) -> Vec<usize> {
    (0..one.len()).filter(|at| one[*at] != other[*at]).collect()
  }

  fn column(at: usize) -> f32 {
    f32::from(u8::try_from(at % 96).expect("a column"))
  }

  /// The most luminance in the places where `shot` differs from `empty`.
  fn brightest(shot: &[[u8; 4]], empty: &[[u8; 4]]) -> f32 {
    differing(shot, empty).iter().map(|at| luminance(shot[*at])).fold(0.0, f32::max)
  }

  #[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a test finds a pixel"
  )]
  fn pixel_of(point: Vec3) -> usize {
    let clip = view_projection() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    let (x, y) = ((ndc.x * 0.5 + 0.5) * 96.0, (0.5 - ndc.y * 0.5) * 96.0);
    y as usize * 96 + x as usize
  }

  #[test]
  fn the_target_shows_the_sky_above_and_the_ground_below_and_the_light_changes_the_ground() {
    let mut scene = scene(&character(), &floor());
    let day = scene.shoot(&[], Light::DAY);
    let sky = day[48];
    assert!(sky[2] > sky[1] && sky[1] > sky[0], "{sky:?}");
    assert!(day[..96].iter().all(|pixel| *pixel == sky));
    let ground = day[95 * 96 + 48];
    assert!(ground[1] > ground[0] && ground[1] > ground[2], "{ground:?}");
    let dark = Light { sun: Vec3::ZERO, ..Light::DAY };
    let unlit = scene.shoot(&[], dark)[95 * 96 + 48];
    assert!(luminance(unlit) * 1.2 < luminance(ground), "{unlit:?} against {ground:?}");
  }

  #[test]
  fn a_character_stands_where_it_is_placed_and_faces_where_it_looks() {
    let mut scene = scene(&character(), &floor());
    let empty = scene.shoot(&[], Light::DAY);
    let middle = scene.shoot(&[standing(0.0)], Light::DAY);
    let right = scene.shoot(&[standing(1.0)], Light::DAY);
    let centre = |shot: &[[u8; 4]]| {
      let drawn = differing(shot, &empty);
      drawn.iter().map(|at| column(*at)).sum::<f32>()
        / f32::from(u16::try_from(drawn.len()).expect("a count"))
    };
    assert!(!differing(&middle, &empty).is_empty());
    assert!((centre(&middle) - 48.0).abs() < 4.0);
    assert!(centre(&right) > centre(&middle) + 5.0);
    let toward = |yaw: f32, direction: Vec3| {
      let at = Placed { position: Vec3::ZERO, yaw, seconds: 0.0 };
      world(&at).transform_vector3(Vec3::Z).distance(direction) < 1e-5
    };
    assert!(toward(0.0, Vec3::X) && toward(FRAC_PI_2, Vec3::Z));
  }

  #[test]
  fn the_clip_moves_the_joints_and_loops() {
    let mut scene = scene(&character(), &floor());
    let at = |seconds: f32| [Placed { position: Vec3::ZERO, yaw: 0.0, seconds }];
    let start = scene.shoot(&at(0.0), Light::DAY);
    assert!(!differing(&start, &scene.shoot(&at(1.0), Light::DAY)).is_empty());
    assert_eq!(start, scene.shoot(&at(2.0), Light::DAY));
  }

  #[test]
  fn the_normal_map_and_the_metal_change_the_light_of_a_surface() {
    let model = character();
    let mut plain = scene(&model, &floor());
    let empty = plain.shoot(&[], Light::DAY);
    let relief = plain.shoot(&[standing(0.0)], Light::DAY);
    let mut flat = model.clone();
    flat.mesh.material.normal = Image { width: 1, height: 1, rgba: vec![128, 128, 255, 255] };
    let smooth = scene(&flat, &floor()).shoot(&[standing(0.0)], Light::DAY);
    assert!(!differing(&relief, &smooth).is_empty());

    let from_the_eye = Light { direction: EYE - Vec3::new(0.0, 1.0, 0.0), ..Light::DAY };
    let matt = plain.shoot(&[standing(0.0)], from_the_eye);
    let mut shiny = model;
    shiny.mesh.material.metallic = 1.0;
    shiny.mesh.material.roughness = 0.2;
    let metal = scene(&shiny, &floor()).shoot(&[standing(0.0)], from_the_eye);
    assert!(brightest(&metal, &empty) > brightest(&matt, &empty));
  }

  #[test]
  fn a_ball_and_a_mesh_of_the_ground_are_drawn_and_a_frame_may_hold_more_characters_than_the_cap() {
    let ball = Solid::Ball { center: [0.0, 1.0, -3.0], radius: 1.0 };
    let triangle = Solid::Mesh {
      vertices: vec![[2.0, 0.5, -2.0], [4.0, 0.5, -2.0], [3.0, 2.5, -2.0]],
      triangles: vec![[0, 1, 2]],
    };
    let mut bare = scene(&character(), &floor());
    let mut full = scene(&character(), &[floor(), vec![ball, triangle]].concat());
    let (empty, shown) = (bare.shoot(&[], Light::DAY), full.shoot(&[], Light::DAY));
    for point in [Vec3::new(0.0, 1.0, -2.0), Vec3::new(3.0, 1.2, -2.0)] {
      assert_ne!(empty[pixel_of(point)], shown[pixel_of(point)], "{point}");
    }
    let crowd = vec![standing(0.0); MAX_CHARACTERS + 3];
    assert_eq!(full.shoot(&crowd, Light::DAY).len(), 96 * 96);
  }

  #[test]
  fn the_depth_follows_the_size_of_the_target() {
    let mut scene = scene(&character(), &floor());
    let size = |scene: &Scene| scene.renderer.depth.as_ref().map(|depth| depth.size);
    assert_eq!(size(&scene), None);
    scene.shoot(&[], Light::DAY);
    assert_eq!(size(&scene), Some(SIZE));
    let small = Capture::new(scene.gpu, [64, 32]);
    let frame =
      Frame { view_projection: view_projection(), eye: EYE, light: Light::DAY, characters: &[] };
    scene.renderer.draw(scene.gpu, &small.view, [64, 32], &frame);
    assert_eq!(size(&scene), Some([64, 32]));
  }
}
