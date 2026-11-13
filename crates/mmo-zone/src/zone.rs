//! The state of one layer of a zone, which its writer alone decides.

use std::collections::BTreeMap;
use std::mem;

use mmo_collide::{Body, Terrain, step};
use mmo_protocol::{
  AccountId, Angle, CharacterId, Entity, Input, Intents, Layer, Link, Position, Relayed, Save,
  Snapshot, State, TICKS_PER_SECOND, Tick, Velocity, ZoneId,
};
use mmo_store::Saved;

use crate::interest::Grid;
use crate::view::{History, Sent};

/// The ticks that the zone keeps the player of a link that closed: 30 s.
const KEPT: u32 = 30 * TICKS_PER_SECOND;

/// The zone takes an input whose tick is at most this many ticks after its present tick.
const REACH: u32 = 4;

/// The number that the zone writer gives to the link of a gateway, which relays the links of its
/// clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Gateway(pub(crate) u64);

/// Why the zone takes no player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Refusal {
  #[error("the character is in zone {}", zone.0)]
  Elsewhere { zone: ZoneId },
  #[error("the layer holds {cap} players, its cap")]
  Full { cap: usize },
}

/// A player that left the zone, and the save that its last state needs. The writer keeps the
/// presence of the account until the store acknowledges that save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Leaver {
  pub(crate) account: AccountId,
  /// None when the store holds the last state already.
  pub(crate) save: Option<Save>,
}

/// The state of one layer of a zone: its players on the ground, at its present tick.
pub(crate) struct Zone {
  layer: Layer,
  /// The most players of the layer, which the writer read from its shard when it claimed it.
  cap: usize,
  terrain: Terrain,
  tick: Tick,
  players: BTreeMap<i64, Player>,
  /// The character of each link that the zone takes messages from.
  links: BTreeMap<u64, i64>,
  history: History,
  left: Vec<Leaver>,
}

struct Player {
  account: AccountId,
  /// The state that the store acknowledged last.
  saved: State,
  /// The version of the last save that the zone sent, which each new save passes by 1.
  version: i64,
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
  pub(crate) fn new(layer: Layer, cap: usize) -> Self {
    Self {
      layer,
      cap,
      terrain: ground(),
      tick: Tick(0),
      players: BTreeMap::new(),
      links: BTreeMap::new(),
      history: History::new(),
      left: Vec::new(),
    }
  }

  /// The count of the players that the zone holds, linked or not.
  pub(crate) fn count(&self) -> usize {
    self.players.len()
  }

  /// The account and the character of each player that has a link.
  pub(crate) fn present(&self) -> Vec<(AccountId, CharacterId)> {
    let linked = |player: &&Player| matches!(player.attachment, Attachment::Linked { .. });
    let players = self.players.iter().map(|(id, player)| (player, id));
    players
      .filter(|(player, _)| linked(player))
      .map(|(p, id)| (p.account, CharacterId(*id)))
      .collect()
  }

  /// The save of each player whose state the store does not hold, each with the next version of
  /// its player. A save that the store does not acknowledge comes again with a greater version.
  pub(crate) fn dirty(&mut self) -> Vec<Save> {
    self.players.iter_mut().filter_map(|(id, player)| player.unsaved(*id)).collect()
  }

  /// Notes that the store holds `saves`, so [`Zone::dirty`] gives a player again only after it
  /// moved.
  pub(crate) fn saved(&mut self, saves: &[Save]) {
    for save in saves {
      if let Some(player) = self.players.get_mut(&save.character.0) {
        player.saved = save.state;
      }
    }
  }

  /// The players that left since the last call.
  pub(crate) fn leavers(&mut self) -> Vec<Leaver> {
    mem::take(&mut self.left)
  }

