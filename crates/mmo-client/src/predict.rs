//! The prediction of the body of the player.

use std::collections::VecDeque;

use mmo_collide::{Body, Terrain, step};
use mmo_protocol::{Angle, Entity, Input, Position, Tick};

/// The ticks that the client runs ahead of the last snapshot that it got: an input must reach the
/// layer writer 1 to 4 ticks before the tick that it names.
const LEAD: u32 = 3;

/// The client runs ahead of the last snapshot by 1 to this many ticks. Outside that range, its
/// clock is not the clock of the writer, so the client takes the clock of the snapshot again.
const WINDOW: i64 = 6;

/// The inputs that the client keeps for the next snapshot, 3.2 s of them.
const PENDING: usize = 64;

/// The inputs that a datagram carries.
const REDUNDANT: usize = 3;

/// One input that the layer writer did not answer yet: the input as it goes on the wire, and the
/// walk that it asks of `mmo-collide`.
struct Pending {
  input: Input,
  walk: mmo_collide::Input,
}

/// The body of the player, which the client runs ahead of the layer writer with the same step.
///
/// The client runs one [`Predictor::step`] each tick of 50 ms, and shows the result at once. Each
/// snapshot of the writer holds the state of the player at its tick: [`Predictor::correct`] takes
/// that state, and runs the inputs that the snapshot does not answer yet again. When the writer
/// agrees, the result does not change.
pub struct Predictor {
  terrain: Terrain,
  tick: Tick,
  before: Body,
  body: Body,
  pending: VecDeque<Pending>,
}

impl Predictor {
  /// Starts at the state `entity` that the writer gave at its tick `server`.
  pub fn start(terrain: Terrain, server: Tick, entity: &Entity) -> Self {
    let body = body_of(entity);
    Self { terrain, tick: Tick(server.0 + LEAD), before: body, body, pending: VecDeque::new() }
  }

  /// Runs one tick: moves the body for the walk of `heading`, or for none, and gives the input to
  /// send. Its tick is the next tick of the client.
  pub fn step(&mut self, heading: Option<Angle>, view: Angle) -> Input {
    self.tick = Tick(self.tick.0 + 1);
    let input = Input { tick: self.tick, heading, view };
    let walk = heading
      .map_or(mmo_collide::Input::Idle, |heading| mmo_collide::Input::Walk { heading: heading.0 });
    self.before = self.body;
    self.body = step(&self.terrain, self.body, walk);
    if self.pending.len() == PENDING {
      self.pending.pop_front();
    }
    self.pending.push_back(Pending { input, walk });
    input
  }

  /// The last 3 inputs, oldest first, for a datagram: the writer recovers the input of a lost
  /// datagram from the next. Before the third input, the oldest one fills the places. `None`
  /// before the first input.
  pub fn recent(&self) -> Option<[Input; REDUNDANT]> {
    let mut inputs = [self.pending.front()?.input; REDUNDANT];
    for (place, pending) in inputs.iter_mut().rev().zip(self.pending.iter().rev()) {
      *place = pending.input;
    }
    Some(inputs)
  }

  /// Takes the state `entity` of the player at the tick `server` of the writer, and runs again
  /// each input after that tick. The writer has answered the inputs up to its tick.
  pub fn correct(&mut self, server: Tick, entity: &Entity) {
    let ahead = i64::from(self.tick.0) - i64::from(server.0);
    if !(1..=WINDOW).contains(&ahead) {
      self.tick = Tick(server.0 + LEAD);
      self.body = body_of(entity);
      self.before = self.body;
      self.pending.clear();
      return;
    }
    while self.pending.front().is_some_and(|pending| pending.input.tick.0 <= server.0) {
      self.pending.pop_front();
    }
    self.body = body_of(entity);
    for pending in &self.pending {
      self.before = self.body;
      self.body = step(&self.terrain, self.body, pending.walk);
    }
  }

  /// The place of the feet, in metres, `alpha` of the way from the body before the last tick to
  /// the body now. `alpha` is 0 at the start of a tick, and 1 at its end.
  pub fn position(&self, alpha: f32) -> [f32; 3] {
    let [x, y, z] = self.before.position;
    let from = Position { x, y, z }.metres();
    let [x, y, z] = self.body.position;
    let to = Position { x, y, z }.metres();
    let alpha = alpha.clamp(0.0, 1.0);
    [0, 1, 2].map(|axis| from[axis] + (to[axis] - from[axis]) * alpha)
  }

  /// The tick of the last input.
  pub fn tick(&self) -> Tick {
    self.tick
  }
}

/// The body that an entity of a snapshot is. A snapshot gives the speed along the axis y of a
/// body in the centimetres that it moved in the tick, which is within 10 cm/s of the speed that
/// the step keeps: a body that falls lands in the same place, to the centimetre.
fn body_of(entity: &Entity) -> Body {
  let Position { x, y, z } = entity.position;
  Body { position: [x, y, z], vertical_speed: entity.velocity.y }
}

#[cfg(test)]
mod tests {
  use mmo_collide::{Terrain, ground};
  use mmo_protocol::{Angle, CharacterId, Entity, Input, Position, Tick, Velocity};

