//! The state of the zone, which its writer alone decides.

use std::collections::BTreeMap;

use mmo_collide::{Body, Solid, Terrain, step};
use mmo_protocol::{
  Angle, CharacterId, Entity, Input, Intents, Link, Position, Relayed, Snapshot, Tick, Velocity,
  ZoneId,
};
use mmo_store::Saved;

use crate::interest::Grid;
use crate::view::{History, Sent};

/// The most players that a zone holds.
const CAP: usize = 2500;

/// The ticks of one second.
const TICKS_PER_SECOND: u32 = 20;

/// The ticks that the zone keeps the player of a link that closed: 30 s.
const KEPT: u32 = 30 * TICKS_PER_SECOND;

/// The zone takes an input whose tick is at most this many ticks after its present tick.
const REACH: u32 = 4;

/// The side of the square of a zone, in metres. Its corner is at the origin.
const SIDE: f32 = 1024.0;

/// The blocks on the ground: the x and the z of the middle of each, and its height, in metres.
/// Each is 4 m wide on the axes x and z.
const BLOCKS: [[f32; 3]; 4] =
  [[500.0, 500.0, 2.0], [540.0, 520.0, 0.3], [470.0, 560.0, 1.0], [600.0, 450.0, 3.0]];

/// The number that the zone writer gives to the link of a gateway, which relays the links of its
/// clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Gateway(pub(crate) u64);

/// Why the zone takes no player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Refusal {
  #[error("the character is in zone {}", zone.0)]
  Elsewhere { zone: ZoneId },
  #[error("the zone holds {CAP} players")]
  Full,
}

/// The state of one zone: its players on the ground, at its present tick.
pub(crate) struct Zone {
  id: ZoneId,
  terrain: Terrain,
  tick: Tick,
  players: BTreeMap<i64, Player>,
  /// The character of each link that the zone takes messages from.
  links: BTreeMap<u64, i64>,
  history: History,
}

struct Player {
  body: Body,
  view: Angle,
  velocity: Velocity,
  hit_points: i32,
  /// The inputs of the next ticks, by their tick.
  inputs: BTreeMap<u32, Input>,
  attachment: Attachment,
}

enum Attachment {
  Linked { gateway: Gateway, link: Link, ack: Option<Tick>, sent: Sent },
  Away { since: Tick },
}

impl Zone {
  pub(crate) fn new(id: ZoneId) -> Self {
    Self {
      id,
      terrain: ground(),
      tick: Tick(0),
      players: BTreeMap::new(),
      links: BTreeMap::new(),
      history: History::new(),
    }
  }

  /// Takes the player of `saved` on `link`. A player that the zone holds already keeps its state,
  /// which is newer than its save, and takes the new link.
  pub(crate) fn join(
    &mut self,
    gateway: Gateway,
    link: Link,
    saved: &Saved,
  ) -> Result<(), Refusal> {
    if saved.zone != self.id {
      return Err(Refusal::Elsewhere { zone: saved.zone });
    }
    let attachment = Attachment::Linked { gateway, link, ack: None, sent: Sent::default() };
    let full = self.players.len() >= CAP;
    match self.players.get_mut(&saved.character.0) {
      Some(player) => {
        if let Attachment::Linked { link: old, .. } = player.attachment {
          self.links.remove(&old.0);
        }
        player.attachment = attachment;
      }
      None if full => return Err(Refusal::Full),
      None => {
        self.players.insert(saved.character.0, Player::new(saved, attachment));
      }
    }
    self.links.insert(link.0, saved.character.0);
    Ok(())
  }

  /// The link of a player closed: the zone keeps the player in the world for 30 s, so its client
  /// comes back to the same state.
  pub(crate) fn leave(&mut self, gateway: Gateway, link: Link) {
    if let Some(player) = self.unlink(gateway, link).and_then(|id| self.players.get_mut(&id)) {
      player.attachment = Attachment::Away { since: self.tick };
    }
  }

  /// The link of the gateway closed: each of its players leaves.
  pub(crate) fn close(&mut self, gateway: Gateway) {
    for player in self.players.values_mut() {
      if let Attachment::Linked { gateway: via, link, .. } = player.attachment
        && via == gateway
      {
        self.links.remove(&link.0);
        player.attachment = Attachment::Away { since: self.tick };
      }
    }
  }

