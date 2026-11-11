//! The keys of the player, as a heading on the ground.

use mmo_protocol::Angle;

/// The movement keys that are down.
#[expect(clippy::struct_excessive_bools, reason = "each of the four keys is down or up")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
  /// The key that walks toward where the camera looks.
  pub forward: bool,
  /// The key that walks away from where the camera looks.
  pub back: bool,
  /// The key that walks to the left of the camera.
  pub left: bool,
  /// The key that walks to the right of the camera.
  pub right: bool,
}

impl Keys {
  /// The heading of the walk for a camera that looks along `yaw`, in radians, or `None` when no
  /// key is down or two opposite keys cancel each other. Two keys at an angle give the heading
  /// between them.
  pub fn heading(self, yaw: f32) -> Option<Angle> {
    let ahead = f32::from(u8::from(self.forward)) - f32::from(u8::from(self.back));
    let side = f32::from(u8::from(self.right)) - f32::from(u8::from(self.left));
    (ahead != 0.0 || side != 0.0).then(|| Angle::from_radians(yaw + side.atan2(ahead)))
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::Angle;

  use super::Keys;

  const FORWARD: Keys = Keys { forward: true, back: false, left: false, right: false };

  #[test]
  fn each_key_walks_to_its_side_of_the_view_of_the_camera() {
    let heading = |keys: Keys, yaw: f32| keys.heading(yaw);
    assert_eq!(heading(FORWARD, 0.0), Some(Angle(0)));
    assert_eq!(heading(FORWARD, std::f32::consts::FRAC_PI_2), Some(Angle(16384)));
    assert_eq!(heading(Keys { back: true, ..Keys::default() }, 0.0), Some(Angle(32768)));
    assert_eq!(heading(Keys { right: true, ..Keys::default() }, 0.0), Some(Angle(16384)));
    assert_eq!(heading(Keys { left: true, ..Keys::default() }, 0.0), Some(Angle(49152)));
    assert_eq!(
      heading(Keys { forward: true, right: true, ..Keys::default() }, 0.0),
      Some(Angle(8192))
    );
  }

  #[test]
  fn no_key_and_two_opposite_keys_stand_still() {
    assert_eq!(Keys::default().heading(1.0), None);
    assert_eq!(Keys { forward: true, back: true, ..Keys::default() }.heading(1.0), None);
    assert_eq!(Keys { left: true, right: true, ..Keys::default() }.heading(1.0), None);
  }
}
