//! The window of the game: winit opens it, and wgpu draws to it.
//!
//! This file holds all the code that needs a window. No test opens a window, so `cargo coverage`
//! leaves this file out (`.cargo/config.toml`), and the gates run the binary under a virtual
//! display with `--demo --frames`, so each run on Linux opens a window and draws frames. All
//! the rest of the client stands in files that tests prove, and this file only moves events to
//! [`Game`] and a [`Scene`] to the renderer.

#![expect(
  clippy::cast_possible_truncation,
  clippy::cast_precision_loss,
  reason = "pixels and pointer moves are small numbers, and the screen shows no more digits"
)]

use std::sync::Arc;
use std::time::Instant;

use mmo_render::{CHARACTER, Frame, Gpu, GpuError, Light, Model, Renderer};
use wgpu::{
  CreateSurfaceError, CurrentSurfaceTexture, Instance, InstanceDescriptor, PresentMode, Surface,
  SurfaceConfiguration, TextureFormat, TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::error::{EventLoopError, OsError};
use winit::event::{
  DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::launch::prepare;
use crate::{Direction, Failure, Game};

/// The pixels of a scroll of a touchpad that make a notch of the wheel of a mouse.
const PIXELS_A_NOTCH: f64 = 40.0;

/// Why the window or the drawing fails.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
  /// The loop of events fails.
  #[error("the loop of events fails: {0}")]
  EventLoop(#[from] EventLoopError),
  /// The system does not open the window.
  #[error("the window does not open: {0}")]
  Window(#[from] OsError),
  /// The window gives no surface to draw on.
  #[error("the window gives no surface: {0}")]
  Surface(#[from] CreateSurfaceError),
  /// The machine has no graphics adapter that draws.
  #[error(transparent)]
  Gpu(#[from] GpuError),
  /// The model of the character is not valid.
  #[error(transparent)]
  Model(#[from] mmo_render::Error),
  /// The adapter cannot draw to the surface.
  #[error("the surface does not fit the adapter")]
  Unfit,
}

/// What stands once the window is open.
struct Running {
  window: Arc<Window>,
  surface: Surface<'static>,
  config: SurfaceConfiguration,
  gpu: Gpu,
  renderer: Renderer,
}

struct App {
  game: Game,
  /// The frames still to draw before the window closes, when the command line fixed a count.
  frames: Option<u32>,
  /// True while the right button of the mouse is down: the pointer turns the camera.
  orbiting: bool,
  last: Instant,
  running: Option<Running>,
  failure: Option<WindowError>,
}

/// Runs the game client: reads `args` and the environment that `read` gives, makes the game, opens
/// the window and runs it until the player quits, the link ends or the count of frames ends. The
/// options are `--demo`, which needs no server, and `--frames <count>`. The variables `MMO_PLAY_*`
/// of [`connect`](crate::launch) name the account and the servers.
///
/// It must run on the main thread, as the window of macOS needs.
pub fn play(args: &[String], read: &dyn Fn(&str) -> Option<String>) -> Result<(), Failure> {
  let launch = prepare(args, read)?;
  let mut app = App {
    game: launch.game,
    frames: launch.frames,
    orbiting: false,
    last: Instant::now(),
    running: None,
    failure: None,
  };
  let event_loop = EventLoop::new().map_err(WindowError::from)?;
  event_loop.set_control_flow(ControlFlow::Poll);
  event_loop.run_app(&mut app).map_err(WindowError::from)?;
  drop(launch.keep);
  app.failure.map_or(Ok(()), |failure| Err(failure.into()))
}

impl App {
  fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), WindowError> {
    let attributes = Window::default_attributes()
      .with_title("mmo-archi")
      .with_inner_size(LogicalSize::new(1280.0, 720.0));
    let window = Arc::new(event_loop.create_window(attributes)?);
    let instance =
      Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(Arc::clone(&window))));
    let surface = instance.create_surface(Arc::clone(&window))?;
    let gpu = Gpu::new(&instance, Some(&surface))?;
    let size = window.inner_size();
    let mut config = surface
      .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
      .ok_or(WindowError::Unfit)?;
    let formats = surface.get_capabilities(&gpu.adapter).formats;
    config.format = formats
      .iter()
      .copied()
      .find(TextureFormat::is_srgb)
      .or_else(|| formats.first().copied())
      .ok_or(WindowError::Unfit)?;
    config.present_mode = PresentMode::AutoVsync;
    surface.configure(&gpu.device, &config);
    let model = Model::from_glb(CHARACTER)?;
    let renderer = Renderer::new(&gpu, config.format, &model, &mmo_collide::ground());
    self.running = Some(Running { window, surface, config, gpu, renderer });
    self.last = Instant::now();
    Ok(())
  }

  fn frame(&mut self, event_loop: &ActiveEventLoop) {
    let Some(running) = &mut self.running else {
      return;
    };
    let now = Instant::now();
    self.game.update((now - self.last).as_secs_f32());
    self.last = now;
    if self.game.over() {
      event_loop.exit();
      return;
    }
    let texture = match running.surface.get_current_texture() {
      CurrentSurfaceTexture::Success(texture) | CurrentSurfaceTexture::Suboptimal(texture) => {
        texture
      }
      CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
        running.surface.configure(&running.gpu.device, &running.config);
        return;
      }
      _ => return,
    };
    let (width, height) = (running.config.width, running.config.height);
    let scene = self.game.scene(width as f32 / height as f32);
    let view = texture.texture.create_view(&TextureViewDescriptor::default());
    let frame = Frame {
      view_projection: scene.view_projection,
      eye: scene.eye,
      light: Light::DAY,
      characters: &scene.characters,
    };
    running.renderer.draw(&running.gpu, &view, [width, height], &frame);
    running.window.pre_present_notify();
    running.gpu.queue.present(texture);
    if let Some(left) = &mut self.frames {
      *left = left.saturating_sub(1);
      if *left == 0 {
        event_loop.exit();
      }
    }
  }

  /// The movement keys: the arrows, W A S D of a QWERTY keyboard and Z Q S D of an AZERTY one.
  fn key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
    let down = event.state == ElementState::Pressed;
    let direction = match &event.logical_key {
      Key::Named(NamedKey::Escape) if down => return event_loop.exit(),
      Key::Named(NamedKey::ArrowUp) => Direction::Forward,
      Key::Named(NamedKey::ArrowDown) => Direction::Back,
      Key::Named(NamedKey::ArrowLeft) => Direction::Left,
      Key::Named(NamedKey::ArrowRight) => Direction::Right,
      Key::Character(text) => match text.to_lowercase().as_str() {
        "w" | "z" => Direction::Forward,
        "s" => Direction::Back,
        "a" | "q" => Direction::Left,
        "d" => Direction::Right,
        _ => return,
      },
      _ => return,
    };
    self.game.key(direction, down);
  }
}

impl ApplicationHandler for App {
  fn resumed(&mut self, event_loop: &ActiveEventLoop) {
    if self.running.is_none()
      && let Err(failure) = self.start(event_loop)
    {
      self.failure = Some(failure);
      event_loop.exit();
    }
  }

  fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
    match event {
      WindowEvent::CloseRequested => event_loop.exit(),
      WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
        if let Some(running) = &mut self.running {
          running.config.width = size.width;
          running.config.height = size.height;
          running.surface.configure(&running.gpu.device, &running.config);
        }
      }
      WindowEvent::KeyboardInput { event, .. } => self.key(event_loop, &event),
      WindowEvent::MouseInput { state, button: MouseButton::Right, .. } => {
        self.orbiting = state == ElementState::Pressed;
      }
      WindowEvent::MouseWheel { delta, .. } => self.game.zoom(match delta {
        MouseScrollDelta::LineDelta(_, notches) => notches,
        MouseScrollDelta::PixelDelta(moved) => (moved.y / PIXELS_A_NOTCH) as f32,
      }),
      WindowEvent::Focused(false) => {
        self.orbiting = false;
        self.game.release();
      }
      WindowEvent::RedrawRequested => self.frame(event_loop),
      _ => {}
    }
  }

  fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
    if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
      && self.orbiting
    {
      self.game.orbit(dx as f32, dy as f32);
    }
  }

  fn about_to_wait(&mut self, _: &ActiveEventLoop) {
    if let Some(running) = &self.running {
      running.window.request_redraw();
    }
  }
}
