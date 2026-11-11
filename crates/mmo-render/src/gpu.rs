//! The graphics device that draws.

use wgpu::{
  Adapter, Device, DeviceDescriptor, Instance, InstanceDescriptor, PowerPreference, Queue,
  RequestAdapterError, RequestAdapterOptions, Surface,
};

/// The reason why no device is made.
#[derive(Debug, thiserror::Error)]
#[error("no graphics adapter can draw: {0}")]
pub struct GpuError(#[from] RequestAdapterError);

/// A graphics adapter with its device and its queue.
pub struct Gpu {
  /// The adapter that the device came from.
  pub adapter: Adapter,
  /// The device that makes the resources.
  pub device: Device,
  /// The queue that gives the work to the device.
  pub queue: Queue,
}

impl Gpu {
  /// Takes the best adapter of `instance`, and one that can draw to `surface` when it is given.
  /// The call waits for the adapter, so a task of an async runtime must not make it.
  #[expect(
    clippy::expect_used,
    reason = "a device that asks for the limits of its own adapter and no feature is always made"
  )]
  pub fn new(instance: &Instance, surface: Option<&Surface<'_>>) -> Result<Self, GpuError> {
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
      power_preference: PowerPreference::HighPerformance,
      compatible_surface: surface,
      force_fallback_adapter: false,
      apply_limit_buckets: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
      label: Some("mmo-render"),
      required_limits: adapter.limits(),
      ..Default::default()
    }))
    .expect("the device of an adapter that asks for its own limits is made");
    Ok(Self { adapter, device, queue })
  }

  /// A device with no window, which draws to a texture. The variables `WGPU_BACKEND` and
  /// `WGPU_ADAPTER_NAME` choose the adapter, as they do for each program of wgpu.
  pub fn headless() -> Result<Self, GpuError> {
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle_from_env());
    Self::new(&instance, None)
  }
}

#[cfg(test)]
mod tests {
  use wgpu::{Backends, Instance, InstanceDescriptor};

  use super::Gpu;

  #[test]
  fn an_instance_with_no_backend_gives_no_device_and_the_default_gives_one() {
    let none = Instance::new(InstanceDescriptor {
      backends: Backends::empty(),
      ..InstanceDescriptor::new_without_display_handle()
    });
    let refused = Gpu::new(&none, None).err().expect("no backend, no adapter").to_string();
    assert!(refused.starts_with("no graphics adapter can draw: "), "{refused}");
    assert!(!crate::testing::gpu().adapter.get_info().name.is_empty());
  }
}
