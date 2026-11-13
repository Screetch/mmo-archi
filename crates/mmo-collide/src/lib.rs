//! The deterministic collisions that the server and the client share.
//!
//! [`step`] moves one body for one tick of [`DT`] in a [`Terrain`]. The same terrain, the same
//! body and the same input give the same bits on each system: parry3d runs with its feature
//! `enhanced-determinism`, the crate takes its sine and its cosine from libm, and each position
//! is rounded to the centimetre. So a client that predicts its body and the layer writer that
//! decides it agree. A change of the result of a step changes the version of the protocol.

mod ground;
mod terrain;

use core::f32::consts::TAU;

use parry3d::math::Vector;
use parry3d::shape::Capsule;

pub use ground::ground;
use terrain::Hit;
pub use terrain::{Solid, Terrain, TerrainError};

/// The time of one tick, in seconds: the world runs 20 ticks each second.
pub const DT: f32 = 1.0 / 20.0;

/// The speed of a body that walks, in metres each second.
const WALK_SPEED: f32 = 6.0;
/// The acceleration of a body that falls, in metres each second squared.
const GRAVITY: f32 = 9.81;
/// The speed of a body that falls stops to grow here, in metres each second.
const TERMINAL_SPEED: f32 = 50.0;
/// The highest rise that a body that stands climbs, and the deepest drop that it follows, in one
/// tick, in metres.
const STEP_HEIGHT: f32 = 0.35;
/// The radius of the capsule of a body, in metres.
const RADIUS: f32 = 0.4;
/// Half of the length of the segment between the two half spheres of the capsule, in metres.
const HALF_SEGMENT: f32 = 0.5;
/// A cast stops the capsule at this distance from a surface, in metres.
const SKIN: f32 = 0.02;
/// After a cast stops it, the capsule steps back from the surface by this distance, in metres.
/// So the next cast does not start at the distance where the last one stopped, where the noise
/// of the query gives false hits along the surface. The rounding of a position to the
/// centimetre moves the capsule by 0.87 cm at most, so it does not take the capsule back there.
const MARGIN: f32 = 0.01;
/// The capsule stands this high above the position of the body, so a body that stands on the
/// ground has the height of the ground.
const CENTER: Vector = Vector::new(0.0, SKIN + MARGIN + RADIUS + HALF_SEGMENT, 0.0);
/// A climb ends only on a surface whose normal leans 45 degrees or less from the axis y: this is
/// the cosine of that angle. So a body does not climb onto the edge of a step that it cannot
/// stand on, as under a low ceiling.
const FOOTING: f32 = core::f32::consts::FRAC_1_SQRT_2;
/// The count of the surfaces along which a body slides in one tick.
const SLIDES: usize = 4;

/// A body that walks: a capsule of 1.8 m that stands up, with a radius of 0.4 m.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Body {
  /// The point under the capsule, in centimetres. A body that stands on the ground has the
  /// height of the ground.
  pub position: [i32; 3],
  /// The speed along the axis y, in centimetres each second. It is 0 for a body that stands, and
  /// below 0 for a body that falls. A body of speed 0 that has no ground under it falls at its
  /// next step.
  pub vertical_speed: i16,
}

/// What a player asks its body to do for one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
  /// The body does not walk.
  Idle,
  /// The body walks at 6 m/s.
  Walk {
    /// The direction of the walk on the ground: 0 points along the axis x, 16384 along the
    /// axis z, and 65536 is a whole turn.
    heading: u16,
  },
}

