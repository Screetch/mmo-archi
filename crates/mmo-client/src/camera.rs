//! The third-person camera: it orbits and zooms around the player.

use core::f32::consts::{FRAC_PI_2, FRAC_PI_3};

use glam::camera::rh::{proj::directx, view};
use glam::{Mat4, Vec3};

/// The radians of a turn of the camera for each pixel that the pointer moves.
const PER_PIXEL: f32 = 0.005;
/// The camera looks at a point this high above the feet of the player, in metres.
const FOCUS_HEIGHT: f32 = 1.5;
/// The nearest and the farthest distance of the camera from the player, in metres.
const NEAREST: f32 = 2.0;
const FARTHEST: f32 = 25.0;
/// The lowest and the highest angle of the camera over the ground, in radians. The camera never
/// goes under the ground, and never looks straight down.
const LOWEST: f32 = 0.05;
const HIGHEST: f32 = FRAC_PI_2 - 0.05;
/// The distance gets this part of itself nearer for each step of the wheel.
const ZOOM: f32 = 0.1;

/// The camera of the player: it looks at a point above the player, from a distance, along a
/// direction that the player turns.
///
/// The world has the axis y up, and a yaw of 0 looks along the axis x. A yaw of a quarter turn
/// looks along the axis z, as a heading of `mmo-collide` does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
  focus: Vec3,
  yaw: f32,
  pitch: f32,
  distance: f32,
}

impl Default for Camera {
  fn default() -> Self {
    Self { focus: Vec3::new(0.0, FOCUS_HEIGHT, 0.0), yaw: 0.0, pitch: 0.4, distance: 6.0 }
  }
}

impl Camera {
  /// Turns the camera by a move of the pointer, in pixels: to the right turns it to the right,
  /// and down lifts it. The angle over the ground stays between 3 and 87 degrees.
  pub fn orbit(&mut self, dx: f32, dy: f32) {
    self.yaw += dx * PER_PIXEL;
    self.pitch = (self.pitch + dy * PER_PIXEL).clamp(LOWEST, HIGHEST);
  }

  /// Brings the camera nearer for each positive step of the wheel, and takes it away for each
  /// negative step. The distance stays between 2 m and 25 m.
  pub fn zoom(&mut self, steps: f32) {
    self.distance = (self.distance * (1.0 - ZOOM * steps)).clamp(NEAREST, FARTHEST);
  }

  /// Looks at the player whose feet are at `feet`, in metres.
  pub fn follow(&mut self, feet: [f32; 3]) {
    self.focus = Vec3::from(feet) + Vec3::Y * FOCUS_HEIGHT;
  }

  /// The direction of the camera on the ground, in radians, from the axis x toward the axis z.
  pub fn yaw(&self) -> f32 {
    self.yaw
  }

  /// The place of the eye, in metres.
  pub fn eye(&self) -> Vec3 {
    let ahead = Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin());
    self.focus - ahead * (self.pitch.cos() * self.distance)
      + Vec3::Y * (self.pitch.sin() * self.distance)
  }

  /// The matrix that takes a point of the world to the clip space of wgpu, with a view of 60
  /// degrees for a window of the ratio `aspect` of its width to its height.
  pub fn view_projection(&self, aspect: f32) -> Mat4 {
    directx::perspective(FRAC_PI_3, aspect, 0.1, 2000.0)
      * view::look_at_mat4(self.eye(), self.focus, Vec3::Y)
  }
}

#[cfg(test)]
mod tests {
  use core::f32::consts::FRAC_PI_2;

  use glam::{Vec3, Vec4};

  use super::{Camera, FARTHEST, HIGHEST, LOWEST, NEAREST};

  fn near(one: Vec3, other: Vec3) -> bool {
    (one - other).length() < 1e-3
  }

  fn looking_at(feet: [f32; 3]) -> Camera {
    let mut camera = Camera::default();
    camera.follow(feet);
    camera
  }

  #[test]
  fn the_camera_stands_behind_and_above_the_player_along_its_yaw() {
    let mut camera = looking_at([10.0, 0.0, 20.0]);
    let (height, back) = (0.4_f32.sin() * 6.0, 0.4_f32.cos() * 6.0);
    assert!(near(camera.eye(), Vec3::new(10.0 - back, 1.5 + height, 20.0)));
    camera.orbit(FRAC_PI_2 / 0.005, 0.0);
    assert!(near(camera.eye(), Vec3::new(10.0, 1.5 + height, 20.0 - back)));
  }

  #[test]
  fn the_pointer_turns_the_camera_and_its_angle_stays_over_the_ground() {
    let mut camera = looking_at([0.0, 0.0, 0.0]);
    camera.orbit(200.0, 10_000.0);
    assert!((camera.yaw() - 1.0).abs() < 1e-5);
    assert!((camera.eye().y - (1.5 + HIGHEST.sin() * 6.0)).abs() < 1e-4);
    camera.orbit(0.0, -20_000.0);
    assert!((camera.eye().y - (1.5 + LOWEST.sin() * 6.0)).abs() < 1e-4);
  }

  #[test]
  fn the_wheel_zooms_between_two_distances() {
    let mut camera = looking_at([0.0, 0.0, 0.0]);
    let away = |camera: &Camera| (camera.eye() - Vec3::new(0.0, 1.5, 0.0)).length();
    camera.zoom(1.0);
    assert!((away(&camera) - 5.4).abs() < 1e-4);
    camera.zoom(-100.0);
    assert!((away(&camera) - FARTHEST).abs() < 1e-3);
    camera.zoom(100.0);
    assert!((away(&camera) - NEAREST).abs() < 1e-3);
  }

  #[test]
  fn the_player_is_at_the_middle_of_the_view_and_a_point_behind_the_eye_is_not_shown() {
    let view = looking_at([3.0, 0.0, 4.0]).view_projection(16.0 / 9.0);
    let middle = view * Vec4::new(3.0, 1.5, 4.0, 1.0);
    let (x, y, depth) = (middle.x / middle.w, middle.y / middle.w, middle.z / middle.w);
    assert!(x.abs() < 1e-4 && y.abs() < 1e-4 && (0.0..1.0).contains(&depth), "{x} {y} {depth}");
    let behind = view * Vec4::new(-100.0, 1.5, 4.0, 1.0);
    assert!(behind.w < 0.0);
  }
}