  /// Takes the player of `saved` on `link`. A player that the zone holds already keeps its state,
  /// which is newer than its save, and takes the new link.
  pub(crate) fn join(
    &mut self,
    gateway: Gateway,
    link: Link,
    account: AccountId,
    saved: &Saved,
  ) -> Result<(), Refusal> {
    if saved.zone != self.layer.zone {
      return Err(Refusal::Elsewhere { zone: saved.zone });
    }
    let attachment = Attachment::Linked { gateway, link, ack: None, sent: Sent::default() };
    let full = self.players.len() >= self.cap;
    match self.players.get_mut(&saved.character.0) {
      Some(player) => {
        if let Attachment::Linked { link: old, .. } = player.attachment {
          self.links.remove(&old.0);
        }
        player.attachment = attachment;
      }
      None if full => return Err(Refusal::Full { cap: self.cap }),
      None => {
        self.players.insert(saved.character.0, Player::new(account, saved, attachment));
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
      self.drop_player(id);
    }
  }

  fn drop_player(&mut self, id: i64) {
    let left = self.players.remove(&id).into_iter();
    self
      .left
      .extend(left.map(|mut player| Leaver { account: player.account, save: player.unsaved(id) }));
  }

  /// Keeps each input of `intents` for one of the next 4 ticks, when `intents` names this layer
  /// and comes on the link of a player.
  pub(crate) fn intents(&mut self, gateway: Gateway, link: Link, intents: &Intents) {
    let now = self.tick.0;
    let player = self.links.get(&link.0).and_then(|id| self.players.get_mut(id));
    let Some(Player { inputs, attachment: Attachment::Linked { gateway: via, ack, .. }, .. }) =
      player
    else {
      return;
    };
    if *via != gateway || intents.zone != self.layer.zone || intents.layer != self.layer.number {
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
    let expired = |player: &&Player| matches!(player.attachment, Attachment::Away { since } if now.0 - since.0 >= KEPT);
    let gone: Vec<i64> =
      self.players.iter().filter(|(_, p)| expired(p)).map(|(id, _)| *id).collect();
    for id in gone {
      self.drop_player(id);
    }
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
  fn new(account: AccountId, saved: &Saved, attachment: Attachment) -> Self {
    let Position { x, y, z } = saved.state.position;
    Self {
      account,
      saved: saved.state,
      version: saved.version,
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

  fn state(&self) -> State {
    let [x, y, z] = self.body.position;
    State { position: Position { x, y, z }, yaw: self.view, hit_points: self.hit_points }
  }

  fn unsaved(&mut self, id: i64) -> Option<Save> {
    let state = self.state();
    (state != self.saved).then(|| {
      self.version += 1;
      Save { character: CharacterId(id), state, version: self.version }
    })
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

/// The ground of the zone, which each client builds in the same way.
#[expect(clippy::expect_used, reason = "a ground of boxes names no corner of a mesh")]
fn ground() -> Terrain {
  Terrain::new(&mmo_collide::ground()).expect("a ground of boxes is valid")
}

#[cfg(test)]
mod tests {
  use mmo_protocol::Layer;
  use mmo_protocol::{
    AccountId, Angle, Change, CharacterId, Entity, Input, Intents, Link, Offset, Position, Relayed,
    Save, Snapshot, State, TICKS_PER_SECOND, Tick, Velocity, ZoneId, encode,
  };
  use mmo_store::Saved;

  use super::{Gateway, KEPT, Leaver, Refusal, Zone};

  const MEADOW: ZoneId = ZoneId(1);
  const LAYER: Layer = Layer { zone: MEADOW, number: 1 };
  /// The cap of a layer at the start of a shard.
  const CAP: usize = 500;
  const ACCOUNT: AccountId = AccountId(3);
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
      layer: 1,
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

  /// 500 players as the load test seeds them: 250 in one square of 128 m and 250 anywhere in the
  /// zone. Each picks one of 8 headings, or a stop, each second, and acknowledges the snapshot of
  /// 3 ticks before.
  #[test]
  fn a_crowd_that_walks_gets_snapshots_of_74_bytes_on_average() {
    let mut zone = Zone::new(LAYER, CAP);
    let mut seed = 9_u64;
    let mut below = |n: u64| {
      seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
      (seed >> 33) % n
    };
    for id in 1..=500 {
      let (origin, span) = if id <= 250 { (10_000, 12_800) } else { (1_000, 100_400) };
      let mut at = || origin + i32::try_from(below(span)).unwrap();
      let mut one = saved(id, at());
      one.state.position.z = at();
      assert_eq!(zone.join(GATE, Link(id.unsigned_abs()), ACCOUNT, &one), Ok(()));
    }
    let mut headings = [None; 500];
    let (mut bytes, mut snapshots) = (0, 0);
    for tick in 1..=120 {
      for (link, heading) in (1..).zip(&mut headings) {
        if tick % TICKS_PER_SECOND == 1 {
          *heading = u16::try_from(below(9) * 8192).ok().map(Angle);
        }
        let ack = tick.checked_sub(3).filter(|ack| *ack > 0);
        let input =
          Input { tick: Tick(tick), heading: *heading, view: heading.unwrap_or(Angle(0)) };
        zone.intents(
          GATE,
          Link(link),
          &Intents { zone: MEADOW, layer: 1, ack: ack.map(Tick), inputs: [input; 3] },
        );
      }
      let sent = zone.tick();
      if tick > 20 {
        bytes += sent.iter().map(|(_, relayed)| encode(&relayed.message).len()).sum::<usize>();
        snapshots += sent.len();
      }
    }
    assert_eq!(bytes / snapshots, 74);
  }

  #[test]
  fn a_player_walks_with_the_inputs_of_its_link_and_its_client_gets_each_delta() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    zone.intents(GATE, Link(1), &intents(None, [1, 2, 3], EAST));
    assert_eq!(zone.tick(), [full(1, 1, vec![entity(7, 10_030, 600, NORTH)])]);
    zone.intents(GATE, Link(1), &intents(Some(1), [2, 3, 4], EAST));
    let straight = Snapshot {
      tick: Tick(4),
      base: Some(Tick(1)),
      entered: vec![],
      changed: vec![],
      left: vec![],
    };
    assert_eq!(run(&mut zone, 3), [(GATE, Relayed { link: Link(1), message: straight })]);
    let stop = Change {
      id: CharacterId(7),
      movement: Offset { x: -30, y: 0, z: 0 },
      velocity: Some(Velocity { x: 0, y: 0, z: 0 }),
      view: None,
      animation: None,
      hit_points: None,
    };
    assert_eq!(run(&mut zone, 1)[0].1.message.changed, [stop]);
  }

  #[test]
  fn the_zone_takes_only_the_inputs_of_the_next_4_ticks_on_the_link_of_a_player() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    let elsewhere = Intents { zone: ZoneId(2), ..intents(None, [1, 2, 3], EAST) };
    zone.intents(GATE, Link(1), &elsewhere);
    let other_layer = Intents { layer: 2, ..intents(None, [1, 2, 3], EAST) };
    zone.intents(GATE, Link(1), &other_layer);
    zone.intents(GATE, Link(2), &intents(None, [1, 2, 3], EAST));
    zone.intents(Gateway(2), Link(1), &intents(None, [1, 2, 3], EAST));
    zone.intents(GATE, Link(1), &intents(Some(9), [0, 5, 6], EAST));
    assert_eq!(run(&mut zone, 6), [full(1, 6, vec![entity(7, 10_000, 0, Angle(0))])]);
  }

  #[test]
  fn the_layer_refuses_a_character_of_another_zone_and_a_player_past_its_cap() {
    let mut zone = Zone::new(LAYER, 3);
    let elsewhere = Saved { zone: ZoneId(2), ..saved(1, 0) };
    assert_eq!(
      zone.join(GATE, Link(1), ACCOUNT, &elsewhere),
      Err(Refusal::Elsewhere { zone: ZoneId(2) })
    );
    for id in 0_i64..3 {
      assert_eq!(zone.join(GATE, Link(id.unsigned_abs()), ACCOUNT, &saved(id, 0)), Ok(()));
    }
    let refused =
      zone.join(GATE, Link(u64::MAX), ACCOUNT, &saved(-1, 0)).map_err(|why| why.to_string());
    assert_eq!(refused, Err("the layer holds 3 players, its cap".to_owned()));
    let elsewhere = Refusal::Elsewhere { zone: ZoneId(2) }.to_string();
    assert_eq!(elsewhere, "the character is in zone 2");
  }

  #[test]
  fn a_player_whose_link_closes_stays_30_s_and_comes_back_to_its_state() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), ACCOUNT, &saved(8, 10_100)), Ok(()));
    zone.intents(GATE, Link(2), &intents(None, [1, 2, 3], EAST));
    zone.tick();
    zone.leave(Gateway(2), Link(2));
    zone.leave(GATE, Link(2));
    zone.leave(GATE, Link(2));
    let both = vec![entity(7, 10_000, 0, Angle(0)), entity(8, 10_190, 0, NORTH)];
    assert_eq!(run(&mut zone, KEPT - 1), [full(1, KEPT, both.clone())]);
    assert_eq!(zone.join(GATE, Link(3), ACCOUNT, &saved(8, 0)), Ok(()));
    let back = run(&mut zone, 1);
    let mirrored = vec![both[1], both[0]];
    assert_eq!(back, [full(1, KEPT + 1, both), full(3, KEPT + 1, mirrored)]);
    assert_eq!(zone.join(GATE, Link(4), ACCOUNT, &saved(8, 0)), Ok(()));
    zone.intents(GATE, Link(3), &intents(None, [KEPT + 2; 3], EAST));
    zone.close(GATE);
    assert_eq!(run(&mut zone, KEPT - 1), []);
    zone.close(GATE);
    assert_eq!(zone.join(GATE, Link(5), ACCOUNT, &saved(9, 10_000)), Ok(()));
    assert_eq!(run(&mut zone, 1), [full(5, 2 * KEPT + 1, vec![entity(9, 10_000, 0, Angle(0))])]);
  }

  #[test]
  fn a_player_that_logs_out_leaves_the_zone_at_once() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), ACCOUNT, &saved(8, 10_100)), Ok(()));
    zone.logout(Gateway(2), Link(2));
    zone.logout(GATE, Link(3));
    zone.logout(GATE, Link(2));
    assert_eq!(zone.tick(), [full(1, 1, vec![entity(7, 10_000, 0, Angle(0))])]);
  }

  /// The block from x = 498 m to x = 502 m stops the capsule of the body a little before it.
  #[test]
  fn a_block_of_the_ground_stops_a_player() {
    let mut zone = Zone::new(LAYER, CAP);
    let start = Saved {
      state: State { position: Position { x: 49_000, y: 0, z: 50_000 }, ..saved(7, 0).state },
      ..saved(7, 0)
    };
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &start), Ok(()));
    let mut last = Vec::new();
    for tick in 0..40 {
      zone.intents(GATE, Link(1), &intents(None, [tick + 1, tick + 2, tick + 3], EAST));
      last = zone.tick();
    }
    let stopped =
      Entity { position: Position { x: 49_757, y: 0, z: 50_000 }, ..entity(7, 0, 0, NORTH) };
    assert_eq!(last, [full(1, 40, vec![stopped])]);
  }

  #[test]
  fn a_player_that_moved_goes_to_the_store_again_with_a_greater_version_until_it_is_acknowledged() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), AccountId(4), &saved(8, 10_100)), Ok(()));
    assert_eq!(zone.dirty(), []);
    zone.intents(GATE, Link(1), &intents(None, [1, 2, 3], EAST));
    zone.tick();
    let moved =
      State { position: Position { x: 10_030, y: 0, z: 10_000 }, yaw: NORTH, hit_points: 100 };
    let save = |version| Save { character: CharacterId(7), state: moved, version };
    assert_eq!(zone.dirty(), [save(1)]);
    assert_eq!(zone.dirty(), [save(2)]);
    zone.saved(&[save(2), Save { character: CharacterId(99), ..save(1) }]);
    assert_eq!(zone.dirty(), []);
    zone.leave(GATE, Link(2));
    assert_eq!((zone.count(), zone.present()), (2, vec![(ACCOUNT, CharacterId(7))]));
  }

  #[test]
  fn a_player_that_leaves_for_good_takes_its_unsaved_state_to_the_store() {
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), AccountId(4), &saved(8, 10_100)), Ok(()));
    assert_eq!(zone.join(GATE, Link(3), AccountId(5), &saved(9, 10_200)), Ok(()));
    zone.intents(GATE, Link(1), &intents(None, [1, 2, 3], EAST));
    zone.tick();
    zone.logout(GATE, Link(1));
    zone.logout(GATE, Link(2));
    zone.leave(GATE, Link(3));
    drop(run(&mut zone, KEPT));
    let moved =
      State { position: Position { x: 10_030, y: 0, z: 10_000 }, yaw: NORTH, hit_points: 100 };
    let save = Save { character: CharacterId(7), state: moved, version: 1 };
    assert_eq!(
      zone.leavers(),
      [
        Leaver { account: ACCOUNT, save: Some(save) },
        Leaver { account: AccountId(4), save: None },
        Leaver { account: AccountId(5), save: None },
      ]
    );
    assert_eq!(zone.leavers(), []);
  }
}
