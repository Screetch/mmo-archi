//! The game of the player: the world that the zone writer shows, the prediction of the body of the
//! player, and the camera.

use glam::{Mat4, Vec3};
use mmo_collide::{DT, Terrain, ground};
use mmo_protocol::{
  Angle, CharacterId, Entity, Intents, Layer, Position, Snapshot, Tick, ToClient, Velocity, ZoneId,
};
use mmo_render::Placed;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{Camera, Direction, Down, Keys, Predictor, Up, World};

/// The longest time that one update runs ticks for, in seconds. A pause that is longer, as the
/// drag of a window, drops the rest: a burst of ticks would send inputs that the writer refuses.
const LONGEST: f32 = 0.25;

/// What the window draws in one frame.
#[derive(Clone, Debug)]
pub struct Scene {
  /// The matrix that takes a point of the world to the clip space of wgpu.
  pub view_projection: Mat4,
  /// The place of the eye, in metres.
  pub eye: Vec3,
  /// Each character that the player sees, its own included.
  pub characters: Vec<Placed>,
}

/// The far end of a game that no server serves: it keeps the channels of the game open.
pub struct Wire {
  /// What the game sends up.
  pub up: UnboundedReceiver<Up>,
  /// Where a message for the game goes in.
  pub down: UnboundedSender<Down>,
}

/// The game of one player. The window gives it the keys and the pointer, and calls
/// [`Game::update`] and [`Game::scene`] for each frame. The game decides nothing: it predicts the
/// body of its player and shows what the zone writer says.
pub struct Game {
  character: CharacterId,
  world: World,
  predictor: Option<Predictor>,
  /// The layer of the player, which the gateway names first.
  layer: Option<Layer>,
  camera: Camera,
  keys: Keys,
  up: UnboundedSender<Up>,
  down: UnboundedReceiver<Down>,
  /// The time since the last tick of the prediction, in seconds.
  owed: f32,
  /// The time since the last snapshot, in seconds.
  since: f32,
  /// The time since the start, in seconds, which sets the clip of each character.
  clock: f32,
  over: bool,
}

#[expect(clippy::expect_used, reason = "the ground of the game is a list of valid boxes")]
fn terrain() -> Terrain {
  Terrain::new(&ground()).expect("the ground is valid")
}

impl Game {
  /// The game of `character`, which sends to `up` and reads from `down`.
  pub fn new(
    character: CharacterId,
    up: UnboundedSender<Up>,
    down: UnboundedReceiver<Down>,
  ) -> Self {
    Self {
      character,
      world: World::default(),
      predictor: None,
      layer: None,
      camera: Camera::default(),
      keys: Keys::default(),
      up,
      down,
      owed: 0.0,
      since: 0.0,
      clock: 0.0,
      over: false,
    }
  }

