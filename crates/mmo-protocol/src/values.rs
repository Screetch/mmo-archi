use std::f64::consts::TAU;
use std::fmt;

use bitcode::{Decode, Encode};

const CENTIMETRES: f64 = 100.0;
const STEPS: f64 = 65536.0;

/// The ticks that a layer writer runs each second.
pub const TICKS_PER_SECOND: u32 = 20;

/// The number of a tick of a layer writer, which runs [`TICKS_PER_SECOND`] ticks each second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Tick(pub u32);

/// The id of a zone, as the table `zones` of PostgreSQL holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct ZoneId(pub i32);

/// The id of a shard: one independent world, with its own zones, layers, queue and players. A
/// player of one shard never meets a player of another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct ShardId(pub u32);

/// A layer: one full copy of a zone in a shard, with its own state, its own players and its own
/// writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Layer {
  /// The zone that the layer copies.
  pub zone: ZoneId,
  /// The number of the layer in its zone: the least number from 1 up that the zone did not use
  /// in its shard when the layer opened.
  pub number: u32,
}

/// The epoch of a layer: the count of the writers that claimed it, as the table `layers` of
/// PostgreSQL holds it.
///
/// Each save of a layer writer carries its epoch, and the keeper refuses a save of an old epoch.
/// So a writer that lost its layer, but still runs, writes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Epoch(pub i64);

/// The id of an account, as the table `accounts` of PostgreSQL holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct AccountId(pub i64);

/// The id of a character, as the table `characters` of PostgreSQL holds it.
///
/// A character keeps it in each zone, and it is also the id of the entity of the character in a
/// [`Snapshot`](crate::Snapshot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct CharacterId(pub i64);

/// The id of the connection of a client to a gateway.
///
/// The gateway draws it at random from 64 bits when the connection opens. So a layer writer tells
/// the links of two gateways apart with no list of gateways: two ids are equal only by a chance
/// that the game ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Link(pub u64);

/// A secret of 32 random bytes from the random source of the system: a session, or a place in the
/// queue. Its `Debug` does not show the bytes, so a log never holds them.
#[derive(Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct Token(pub [u8; 32]);

impl fmt::Debug for Token {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("Token(..)")
  }
}

/// The password of an account, which only the encrypted connection to the login service carries.
/// Its `Debug` does not show it, so a log never holds it.
#[derive(Clone, PartialEq, Eq, Encode, Decode)]
pub struct Password(pub String);

impl fmt::Debug for Password {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("Password(..)")
  }
}

/// A position in the world, in centimetres on each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Position {
  /// The first axis, in centimetres.
  pub x: i32,
  /// The second axis, in centimetres.
  pub y: i32,
  /// The third axis, in centimetres.
  pub z: i32,
}

impl Position {
  /// A position in metres, with each axis rounded to the nearest centimetre, and a half
  /// centimetre away from zero. A value past the range of an `i32` gives the nearest end of that
  /// range, and a value that is not a number gives 0.
  #[expect(clippy::cast_possible_truncation, reason = "the cast saturates, as the doc says")]
  pub fn from_metres([x, y, z]: [f32; 3]) -> Self {
    Self { x: rounded(x) as i32, y: rounded(y) as i32, z: rounded(z) as i32 }
  }

  /// The position in metres, at the precision of an `f32`.
  #[expect(clippy::cast_possible_truncation, reason = "an f32 holds the metres at its precision")]
  pub fn metres(self) -> [f32; 3] {
    [self.x, self.y, self.z].map(|at| (f64::from(at) / CENTIMETRES) as f32)
  }
}

/// The movement of an entity between two snapshots, in centimetres on each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Offset {
  /// The movement on the first axis, in centimetres.
  pub x: i32,
  /// The movement on the second axis, in centimetres.
  pub y: i32,
  /// The movement on the third axis, in centimetres.
  pub z: i32,
}

/// A velocity, in centimetres each second on each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Velocity {
  /// The speed on the first axis, in centimetres each second.
  pub x: i16,
  /// The speed on the second axis, in centimetres each second.
  pub y: i16,
  /// The speed on the third axis, in centimetres each second.
  pub z: i16,
}

