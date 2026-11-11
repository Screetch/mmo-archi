//! A texture that a test draws to, and its pixels.

use std::sync::OnceLock;

use wgpu::{
  CommandEncoderDescriptor, Extent3d, MapMode, Origin3d, PollType, TexelCopyBufferInfo,
  TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect, TextureDescriptor,
  TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};

use crate::gpu::Gpu;

/// The one device of the tests. A device for each test, all alive at once, crashed the tests on
/// Windows with an access violation, so the tests share one.
pub(crate) fn gpu() -> &'static Gpu {
  static GPU: OnceLock<Gpu> = OnceLock::new();
  GPU.get_or_init(|| Gpu::headless().expect("this machine has an adapter, or a software one"))
}

pub(crate) const FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

pub(crate) struct Capture {
  texture: Texture,
  pub(crate) view: TextureView,
  pub(crate) size: [u32; 2],
}

impl Capture {
  pub(crate) fn new(gpu: &Gpu, size: [u32; 2]) -> Self {
    let texture = gpu.device.create_texture(&TextureDescriptor {
      label: None,
      size: Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
      mip_level_count: 1,
      sample_count: 1,
      dimension: TextureDimension::D2,
      format: FORMAT,
      usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
      view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    Self { texture, view, size }
  }

  /// The pixels, row by row, as bytes of sRGB.
  pub(crate) fn pixels(&self, gpu: &Gpu) -> Vec<[u8; 4]> {
    let [width, height] = self.size.map(|side| usize::try_from(side).expect("a size"));
    let row = (width * 4).next_multiple_of(256);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
      label: None,
      size: (row * height) as u64,
      usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
      mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
      TexelCopyTextureInfo {
        texture: &self.texture,
        mip_level: 0,
        origin: Origin3d::ZERO,
        aspect: TextureAspect::All,
      },
      TexelCopyBufferInfo {
        buffer: &buffer,
        layout: TexelCopyBufferLayout {
          offset: 0,
          bytes_per_row: Some(u32::try_from(row).expect("a row")),
          rows_per_image: Some(self.size[1]),
        },
      },
      Extent3d { width: self.size[0], height: self.size[1], depth_or_array_layers: 1 },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(MapMode::Read, |read| read.expect("the buffer maps"));
    gpu.device.poll(PollType::wait_indefinitely()).expect("the device finishes");
    let data = buffer.slice(..).get_mapped_range().expect("the buffer is mapped");
    data
      .chunks(row)
      .take(height)
      .flat_map(|line| {
        line[..width * 4].chunks(4).map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
      })
      .collect()
  }
}