/// Moves `body` for one tick of [`DT`] in `terrain`, as `input` asks.
///
/// The body slides along each wall that it meets. A body that stands climbs a rise of 35 cm at
/// most in one tick, and follows the ground down a drop of 35 cm at most. A body with no ground
/// under it falls with the gravity, up to 50 m/s, and stands where it meets the ground.
pub fn step(terrain: &Terrain, body: Body, input: Input) -> Body {
  let feet = metres(body.position);
  let standing = body.vertical_speed == 0;
  let speed = (f32::from(body.vertical_speed) / 100.0 - GRAVITY * DT).max(-TERMINAL_SPEED);
  let fall = -speed * DT;
  let reach = fall + if standing { STEP_HEIGHT } else { 0.0 };
  let (walk, lift) = match input {
    Input::Idle => (Vector::ZERO, 0.0),
    Input::Walk { heading } => {
      let turn = f32::from(heading) * (TAU / 65536.0);
      let walk = Vector::new(libm::cosf(turn), 0.0, libm::sinf(turn)) * (WALK_SPEED * DT);
      let up = Vector::Y * STEP_HEIGHT;
      let lift = if standing {
        cast(terrain, feet, up)
          .map_or(STEP_HEIGHT, |hit| (hit.fraction * STEP_HEIGHT - MARGIN).max(0.0))
      } else {
        0.0
      };
      (walk, lift)
    }
  };
  let climb = stride(terrain, feet + Vector::Y * lift, walk, lift + reach);
  let (lift, end) = match climb {
    Stride::Landed { under, .. }
      if under.point.y > feet.y + STEP_HEIGHT || under.normal.y < FOOTING =>
    {
      (0.0, stride(terrain, feet, walk, reach))
    }
    _ => (lift, climb),
  };
  match end {
    Stride::Landed { feet, .. } => Body { position: centimetres(feet), vertical_speed: 0 },
    Stride::Aloft { feet } => Body {
      position: centimetres(feet - Vector::Y * (lift + fall)),
      vertical_speed: centimetres_each_second(speed),
    },
  }
}

/// The end of a walk of one tick.
enum Stride {
  /// The body stands on the surface that `under` stopped.
  Landed { feet: Vector, under: Hit },
  /// The body found no surface under it.
  Aloft { feet: Vector },
}

/// Walks the body at `feet` along `walk`, then puts it down on the first surface within `reach`
/// under it.
fn stride(terrain: &Terrain, feet: Vector, walk: Vector, reach: f32) -> Stride {
  let moved = slide(terrain, feet, walk);
  let down = Vector::NEG_Y * reach;
  match cast(terrain, moved, down) {
    Some(hit) => {
      Stride::Landed { feet: moved + down * hit.fraction + Vector::Y * MARGIN, under: hit }
    }
    None => Stride::Aloft { feet: moved },
  }
}

/// Moves the body at `feet` along `motion`, and slides it along each wall that stops it.
///
/// The horizontal part of the normal of a wall takes out the part of the rest of the motion that
/// goes into the wall. A floor or a ceiling has almost no horizontal part, so the walk goes on
/// along it.
fn slide(terrain: &Terrain, mut feet: Vector, motion: Vector) -> Vector {
  let mut left = motion;
  for _ in 0..SLIDES {
    let Some(hit) = cast(terrain, feet, left) else {
      return feet + left;
    };
    feet += left * hit.fraction + hit.normal * MARGIN;
    left *= 1.0 - hit.fraction;
    let across = Vector::new(hit.normal.x, 0.0, hit.normal.z);
    left -= across * left.dot(across).min(0.0);
  }
  feet
}

/// Casts the capsule of the body at `feet` along `motion` in `terrain`.
fn cast(terrain: &Terrain, feet: Vector, motion: Vector) -> Option<Hit> {
  terrain.cast(&Capsule::new_y(HALF_SEGMENT, RADIUS), feet + CENTER, motion, SKIN)
}