impl Velocity {
  /// A velocity in metres each second, with each axis rounded to the nearest centimetre each
  /// second, and a half away from zero. A value past the range of an `i16`, 327 m/s, gives the
  /// nearest end of that range, and a value that is not a number gives 0.
  #[expect(clippy::cast_possible_truncation, reason = "the cast saturates, as the doc says")]
  pub fn from_metres_per_second([x, y, z]: [f32; 3]) -> Self {
    Self { x: rounded(x) as i16, y: rounded(y) as i16, z: rounded(z) as i16 }
  }

  /// The velocity in metres each second.
  pub fn metres_per_second(self) -> [f32; 3] {
    [self.x, self.y, self.z].map(|at| f32::from(at) / 100.0)
  }
}

/// A direction on the horizontal plane, as a part of one turn in 65536 steps, so a step is about
/// 0.0055 degrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Angle(pub u16);

impl Angle {
  /// The nearest step of an angle in radians, and a half step away from zero. An angle out of one
  /// turn wraps into it. Check that the angle is a finite number before the call: else the step
  /// has no meaning.
  #[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the cast to u16 keeps the steps of one turn, so the angle wraps"
  )]
  pub fn from_radians(radians: f32) -> Self {
    Self((f64::from(radians) / TAU * STEPS).round() as i64 as u16)
  }

  /// The angle in radians, from 0 to one turn.
  pub fn radians(self) -> f32 {
    f32::from(self.0) * (std::f32::consts::TAU / 65536.0)
  }
}

fn rounded(value: f32) -> f64 {
  (f64::from(value) * CENTIMETRES).round()
}

#[cfg(test)]
mod tests {
  use std::f32::consts::{FRAC_PI_2, PI, TAU};

  use super::{Angle, Password, Position, Token, Velocity};

  #[test]
  fn a_position_rounds_to_the_nearest_centimetre_and_a_half_away_from_zero() {
    let position = Position::from_metres([0.125, -0.125, 1.005]);
    assert_eq!(position, Position { x: 13, y: -13, z: 100 });
    let far = Position::from_metres([1e30, -1e30, f32::NAN]);
    assert_eq!(far, Position { x: i32::MAX, y: i32::MIN, z: 0 });
  }

  #[test]
  fn a_position_gives_its_metres() {
    let metres = Position { x: 12345, y: -7, z: 0 }.metres();
    assert_eq!(metres.map(f32::to_bits), [123.45_f32, -0.07, 0.0].map(f32::to_bits));
  }

  #[test]
  fn a_velocity_rounds_to_the_centimetre_each_second_and_saturates() {
    let velocity = Velocity::from_metres_per_second([6.0, -0.125, 400.0]);
    assert_eq!(velocity, Velocity { x: 600, y: -13, z: i16::MAX });
    let far = Velocity::from_metres_per_second([-400.0, f32::NAN, 0.0]);
    assert_eq!(far, Velocity { x: i16::MIN, y: 0, z: 0 });
    let metres = Velocity { x: 600, y: -13, z: 5 }.metres_per_second();
    assert_eq!(metres.map(f32::to_bits), [6.0_f32, -0.13, 0.05].map(f32::to_bits));
  }

  #[test]
  fn an_angle_takes_the_nearest_step_and_wraps_in_one_turn() {
    assert_eq!(Angle::from_radians(0.0), Angle(0));
    assert_eq!(Angle::from_radians(FRAC_PI_2), Angle(16384));
    assert_eq!(Angle::from_radians(PI), Angle(32768));
    assert_eq!(Angle::from_radians(-FRAC_PI_2), Angle(49152));
    assert_eq!(Angle::from_radians(TAU), Angle(0));
    assert_eq!(Angle::from_radians(TAU + FRAC_PI_2), Angle(16384));
    assert_eq!(Angle(16384).radians().to_bits(), FRAC_PI_2.to_bits());
    assert_eq!(Angle(u16::MAX).radians().to_bits(), (TAU - TAU / 65536.0).to_bits());
  }

  #[test]
  fn a_secret_never_shows_in_a_log() {
    assert_eq!(format!("{:?}", Token([7; 32])), "Token(..)");
    assert_eq!(format!("{:?}", Password("hunter2".to_owned())), "Password(..)");
  }
}