  /// The client of `link` ends the play of its character, which leaves the zone at once.
  pub(crate) fn logout(&mut self, gateway: Gateway, link: Link) {
    if let Some(id) = self.unlink(gateway, link) {
      self.players.remove(&id);
    }
  }

  /// Keeps each input of `intents` for one of the next 4 ticks, when `intents` names this zone and
  /// comes on the link of a player.
  pub(crate) fn intents(&mut self, gateway: Gateway, link: Link, intents: &Intents) {
    let now = self.tick.0;
    let player = self.links.get(&link.0).and_then(|id| self.players.get_mut(id));
    let Some(Player { inputs, attachment: Attachment::Linked { gateway: via, ack, .. }, .. }) =
      player
    else {
      return;
    };
    if *via != gateway || intents.zone != self.id {
      return;
    }
    if let Some(tick) = intents.ack
      && tick.0 <= now
      && ack.is_none_or(|held| held.0 < tick.0)
    {
      *ack = Some(tick);
    }
    for input in intents.inputs {
      if input.tick.0.checked_sub(now).is_some_and(|ahead| (1..=REACH).contains(&ahead)) {
        inputs.entry(input.tick.0).or_insert(input);
      }
    }
  }

  /// Runs one tick: moves each player with its input of this tick, and gives the snapshot of each
  /// player of a link, with the gateway that relays it.
  pub(crate) fn tick(&mut self) -> Vec<(Gateway, Relayed<Snapshot>)> {
    self.tick = Tick(self.tick.0 + 1);
    let now = self.tick;
    self.players.retain(|_, player| {
      !matches!(player.attachment, Attachment::Away { since } if now.0 - since.0 >= KEPT)
    });
    for player in self.players.values_mut() {
      player.step(&self.terrain, now);
    }
    let entities = self.players.iter().map(|(id, player)| player.entity(CharacterId(*id)));
    self.history.push(now, entities.collect());
    let entities = self.history.entities();
    let grid = Grid::new(entities);
    let mut snapshots = Vec::new();
    for (player, entity) in self.players.values_mut().zip(entities) {
      if let Attachment::Linked { gateway, link, ack, sent } = &mut player.attachment {
        let message = sent.snapshot(&self.history, *ack, &grid.visible(entity));
        snapshots.push((*gateway, Relayed { link: *link, message }));
      }
    }
    snapshots
  }

  /// Takes no more messages of `link`, when it is the link of a player on `gateway`, and gives
  /// the character of that player.
  fn unlink(&mut self, gateway: Gateway, link: Link) -> Option<i64> {
    let id = *self.links.get(&link.0)?;
    let ours = self.players.get(&id).is_some_and(|player| {
      matches!(player.attachment, Attachment::Linked { gateway: via, .. } if via == gateway)
    });
    ours.then(|| {
      self.links.remove(&link.0);
      id
    })
  }
}

impl Player {
  fn new(saved: &Saved, attachment: Attachment) -> Self {
    let Position { x, y, z } = saved.state.position;
    Self {
      body: Body { position: [x, y, z], vertical_speed: 0 },
      view: saved.state.yaw,
      velocity: Velocity { x: 0, y: 0, z: 0 },
      hit_points: saved.state.hit_points,
      inputs: BTreeMap::new(),
      attachment,
    }
  }

  /// Moves the body for the tick `now`, with the input of that tick, or with none when none came.
  fn step(&mut self, terrain: &Terrain, now: Tick) {
    let input = self.inputs.remove(&now.0);
    let walk = match input {
      Some(Input { heading: Some(heading), .. }) => mmo_collide::Input::Walk { heading: heading.0 },
      _ => mmo_collide::Input::Idle,
    };
    if let Some(input) = input {
      self.view = input.view;
    }
    let moved = step(terrain, self.body, walk);
    let [x, y, z] = [0, 1, 2].map(|axis| speed(self.body.position[axis], moved.position[axis]));
    self.velocity = Velocity { x, y, z };
    self.body = moved;
  }

  fn entity(&self, id: CharacterId) -> Entity {
    let [x, y, z] = self.body.position;
    Entity {
      id,
      position: Position { x, y, z },
      velocity: self.velocity,
      view: self.view,
      animation: 0,
      hit_points: self.hit_points,
    }
  }
}

/// The speed of a move of one tick from `from` to `to`, in centimetres each second.
#[expect(clippy::expect_used, reason = "a step moves a body by less than 4 m, so 80 m/s at most")]
fn speed(from: i32, to: i32) -> i16 {
  let ticks = i32::try_from(TICKS_PER_SECOND).expect("20 fits in an i32");
  i16::try_from((to - from) * ticks).expect("a step moves a body by less than 4 m")
}