  /// A game with no server: the zone has a player and two others, who stand still. It shows the
  /// drawing and the controls with no stack. Keep the [`Wire`] as long as the game runs.
  #[expect(clippy::expect_used, reason = "the game holds the other end of each channel")]
  pub fn demo() -> (Self, Wire) {
    let (up, from) = unbounded_channel();
    let (to, down) = unbounded_channel();
    let standing = |id, x, z| Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    };
    let first = Snapshot {
      tick: Tick(10),
      base: None,
      entered: vec![
        standing(1, 49_000, 49_000),
        standing(2, 49_400, 48_700),
        standing(3, 48_700, 49_500),
      ],
      changed: vec![],
      left: vec![],
    };
    to.send(Down::Message(ToClient::Moved { zone: ZoneId(1), layer: 1 }))
      .expect("the game listens");
    to.send(Down::Snapshot(first)).expect("the game listens");
    (Self::new(CharacterId(1), up, down), Wire { up: from, down: to })
  }

  /// Puts a movement key down or up.
  pub fn key(&mut self, direction: Direction, down: bool) {
    self.keys.set(direction, down);
  }

  /// Lets go of each key, as when the window loses the focus.
  pub fn release(&mut self) {
    self.keys = Keys::default();
  }

  /// Turns the camera by a move of the pointer, in pixels.
  pub fn orbit(&mut self, dx: f32, dy: f32) {
    self.camera.orbit(dx, dy);
  }

  /// Zooms the camera by steps of the wheel.
  pub fn zoom(&mut self, steps: f32) {
    self.camera.zoom(steps);
  }

  /// True when the link ended, or the server refused the session: the window closes.
  pub fn over(&self) -> bool {
    self.over
  }

  /// Takes what the server sent, runs the ticks that `dt` seconds make, and sends the intents of
  /// each tick.
  pub fn update(&mut self, dt: f32) {
    self.clock += dt;
    self.since += dt;
    self.receive();
    self.owed = (self.owed + dt).min(LONGEST);
    while self.owed >= DT {
      self.owed -= DT;
      self.tick();
    }
    if let Some(predictor) = &self.predictor {
      self.camera.follow(predictor.position(self.owed / DT));
    }
  }

  /// The frame to draw, for a window whose width is `aspect` times its height.
  pub fn scene(&self, aspect: f32) -> Scene {
    let alpha = (self.since / DT).min(1.0);
    let mut characters: Vec<Placed> = self
      .world
      .seen(alpha)
      .into_iter()
      .filter(|seen| seen.id != self.character)
      .map(|seen| Placed {
        position: Vec3::from(seen.position),
        yaw: seen.view,
        seconds: self.clock,
      })
      .collect();
    if let Some(predictor) = &self.predictor {
      let position = Vec3::from(predictor.position(self.owed / DT));
      characters.push(Placed { position, yaw: self.camera.yaw(), seconds: self.clock });
    }
    Scene {
      view_projection: self.camera.view_projection(aspect),
      eye: self.camera.eye(),
      characters,
    }
  }

  fn receive(&mut self) {
    loop {
      match self.down.try_recv() {
        Ok(Down::Snapshot(snapshot)) => self.snapshot(&snapshot),
        Ok(Down::Message(message)) => self.message(&message),
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
          self.over = true;
          return;
        }
      }
    }
  }

  /// A snapshot that the world refuses, as a late one, changes nothing: the next one comes in 50
  /// ms, and a missing base makes the writer send a full snapshot.
  fn snapshot(&mut self, snapshot: &Snapshot) {
    if self.world.apply(snapshot).is_err() {
      return;
    }
    self.since = 0.0;
    let Some(entity) = self.world.entity(self.character).copied() else {
      return;
    };
    match &mut self.predictor {
      Some(predictor) => predictor.correct(snapshot.tick, &entity),
      None => self.predictor = Some(Predictor::start(terrain(), snapshot.tick, &entity)),
    }
  }

  fn message(&mut self, message: &ToClient) {
    match message {
      ToClient::Moved { zone, layer } => {
        self.layer = Some(Layer { zone: *zone, number: *layer });
        self.world.clear();
        self.predictor = None;
      }
      ToClient::NoSession => self.over = true,
      ToClient::Full { .. } | ToClient::Line { .. } => {}
    }
  }

  /// One tick of 50 ms: predict the step, and send the last 3 inputs.
  fn tick(&mut self) {
    let (Some(predictor), Some(Layer { zone, number: layer })) = (&mut self.predictor, self.layer)
    else {
      return;
    };
    let yaw = self.camera.yaw();
    predictor.step(self.keys.heading(yaw), Angle::from_radians(yaw));
    let ack = self.world.ack();
    let intents = predictor.recent().map(|inputs| Intents { zone, layer, ack, inputs });
    if intents.is_some_and(|intents| self.up.send(Up::Intents(intents)).is_err()) {
      self.over = true;
    }
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Angle, CharacterId, Entity, Intents, Position, Snapshot, Tick, ToClient, Velocity, ZoneId,
  };
  use tokio::sync::mpsc::unbounded_channel;

  use super::{Game, LONGEST, Wire};
  use crate::{Direction, Down, Up};

  const ME: CharacterId = CharacterId(1);

  fn entity(id: i64, x: i32) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z: 10_000 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    }
  }

  fn snapshot(tick: u32, entered: Vec<Entity>) -> Snapshot {
    Snapshot { tick: Tick(tick), base: None, entered, changed: vec![], left: vec![] }
  }

  fn fresh() -> (Game, Wire) {
    let (up, from) = unbounded_channel();
    let (to, down) = unbounded_channel();
    (Game::new(ME, up, down), Wire { up: from, down: to })
  }

  fn sent(wire: &mut Wire) -> Vec<Intents> {
    let mut all = Vec::new();
    while let Ok(Up::Intents(intents)) = wire.up.try_recv() {
      all.push(intents);
    }
    all
  }

  /// A game in the zone 1 that shows the player at 100 m and another character at 120 m.
  fn playing() -> (Game, Wire) {
    let (mut game, wire) = fresh();
    wire.down.send(Down::Message(ToClient::Moved { zone: ZoneId(1), layer: 1 })).expect("sent");
    wire
      .down
      .send(Down::Snapshot(snapshot(10, vec![entity(1, 10_000), entity(2, 12_000)])))
      .expect("sent");
    game.update(0.0);
    (game, wire)
  }

  #[test]
  fn a_snapshot_starts_the_prediction_and_each_tick_sends_the_last_inputs() {
    let (mut game, mut wire) = playing();
    assert!(sent(&mut wire).is_empty());
    game.key(Direction::Forward, true);
    game.update(0.06);
    let ones = sent(&mut wire);
    assert_eq!(ones.len(), 1);
    let one = ones[0];
    assert_eq!((one.zone, one.layer, one.ack), (ZoneId(1), 1, Some(Tick(10))));
    assert_eq!(one.inputs[2].tick, Tick(14));
    assert_eq!(one.inputs[2].heading, Some(Angle(0)));
    game.key(Direction::Forward, false);
    game.update(0.05);
    let twos = sent(&mut wire);
    assert_eq!(twos.len(), 1);
    let two = twos[0];
    assert_eq!((two.inputs[2].tick, two.inputs[2].heading), (Tick(15), None));
  }

  #[test]
  fn the_scene_shows_the_player_where_it_walks_and_each_other_character_where_the_snapshot_says() {
    let (mut game, _wire) = playing();
    let scene = game.scene(16.0 / 9.0);
    let places: Vec<_> = scene.characters.iter().map(|placed| placed.position.to_array()).collect();
    assert_eq!(places, [[120.0, 0.0, 100.0], [100.0, 0.0, 100.0]]);
    game.key(Direction::Forward, true);
    game.update(0.1);
    let walked = game.scene(1.0).characters[1].position;
    assert!(walked.x > 100.2, "{walked}");
    assert!((scene.eye - game.scene(1.0).eye).length() > 0.2);
  }

  #[test]
  fn a_new_snapshot_corrects_the_prediction_and_a_refused_one_changes_nothing() {
    let (mut game, wire) = playing();
    wire.down.send(Down::Snapshot(snapshot(11, vec![entity(1, 10_500)]))).expect("sent");
    game.update(0.05);
    assert!((game.scene(1.0).characters[0].position.x - 105.0).abs() < 0.5);
    let late = Down::Snapshot(snapshot(11, vec![entity(1, 30_000)]));
    let unknown = Snapshot { base: Some(Tick(99)), ..snapshot(12, vec![entity(1, 30_000)]) };
    for refused in [late, Down::Snapshot(unknown)] {
      wire.down.send(refused).expect("sent");
    }
    game.update(0.05);
    assert!((game.scene(1.0).characters[0].position.x - 105.0).abs() < 0.5);
    wire.down.send(Down::Snapshot(snapshot(13, vec![entity(2, 12_000)]))).expect("sent");
    game.update(0.05);
    let seen = game.scene(1.0).characters;
    assert_eq!(seen.len(), 2);
    assert!((seen[0].position.x - 120.0).abs() < 0.5 && (seen[1].position.x - 105.0).abs() < 0.5);
  }

  #[test]
  fn a_move_to_another_zone_starts_the_world_again_and_the_other_messages_wait_their_turn() {
    let (mut game, mut wire) = playing();
    wire.down.send(Down::Message(ToClient::Moved { zone: ZoneId(2), layer: 3 })).expect("sent");
    for message in [
      ToClient::Full { zone: ZoneId(3) },
      ToClient::Line { channel: "world".to_owned(), from: "Ada".to_owned(), text: "hi".to_owned() },
    ] {
      wire.down.send(Down::Message(message)).expect("sent");
    }
    game.update(0.2);
    assert!(game.scene(1.0).characters.is_empty());
    assert!(!game.over());
    assert!(sent(&mut wire).is_empty());
    wire.down.send(Down::Snapshot(snapshot(3, vec![entity(1, 10_000)]))).expect("sent");
    game.update(0.05);
    let moved = sent(&mut wire)[0];
    assert_eq!((moved.zone, moved.layer), (ZoneId(2), 3));
  }

  #[test]
  fn the_game_ends_when_the_session_is_refused_the_link_ends_or_the_writer_is_gone() {
    let (mut refused, wire) = fresh();
    wire.down.send(Down::Message(ToClient::NoSession)).expect("sent");
    refused.update(0.0);
    assert!(refused.over());
    let (mut closed, wire) = fresh();
    drop(wire.down);
    closed.update(0.0);
    assert!(closed.over());
    let (mut deaf, wire) = playing();
    drop(wire.up);
    deaf.update(0.06);
    assert!(deaf.over());
  }

  #[test]
  fn a_long_pause_runs_a_few_ticks_and_the_player_without_an_entity_has_no_body() {
    let (mut game, mut wire) = playing();
    game.update(30.0);
    let ticks = sent(&mut wire).len();
    assert!((4..=5).contains(&ticks), "{ticks} ticks");
    assert!(f32::from(u8::try_from(ticks).expect("a count")) * 0.05 <= LONGEST + 0.05);
    let (mut absent, wire) = fresh();
    wire.down.send(Down::Message(ToClient::Moved { zone: ZoneId(1), layer: 1 })).expect("sent");
    wire.down.send(Down::Snapshot(snapshot(4, vec![entity(2, 12_000)]))).expect("sent");
    absent.update(0.2);
    assert_eq!(absent.scene(1.0).characters.len(), 1);
    assert!(sent(&mut { wire }).is_empty());
  }

  #[test]
  fn the_pointer_and_the_wheel_move_the_camera_and_a_lost_focus_lets_go_of_the_keys() {
    let (mut game, mut wire) = playing();
    let before = game.scene(1.0).eye;
    game.orbit(100.0, 0.0);
    let turned = game.scene(1.0).eye;
    assert!((turned - before).length() > 1.0);
    game.zoom(5.0);
    assert!((game.scene(1.0).eye - turned).length() > 0.5);
    game.key(Direction::Forward, true);
    game.release();
    game.update(0.06);
    assert_eq!(sent(&mut wire)[0].inputs[2].heading, None);
  }

  #[test]
  fn the_demo_shows_three_characters_with_no_server() {
    let (mut game, mut wire) = Game::demo();
    game.update(0.0);
    assert_eq!(game.scene(1.0).characters.len(), 3);
    game.update(0.05);
    assert!(!game.over());
    assert_eq!(sent(&mut wire).len(), 1);
  }
}