/// The hash FNV-1a of the bits of `bodies`, in their order. Two systems that agree on each body
/// give the same hash, so one constant can hold the state of a whole world in a test.
pub fn state_hash(bodies: &[Body]) -> u64 {
  bodies
    .iter()
    .flat_map(|body| {
      let [x, y, z] = body.position;
      [x.to_le_bytes(), y.to_le_bytes(), z.to_le_bytes()]
        .into_iter()
        .flatten()
        .chain(body.vertical_speed.to_le_bytes())
    })
    .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
      (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[expect(clippy::cast_precision_loss, reason = "an f32 holds each centimetre up to 167 km")]
fn metres(position: [i32; 3]) -> Vector {
  Vector::from(position.map(|centimetres| centimetres as f32 / 100.0))
}

#[expect(clippy::cast_possible_truncation, reason = "a position past 21474 km saturates")]
fn centimetres(position: Vector) -> [i32; 3] {
  position.to_array().map(|metres| (metres * 100.0).round() as i32)
}

#[expect(clippy::cast_possible_truncation, reason = "the terminal speed fits in an i16")]
fn centimetres_each_second(speed: f32) -> i16 {
  (speed * 100.0).round() as i16
}

#[cfg(test)]
mod tests {
  use super::{Body, Input, Solid, Terrain, state_hash, step};

  /// A floor of 100 m by 100 m whose top is at the height 0.
  fn floor() -> Solid {
    Solid::Cuboid { center: [0.0, -0.5, 0.0], half_extents: [50.0, 0.5, 50.0] }
  }

  /// A box from x = 2 m to x = 4 m, from z = -5 m to z = 5 m, from the floor to `top`.
  fn block(top: f32) -> Solid {
    Solid::Cuboid { center: [3.0, top / 2.0, 0.0], half_extents: [1.0, top / 2.0, 5.0] }
  }

  fn world(solids: &[Solid]) -> Terrain {
    Terrain::new(solids).expect("the solids of a test are valid")
  }

  fn at(x: i32, y: i32, z: i32) -> Body {
    Body { position: [x, y, z], vertical_speed: 0 }
  }

  fn run(terrain: &Terrain, mut body: Body, input: Input, ticks: usize) -> Body {
    for _ in 0..ticks {
      body = step(terrain, body, input);
    }
    body
  }

  const EAST: Input = Input::Walk { heading: 0 };

  #[test]
  fn a_body_on_the_ground_stays_still() {
    let terrain = world(&[floor()]);
    assert_eq!(run(&terrain, at(120, 0, -340), Input::Idle, 20), at(120, 0, -340));
  }

  #[test]
  fn a_body_walks_30_cm_each_tick_toward_its_heading() {
    let terrain = world(&[floor()]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 10), at(300, 0, 0));
    assert_eq!(run(&terrain, at(0, 0, 0), Input::Walk { heading: 16384 }, 10), at(0, 0, 300));
    assert_eq!(run(&terrain, at(0, 0, 0), Input::Walk { heading: 32768 }, 10), at(-300, 0, 0));
  }

  #[test]
  fn a_body_falls_with_the_gravity_and_stands_where_it_meets_the_ground() {
    let terrain = world(&[floor()]);
    let first = step(&terrain, at(0, 1000, 0), Input::Idle);
    assert_eq!(first, Body { position: [0, 998, 0], vertical_speed: -49 });
    assert_eq!(
      step(&terrain, first, Input::Idle),
      Body { position: [0, 993, 0], vertical_speed: -98 }
    );
    assert_eq!(run(&terrain, at(0, 1000, 0), Input::Idle, 40), at(0, 0, 0));
  }

  #[test]
  fn a_fall_stops_to_speed_up_at_50_m_each_second() {
    let nothing = world(&[]);
    assert_eq!(run(&nothing, at(0, 0, 0), Input::Idle, 200).vertical_speed, -5000);
  }

  /// The capsule, of a radius of 40 cm, stops 3 cm from the wall at x = 2 m.
  #[test]
  fn a_wall_stops_a_body_a_little_before_its_capsule_touches_it() {
    let terrain = world(&[floor(), block(2.0)]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 20), at(157, 0, 0));
  }

  #[test]
  fn a_body_slides_along_a_wall_that_it_meets_at_an_angle() {
    let terrain = world(&[floor(), block(2.0)]);
    assert_eq!(run(&terrain, at(100, 0, 0), Input::Walk { heading: 8192 }, 10), at(157, 0, 210));
  }

  #[test]
  fn a_body_climbs_a_step_of_30_cm_and_follows_the_ground_down() {
    let terrain = world(&[floor(), block(0.3)]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 10), at(300, 30, 0));
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 15), at(450, 0, 0));
  }

  #[test]
  fn a_rise_of_40_cm_stops_a_body() {
    let terrain = world(&[floor(), block(0.4)]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 10), at(157, 0, 0));
  }

  #[test]
  fn a_body_that_walks_off_a_ledge_falls_and_keeps_its_walk() {
    let terrain = world(&[floor(), block(1.0)]);
    let falling = run(&terrain, at(300, 100, 0), EAST, 5);
    assert_eq!(falling, Body { position: [450, 93, 0], vertical_speed: -49 });
    assert_eq!(run(&terrain, falling, EAST, 9), at(720, 0, 0));
  }

  /// Each wall keeps the capsule 2 cm to 3 cm from it, so the body stops between x = 1.74 m and
  /// x = 1.77 m, short of the corner at x = 3 m.
  #[test]
  fn a_body_that_walks_into_a_narrow_corner_stops_in_it() {
    let wall = |z: f32| [[3.0, 0.0, 0.0], [3.0, 2.0, 0.0], [-2.0, 0.0, z], [-2.0, 2.0, z]];
    let [tip, top, left, left_top] = wall(1.82);
    let [_, _, right, right_top] = wall(-1.82);
    let corner = Solid::Mesh {
      vertices: vec![tip, top, left, left_top, right, right_top],
      triangles: vec![[0, 1, 2], [1, 3, 2], [0, 4, 1], [1, 4, 5]],
    };
    let terrain = world(&[floor(), corner]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 10), at(176, 0, 0));
  }

  /// Under a ceiling at 2 m, the body rises 15 cm at most, so the edge of the step at 30 cm stops
  /// its round bottom at x = 1.59 m.
  #[test]
  fn a_low_ceiling_stops_a_climb() {
    let ceiling = Solid::Cuboid { center: [0.0, 2.5, 0.0], half_extents: [50.0, 0.5, 50.0] };
    let terrain = world(&[floor(), block(0.3), ceiling]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 10), at(159, 0, 0));
  }

  #[test]
  fn a_ball_and_a_mesh_stop_a_body_as_a_box_does() {
    let ground = Solid::Mesh {
      vertices: vec![
        [-50.0, 0.0, -50.0],
        [50.0, 0.0, -50.0],
        [50.0, 0.0, 50.0],
        [-50.0, 0.0, 50.0],
      ],
      triangles: vec![[0, 2, 1], [0, 3, 2]],
    };
    let ball = Solid::Ball { center: [3.0, 0.9, 0.0], radius: 1.0 };
    let terrain = world(&[ground, ball]);
    assert_eq!(run(&terrain, at(0, 0, 0), EAST, 20), at(157, 0, 0));
  }

  #[test]
  fn the_hash_of_the_state_is_fnv_1a_of_its_bits() {
    assert_eq!(state_hash(&[]), 0xcbf2_9ce4_8422_2325);
    let body = Body { position: [1, -2, 300], vertical_speed: -49 };
    assert_eq!(state_hash(&[body]), HASH_OF_ONE_BODY);
  }

  const HASH_OF_ONE_BODY: u64 = 0x8758_d6da_079c_c198;

  /// A town of walls, steps, balls and a ramp of triangles, on a floor.
  fn town() -> Vec<Solid> {
    let mut solids = vec![floor()];
    for n in 0..16u8 {
      let x = f32::from(n % 4) * 12.0 - 18.0;
      let z = f32::from(n / 4) * 12.0 - 18.0;
      let top = f32::from(n % 5 + 1) * 0.15;
      solids.push(Solid::Cuboid { center: [x, top / 2.0, z], half_extents: [2.0, top / 2.0, 1.5] });
      solids.push(Solid::Ball { center: [x + 5.0, 0.5, z + 4.0], radius: 0.8 });
    }
    solids.push(Solid::Mesh {
      vertices: vec![[20.0, 0.0, -5.0], [30.0, 3.0, -5.0], [30.0, 3.0, 5.0], [20.0, 0.0, 5.0]],
      triangles: vec![[0, 2, 1], [0, 3, 2]],
    });
    solids
  }

  /// The input of the body `n` at the tick `tick`: a walk whose heading changes each second, and a
  /// rest of one second in five.
  fn input(n: u32, tick: u32) -> Input {
    let mix = (n * 7919 + tick / 20 * 104_729) % 65536;
    if mix.is_multiple_of(5) {
      Input::Idle
    } else {
      Input::Walk { heading: u16::try_from(mix).unwrap() }
    }
  }

  /// 1000 bodies on a grid of 1.5 m, one in seven of them 5 m above the floor, for 200 ticks.
  fn play() -> Vec<Body> {
    let terrain = world(&town());
    let mut bodies: Vec<Body> = (0..1000)
      .map(|n| at(n % 40 * 150 - 3000, if n % 7 == 0 { 500 } else { 0 }, n / 40 * 150 - 1800))
      .collect();
    for tick in 0..200 {
      for (n, body) in (0..).zip(bodies.iter_mut()) {
        *body = step(&terrain, *body, input(n, tick));
      }
    }
    bodies
  }

  #[test]
  fn the_same_inputs_give_the_same_bits_on_each_system() {
    let first = play();
    assert_eq!(play(), first);
    assert_eq!(state_hash(&first), HASH_OF_THE_TOWN);
  }

  const HASH_OF_THE_TOWN: u64 = 0x5987_25b3_26b1_77df;
}