/// The ground of the zone: a flat square, with a few blocks on it.
#[expect(clippy::expect_used, reason = "a ground of boxes names no corner of a mesh")]
fn ground() -> Terrain {
  let half = SIDE / 2.0;
  let floor = Solid::Cuboid { center: [half, -0.5, half], half_extents: [half, 0.5, half] };
  let blocks = BLOCKS.map(|[x, z, top]| Solid::Cuboid {
    center: [x, top / 2.0, z],
    half_extents: [2.0, top / 2.0, 2.0],
  });
  Terrain::new(&[[floor].as_slice(), &blocks].concat()).expect("a ground of boxes is valid")
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Angle, Change, CharacterId, Entity, Input, Intents, Link, Offset, Position, Relayed, Snapshot,
    Tick, Velocity, ZoneId,
  };
  use mmo_store::{Saved, State};

  use super::{CAP, Gateway, KEPT, Refusal, Zone};

  const MEADOW: ZoneId = ZoneId(1);
  const GATE: Gateway = Gateway(1);
  const EAST: Option<Angle> = Some(Angle(0));
  const NORTH: Angle = Angle(16384);

  fn saved(id: i64, x: i32) -> Saved {
    Saved {
      character: CharacterId(id),
      zone: MEADOW,
      state: State { position: Position { x, y: 0, z: 10_000 }, yaw: Angle(0), hit_points: 100 },
      version: 0,
    }
  }

  fn entity(id: i64, x: i32, speed: i16, view: Angle) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z: 10_000 },
      velocity: Velocity { x: speed, y: 0, z: 0 },
      view,
      animation: 0,
      hit_points: 100,
    }
  }

  fn intents(ack: Option<u32>, ticks: [u32; 3], heading: Option<Angle>) -> Intents {
    Intents {
      zone: MEADOW,
      ack: ack.map(Tick),
      inputs: ticks.map(|tick| Input { tick: Tick(tick), heading, view: NORTH }),
    }
  }

  fn full(link: u64, tick: u32, entered: Vec<Entity>) -> (Gateway, Relayed<Snapshot>) {
    let message = Snapshot { tick: Tick(tick), base: None, entered, changed: vec![], left: vec![] };
    (GATE, Relayed { link: Link(link), message })
  }

  fn run(zone: &mut Zone, ticks: u32) -> Vec<(Gateway, Relayed<Snapshot>)> {
    (1..ticks).for_each(|_| drop(zone.tick()));
    zone.tick()
  }

  #[test]
  fn a_player_walks_with_the_inputs_of_its_link_and_its_client_gets_each_delta() {
    let mut zone = Zone::new(MEADOW);
    assert_eq!(zone.join(GATE, Link(1), &saved(7, 10_000)), Ok(()));
    zone.intents(GATE, Link(1), &intents(None, [1, 2, 3], EAST));
    assert_eq!(zone.tick(), [full(1, 1, vec![entity(7, 10_030, 600, NORTH)])]);
    zone.intents(GATE, Link(1), &intents(Some(1), [2, 3, 4], EAST));
    let change = Change {
      id: CharacterId(7),
      movement: Offset { x: 90, y: 0, z: 0 },
      velocity: Velocity { x: 600, y: 0, z: 0 },
      view: NORTH,
      animation: 0,
      hit_points: 100,
    };
    let delta = Snapshot {
      tick: Tick(4),
      base: Some(Tick(1)),
      entered: vec![],
      changed: vec![change],
      left: vec![],
    };
    assert_eq!(run(&mut zone, 3), [(GATE, Relayed { link: Link(1), message: delta })]);
    assert_eq!(run(&mut zone, 1)[0].1.message.changed[0].velocity, Velocity { x: 0, y: 0, z: 0 });
  }

  #[test]
  fn the_zone_takes_only_the_inputs_of_the_next_4_ticks_on_the_link_of_a_player() {
    let mut zone = Zone::new(MEADOW);
    assert_eq!(zone.join(GATE, Link(1), &saved(7, 10_000)), Ok(()));
    let elsewhere = Intents { zone: ZoneId(2), ..intents(None, [1, 2, 3], EAST) };
    zone.intents(GATE, Link(1), &elsewhere);
    zone.intents(GATE, Link(2), &intents(None, [1, 2, 3], EAST));
    zone.intents(Gateway(2), Link(1), &intents(None, [1, 2, 3], EAST));
    zone.intents(GATE, Link(1), &intents(Some(9), [0, 5, 6], EAST));
    assert_eq!(run(&mut zone, 6), [full(1, 6, vec![entity(7, 10_000, 0, Angle(0))])]);
  }

  #[test]
  fn the_zone_refuses_a_character_of_another_zone_and_a_player_past_its_cap() {
    let mut zone = Zone::new(MEADOW);
    let elsewhere = Saved { zone: ZoneId(2), ..saved(1, 0) };
    assert_eq!(zone.join(GATE, Link(1), &elsewhere), Err(Refusal::Elsewhere { zone: ZoneId(2) }));
    for id in 0..CAP {
      let id = i64::try_from(id).unwrap();
      assert_eq!(zone.join(GATE, Link(id.unsigned_abs()), &saved(id, 0)), Ok(()));
    }
    let refused = zone.join(GATE, Link(u64::MAX), &saved(-1, 0)).map_err(|why| why.to_string());
    assert_eq!(refused, Err("the zone holds 2500 players".to_owned()));
    let elsewhere = Refusal::Elsewhere { zone: ZoneId(2) }.to_string();
    assert_eq!(elsewhere, "the character is in zone 2");
  }

  #[test]
  fn a_player_whose_link_closes_stays_30_s_and_comes_back_to_its_state() {
    let mut zone = Zone::new(MEADOW);
    assert_eq!(zone.join(GATE, Link(1), &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), &saved(8, 10_100)), Ok(()));
    zone.intents(GATE, Link(2), &intents(None, [1, 2, 3], EAST));
    zone.tick();
    zone.leave(Gateway(2), Link(2));
    zone.leave(GATE, Link(2));
    zone.leave(GATE, Link(2));
    let both = vec![entity(7, 10_000, 0, Angle(0)), entity(8, 10_190, 0, NORTH)];
    assert_eq!(run(&mut zone, KEPT - 1), [full(1, KEPT, both.clone())]);
    assert_eq!(zone.join(GATE, Link(3), &saved(8, 0)), Ok(()));
    let back = run(&mut zone, 1);
    let mirrored = vec![both[1], both[0]];
    assert_eq!(back, [full(1, KEPT + 1, both), full(3, KEPT + 1, mirrored)]);
    assert_eq!(zone.join(GATE, Link(4), &saved(8, 0)), Ok(()));
    zone.intents(GATE, Link(3), &intents(None, [KEPT + 2; 3], EAST));
    zone.close(GATE);
    assert_eq!(run(&mut zone, KEPT - 1), []);
    zone.close(GATE);
    assert_eq!(zone.join(GATE, Link(5), &saved(9, 10_000)), Ok(()));
    assert_eq!(run(&mut zone, 1), [full(5, 2 * KEPT + 1, vec![entity(9, 10_000, 0, Angle(0))])]);
  }

  #[test]
  fn a_player_that_logs_out_leaves_the_zone_at_once() {
    let mut zone = Zone::new(MEADOW);
    assert_eq!(zone.join(GATE, Link(1), &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), &saved(8, 10_100)), Ok(()));
    zone.logout(Gateway(2), Link(2));
    zone.logout(GATE, Link(3));
    zone.logout(GATE, Link(2));
    assert_eq!(zone.tick(), [full(1, 1, vec![entity(7, 10_000, 0, Angle(0))])]);
  }

  /// The block from x = 498 m to x = 502 m stops the capsule of the body a little before it.
  #[test]
  fn a_block_of_the_ground_stops_a_player() {
    let mut zone = Zone::new(MEADOW);
    let start = Saved {
      state: State { position: Position { x: 49_000, y: 0, z: 50_000 }, ..saved(7, 0).state },
      ..saved(7, 0)
    };
    assert_eq!(zone.join(GATE, Link(1), &start), Ok(()));
    let mut last = Vec::new();
    for tick in 0..40 {
      zone.intents(GATE, Link(1), &intents(None, [tick + 1, tick + 2, tick + 3], EAST));
      last = zone.tick();
    }
    let stopped =
      Entity { position: Position { x: 49_757, y: 0, z: 50_000 }, ..entity(7, 0, 0, NORTH) };
    assert_eq!(last, [full(1, 40, vec![stopped])]);
  }
}