  use super::{PENDING, Predictor};

  const EAST: Option<Angle> = Some(Angle(0));
  const NORTH: Angle = Angle(16384);

  fn terrain() -> Terrain {
    Terrain::new(&ground()).expect("the ground is valid")
  }

  fn entity(x: i32, y: i32, speed_y: i16) -> Entity {
    Entity {
      id: CharacterId(7),
      position: Position { x, y, z: 10_000 },
      velocity: Velocity { x: 0, y: speed_y, z: 0 },
      view: NORTH,
      animation: 0,
      hit_points: 100,
    }
  }

  fn input(tick: u32, heading: Option<Angle>) -> Input {
    Input { tick: Tick(tick), heading, view: NORTH }
  }

  fn started() -> Predictor {
    Predictor::start(terrain(), Tick(10), &entity(10_000, 0, 0))
  }

  #[test]
  fn each_step_walks_30_cm_and_gives_the_input_of_the_next_tick() {
    let mut predictor = started();
    assert_eq!(predictor.tick(), Tick(13));
    assert_eq!(predictor.step(EAST, NORTH), input(14, EAST));
    assert_eq!(predictor.step(None, NORTH), input(15, None));
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [100.3, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.position(0.0).map(f32::to_bits), [100.3, 0.0, 100.0].map(f32::to_bits));
    predictor.step(EAST, NORTH);
    assert_eq!(predictor.position(0.5).map(f32::to_bits), [100.45, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.position(9.0).map(f32::to_bits), [100.6, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.position(-9.0).map(f32::to_bits), [100.3, 0.0, 100.0].map(f32::to_bits));
  }

  #[test]
  fn a_datagram_holds_the_last_3_inputs_and_pads_with_the_oldest() {
    let mut predictor = started();
    assert_eq!(predictor.recent(), None);
    predictor.step(EAST, NORTH);
    assert_eq!(predictor.recent(), Some([input(14, EAST); 3]));
    predictor.step(None, NORTH);
    assert_eq!(predictor.recent(), Some([input(14, EAST), input(14, EAST), input(15, None)]));
    predictor.step(None, NORTH);
    predictor.step(EAST, NORTH);
    assert_eq!(predictor.recent(), Some([input(15, None), input(16, None), input(17, EAST)]));
  }

  #[test]
  fn a_snapshot_that_agrees_changes_nothing_and_one_that_differs_moves_the_body_again() {
    let mut predictor = started();
    for _ in 0..3 {
      predictor.step(EAST, NORTH);
    }
    predictor.correct(Tick(14), &entity(10_030, 0, 0));
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [100.9, 0.0, 100.0].map(f32::to_bits));
    predictor.correct(Tick(14), &entity(10_000, 0, 0));
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [100.6, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.position(0.0).map(f32::to_bits), [100.3, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.recent(), Some([input(15, EAST), input(15, EAST), input(16, EAST)]));
    predictor.correct(Tick(15), &entity(10_030, 0, 0));
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [100.6, 0.0, 100.0].map(f32::to_bits));
    assert_eq!(predictor.recent(), Some([input(16, EAST); 3]));
  }

  #[test]
  fn a_snapshot_that_the_client_does_not_run_ahead_of_by_1_to_6_ticks_sets_its_clock() {
    let mut predictor = started();
    predictor.step(EAST, NORTH);
    predictor.correct(Tick(14), &entity(20_000, 0, 0));
    assert_eq!(predictor.tick(), Tick(17));
    assert_eq!(predictor.recent(), None);
    assert_eq!(predictor.position(0.0).map(f32::to_bits), [200.0, 0.0, 100.0].map(f32::to_bits));
    predictor.step(EAST, NORTH);
    predictor.correct(Tick(10), &entity(30_000, 0, 0));
    assert_eq!(predictor.tick(), Tick(13));
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [300.0, 0.0, 100.0].map(f32::to_bits));
  }

  #[test]
  fn a_body_that_falls_lands_where_the_step_of_the_writer_lands_it() {
    let mut predictor = Predictor::start(terrain(), Tick(10), &entity(10_000, 998, -49));
    predictor.step(None, NORTH);
    assert_eq!(predictor.position(1.0).map(f32::to_bits), [100.0, 9.93, 100.0].map(f32::to_bits));
    let mut again = Predictor::start(terrain(), Tick(10), &entity(10_000, 998, -49));
    again.step(None, NORTH);
    again.correct(Tick(14), &entity(10_000, 993, -98));
    assert_eq!(again.position(1.0).map(f32::to_bits), [100.0, 9.93, 100.0].map(f32::to_bits));
  }

  #[test]
  fn the_client_keeps_64_inputs_for_a_writer_that_does_not_answer() {
    let mut predictor = started();
    for _ in 0..PENDING + 6 {
      predictor.step(EAST, NORTH);
    }
    assert_eq!(predictor.pending.len(), PENDING);
    assert_eq!(predictor.pending.front().map(|pending| pending.input.tick), Some(Tick(20)));
  }
}
