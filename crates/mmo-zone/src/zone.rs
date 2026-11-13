//! The state of one layer of a zone, which its writer alone decides.

use std::collections::BTreeMap;
use std::mem;

use mmo_collide::{Body, SIDE_CM, Terrain, step};
use mmo_protocol::{
  AccountId, Angle, CharacterId, Entity, Input, Intents, Layer, Link, Position, Relayed, Save,
  Snapshot, State, TICKS_PER_SECOND, Tick, Velocity, ZoneId,
};
use mmo_store::{Neighbours, Saved};

use crate::interest::Grid;
use crate::view::{History, Sent};

/// The ticks that the zone keeps the player of a link that closed: 30 s.
const KEPT: u32 = 30 * TICKS_PER_SECOND;

/// The zone takes an input whose tick is at most this many ticks after its present tick.
const REACH: u32 = 4;

/// How far from a border a player is seen across it: 48 m.
const EDGE_CM: i32 = 4800;

/// The most entities that a layer sends across one border, so a message holds less than a frame.
const EDGE_MAX: usize = 1500;

/// The ticks that the zone keeps the entities that a layer across a border sent: 1 s. A layer that
/// sends no more takes its entities away after that time.
const EDGE_KEPT: u32 = TICKS_PER_SECOND;

/// The number that the layer writer gives to the link of a gateway, which relays the links of its
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
  #[error("the layer drains, and takes no new player")]
  Draining,
}

/// A player that left the zone, and the save that its last state needs. The writer keeps the
/// presence of the account until the store acknowledges that save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Leaver {
  pub(crate) account: AccountId,
  /// None when the store holds the last state already.
  pub(crate) save: Option<Save>,
}

/// A player that walked past a border of the zone, for its writer to hand over to the next zone, or
/// that moves out of a layer that drains, to another layer of its zone. The zone holds the player
/// until [`Zone::depart`] or [`Zone::stay`]: it takes no input of it, and saves it no more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Crossing {
  /// The zone that the player enters.
  pub(crate) zone: ZoneId,
  pub(crate) account: AccountId,
  /// The player in the coordinates of the zone that it enters.
  pub(crate) state: Entity,
  /// The version of the last save that this zone sent for the player.
  pub(crate) version: i64,
}

/// The state of one layer of a zone: its players on the ground, at its present tick.
pub(crate) struct Zone {
  layer: Layer,
  /// The zone across each border of the zone, as the map of the world holds them.
  neighbours: Neighbours,
  /// The entities that the layer with the same number in each zone across a border sent last, with
  /// the tick of the message, which this zone shows as entities that it does not own.
  edge: BTreeMap<i32, (Tick, Vec<Entity>)>,
  /// The most players of the layer, which the writer read from its shard when it claimed it.
  cap: usize,
  /// True once the directory put the layer in drain.
  draining: bool,
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
  /// The state that the store acknowledged last, or `None` when the store holds the player in
  /// another zone.
  saved: Option<State>,
  /// The version of the last save that the zone sent, which each new save passes by 1.
  version: i64,
  body: Body,
  view: Angle,
  velocity: Velocity,
  hit_points: i32,
  /// The inputs of the next ticks, by their tick.
  inputs: BTreeMap<u32, Input>,
  attachment: Attachment,
  /// True from the tick when the player walked past a border until it departs.
  crossing: bool,
}

enum Attachment {
  Linked { gateway: Gateway, link: Link, ack: Option<Tick>, sent: Sent },
  Away { since: Tick },
}

impl Zone {
  pub(crate) fn new(layer: Layer, cap: usize) -> Self {
    Self {
      layer,
      neighbours: Neighbours::default(),
      edge: BTreeMap::new(),
      cap,
      draining: false,
      terrain: ground(),
      tick: Tick(0),
      players: BTreeMap::new(),
      links: BTreeMap::new(),
      history: History::new(),
      left: Vec::new(),
    }
  }

  /// Sets the zone across each border, as the map of the world gives them, as section 6 of
  /// `docs/architecture.md` says.
  pub(crate) fn beside(mut self, neighbours: Neighbours) -> Self {
    self.neighbours = neighbours;
    self
  }

  /// The zone of the layer.
  pub(crate) fn zone(&self) -> ZoneId {
    self.layer.zone
  }

  /// The entities of the players within 48 m of each border that has a zone across it, for the
  /// layer with the same number in that zone. The entities are in the coordinates of that zone,
  /// which puts them outside its square. A border with no player near it gives an empty list.
  pub(crate) fn edges(&self) -> Vec<(Layer, Vec<Entity>)> {
    let Neighbours { west, east, south, north } = self.neighbours;
    let mut found = Vec::new();
    for (across, axis, high) in
      [(west, 0, false), (east, 0, true), (south, 1, false), (north, 1, true)]
    {
      let Some(zone) = across else { continue };
      let shift = if high { -SIDE_CM } else { SIDE_CM };
      let along = |entity: &Entity| [entity.position.x, entity.position.z][axis];
      let near = |entity: &Entity| {
        if high { along(entity) > SIDE_CM - EDGE_CM } else { along(entity) < EDGE_CM }
      };
      let entities = self.players.iter().map(|(id, player)| player.entity(CharacterId(*id)));
      let entities = entities.filter(near).take(EDGE_MAX).map(|mut entity| {
        if axis == 0 {
          entity.position.x += shift;
        } else {
          entity.position.z += shift;
        }
        entity
      });
      found.push((Layer { zone, number: self.layer.number }, entities.collect()));
    }
    found
  }

  /// Takes the entities that the layer of `zone` sent from its border, which replace the last ones
  /// of that zone. A zone that is not across a border of this zone is none of its business.
  pub(crate) fn seen(&mut self, zone: ZoneId, entities: Vec<Entity>) {
    let Neighbours { west, east, south, north } = self.neighbours;
    if [west, east, south, north].contains(&Some(zone)) {
      self.edge.insert(zone.0, (self.tick, entities));
    }
  }

  /// The count of the players that the zone holds, linked or not.
  pub(crate) fn count(&self) -> usize {
    self.players.len()
  }

  /// True when the layer does not drain and holds at most a quarter of its cap, the count under
  /// which its writer asks the directory to drain it.
  pub(crate) fn low(&self) -> bool {
    !self.draining && self.players.len() * 4 <= self.cap
  }

  /// Puts the layer in drain, as the directory did: it takes no new player, and gives each of its
  /// players to [`Zone::moves`].
  pub(crate) fn drain(&mut self) {
    self.draining = true;
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
    let walking = self.players.iter_mut().filter(|(_, player)| !player.crossing);
    walking.filter_map(|(id, player)| player.unsaved(*id)).collect()
  }

  /// Notes that the store holds `saves`, so [`Zone::dirty`] gives a player again only after it
  /// moved.
  pub(crate) fn saved(&mut self, saves: &[Save]) {
    for save in saves {
      if let Some(player) = self.players.get_mut(&save.character.0) {
        player.saved = Some(save.state);
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
    let refusal = self.refusal();
    match (self.players.get_mut(&saved.character.0), refusal) {
      (Some(player), _) => {
        if let Attachment::Linked { link: old, .. } = player.attachment {
          self.links.remove(&old.0);
        }
        player.attachment = attachment;
      }
      (None, Some(why)) => return Err(why),
      (None, None) => {
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

  /// The client of `link` ends the play of its character, which leaves the zone at once, unless
  /// the zone hands it to the next zone: the writer of that zone keeps it for 30 s then.
  pub(crate) fn logout(&mut self, gateway: Gateway, link: Link) {
    let Some(id) = self.unlink(gateway, link) else { return };
    match self.players.get_mut(&id) {
      Some(player) if player.crossing => player.attachment = Attachment::Away { since: self.tick },
      _ => self.drop_player(id),
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
    let Some(Player {
      inputs,
      attachment: Attachment::Linked { gateway: via, ack, .. },
      crossing: false,
      ..
    }) = player
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
    let expired = |player: &&Player| {
      matches!(player.attachment, Attachment::Away { since } if now.0 - since.0 >= KEPT)
        && !player.crossing
    };
    let gone: Vec<i64> =
      self.players.iter().filter(|(_, p)| expired(p)).map(|(id, _)| *id).collect();
    for id in gone {
      self.drop_player(id);
    }
    for player in self.players.values_mut().filter(|player| !player.crossing) {
      player.step(&self.terrain, now);
    }
    self.edge.retain(|_, (at, _)| now.0 - at.0 < EDGE_KEPT);
    let ghosts = self.edge.values().flat_map(|(_, found)| found).copied();
    let players = self.players.iter().map(|(id, player)| player.entity(CharacterId(*id)));
    let mut entities: Vec<Entity> = players.chain(ghosts).collect();
    entities.sort_by_key(|entity| entity.id.0);
    entities.dedup_by_key(|entity| entity.id.0);
    self.history.push(now, entities);
    let entities = self.history.entities();
    let grid = Grid::new(entities);
    let mut snapshots = Vec::new();
    for (id, player) in &mut self.players {
      let entity = &entities[entities.partition_point(|one| one.id.0 < *id)];
      if let Attachment::Linked { gateway, link, ack, sent } = &mut player.attachment {
        let message = sent.snapshot(&self.history, *ack, &grid.visible(entity));
        snapshots.push((*gateway, Relayed { link: *link, message }));
      }
    }
    snapshots
  }

  /// The players that walked past a border to a zone that the world holds, since the last call.
  /// Each stops where it stands, and the writer hands it over.
  pub(crate) fn crossings(&mut self) -> Vec<Crossing> {
    let mut found = Vec::new();
    for (id, player) in &mut self.players {
      let [x, _, z] = player.body.position;
      let Neighbours { west, east, south, north } = self.neighbours;
      let (to, shift) = if x < 0 {
        (west, [SIDE_CM, 0])
      } else if x >= SIDE_CM {
        (east, [-SIDE_CM, 0])
      } else if z < 0 {
        (south, [0, SIDE_CM])
      } else if z >= SIDE_CM {
        (north, [0, -SIDE_CM])
      } else {
        continue;
      };
      let Some(zone) = to.filter(|_| player.walks()) else { continue };
      found.push(player.cross(zone, CharacterId(*id), shift));
    }
    found
  }

  /// Stops each player of a link that walks in a layer that drains, for its writer to move it to
  /// another layer of the zone. A move cuts no act of a player, since the game has no order, no
  /// hit and no trade yet, so each tick is a safe point.
  pub(crate) fn moves(&mut self) -> Vec<Crossing> {
    let (zone, draining) = (self.layer.zone, self.draining);
    let walking = self.players.iter_mut().filter(|(_, player)| draining && player.walks());
    walking.map(|(id, player)| player.cross(zone, CharacterId(*id), [0, 0])).collect()
  }

  /// The player of `character` stays in the layer, since no other layer took it: it walks again,
  /// and its writer moves it at a later tick.
  pub(crate) fn stay(&mut self, character: CharacterId) {
    if let Some(player) = self.players.get_mut(&character.0) {
      player.crossing = false;
    }
  }

  /// Drops the player of `character`, which the writer of the next zone holds now, and gives the
  /// gateway and the link that carried it, when it had one.
  pub(crate) fn depart(&mut self, character: CharacterId) -> Option<(Gateway, Link)> {
    let player = self.players.remove(&character.0)?;
    let Attachment::Linked { gateway, link, .. } = player.attachment else { return None };
    self.links.remove(&link.0);
    Some((gateway, link))
  }

  /// The gateway and the link of the player of `character`, when it has one.
  pub(crate) fn linked(&self, character: CharacterId) -> Option<(Gateway, Link)> {
    match self.players.get(&character.0)?.attachment {
      Attachment::Linked { gateway, link, .. } => Some((gateway, link)),
      Attachment::Away { .. } => None,
    }
  }

  /// Takes the player that `entity` describes, which crossed a border of the next zone, with no
  /// link: its client joins it after the writer of the next zone tells the client. It gives the
  /// save that the store needs, which moves the character to this zone with the next version, or
  /// `None` when the zone holds the player and the store holds its state. A zone that holds the
  /// player already gives its save again, as a hand-over that the next zone repeats needs.
  pub(crate) fn receive(
    &mut self,
    account: AccountId,
    entity: &Entity,
    version: i64,
  ) -> Result<Option<Save>, Refusal> {
    let id = entity.id.0;
    if let Some(player) = self.players.get_mut(&id) {
      return Ok(player.unsaved(id));
    }
    if let Some(why) = self.refusal() {
      return Err(why);
    }
    let mut player =
      Player::arrived(account, entity, version, Attachment::Away { since: self.tick });
    let save = player.unsaved(id);
    self.players.insert(id, player);
    Ok(save)
  }

  /// Why the layer takes no new player now, or `None` when it takes one.
  fn refusal(&self) -> Option<Refusal> {
    if self.draining {
      Some(Refusal::Draining)
    } else {
      (self.players.len() >= self.cap).then_some(Refusal::Full { cap: self.cap })
    }
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
      saved: Some(saved.state),
      version: saved.version,
      body: Body { position: [x, y, z], vertical_speed: 0 },
      view: saved.state.yaw,
      velocity: Velocity { x: 0, y: 0, z: 0 },
      hit_points: saved.state.hit_points,
      inputs: BTreeMap::new(),
      attachment,
      crossing: false,
    }
  }

  /// A player that crossed a border into this zone, which the store holds in the other zone.
  fn arrived(account: AccountId, entity: &Entity, version: i64, attachment: Attachment) -> Self {
    let Position { x, y, z } = entity.position;
    Self {
      account,
      saved: None,
      version,
      body: Body { position: [x, y, z], vertical_speed: 0 },
      view: entity.view,
      velocity: Velocity { x: 0, y: 0, z: 0 },
      hit_points: entity.hit_points,
      inputs: BTreeMap::new(),
      attachment,
      crossing: false,
    }
  }

  /// True when the player has a link, and leaves the layer by no border and no move.
  fn walks(&self) -> bool {
    matches!(self.attachment, Attachment::Linked { .. }) && !self.crossing
  }

  /// Stops the player for its hand-over to `zone`, and gives its state there, `shift` centimetres
  /// along x and z from its place here.
  fn cross(&mut self, zone: ZoneId, id: CharacterId, shift: [i32; 2]) -> Crossing {
    self.crossing = true;
    self.velocity = Velocity { x: 0, y: 0, z: 0 };
    let mut state = self.entity(id);
    state.position.x += shift[0];
    state.position.z += shift[1];
    Crossing { zone, account: self.account, state, version: self.version }
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
    (Some(state) != self.saved).then(|| {
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
  use mmo_collide::SIDE_CM;
  use mmo_protocol::Layer;
  use mmo_protocol::{
    AccountId, Angle, Change, CharacterId, Entity, Input, Intents, Link, Offset, Position, Relayed,
    Save, Snapshot, State, TICKS_PER_SECOND, Tick, Velocity, ZoneId, encode,
  };
  use mmo_store::{Neighbours, Saved};

  use super::{Crossing, Gateway, KEPT, Leaver, Refusal, Zone};

  const MEADOW: ZoneId = ZoneId(1);
  /// The zones across the four borders of the meadow, with ids that no rule of order gives.
  const AROUND: Neighbours = Neighbours {
    west: Some(ZoneId(11)),
    east: Some(ZoneId(12)),
    south: Some(ZoneId(13)),
    north: Some(ZoneId(14)),
  };
  const FOREST: ZoneId = ZoneId(2);
  const LAYER: Layer = Layer { zone: MEADOW, number: 1 };
  /// The cap of a layer at the start of a shard.
  const CAP: usize = 500;
  const ACCOUNT: AccountId = AccountId(3);
  const GATE: Gateway = Gateway(1);
  const EAST: Option<Angle> = Some(Angle(0));
  const NORTH: Angle = Angle(16384);

  fn saved(id: i64, x: i32) -> Saved {
    placed(id, x, 10_000)
  }

  fn placed(id: i64, x: i32, z: i32) -> Saved {
    Saved {
      character: CharacterId(id),
      zone: MEADOW,
      state: State { position: Position { x, y: 0, z }, yaw: Angle(0), hit_points: 100 },
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

  /// A layer of the meadow with the zones `across` its borders, and the player 7 at `x` and `z`.
  fn bordered(across: Neighbours, x: i32, z: i32) -> Zone {
    let mut writer = Zone::new(LAYER, CAP).beside(across);
    assert_eq!(writer.join(GATE, Link(1), ACCOUNT, &placed(7, x, z)), Ok(()));
    writer
  }

  /// Runs `ticks` ticks of a zone with the walk of its one player, who is on the link 1.
  fn walk(zone: &mut Zone, at: ZoneId, heading: Option<Angle>, ticks: u32) {
    for _ in 0..ticks {
      let now = zone.tick.0;
      let walking = Intents { zone: at, ..intents(None, [now + 1, now + 2, now + 3], heading) };
      zone.intents(GATE, Link(1), &walking);
      zone.tick();
    }
  }

  fn standing(id: i64, x: i32, z: i32) -> Entity {
    Entity { position: Position { x, y: 0, z }, ..entity(id, x, 0, Angle(0)) }
  }

  /// A layer of the meadow with the zones `AROUND` it, and a player on the link 1 at each place.
  fn crowded(places: &[(i32, i32)]) -> Zone {
    let mut zone = Zone::new(LAYER, CAP).beside(AROUND);
    for (id, (x, z)) in (1_i64..).zip(places) {
      assert_eq!(
        zone.join(GATE, Link(id.unsigned_abs()), AccountId(id), &placed(id, *x, *z)),
        Ok(())
      );
    }
    zone
  }

  #[test]
  fn a_layer_gives_the_players_within_48_m_of_a_border_to_the_layer_across_it() {
    let mut zone = crowded(&[
      (4_799, 10_000),
      (4_800, 10_000),
      (SIDE_CM - 4_800, 10_000),
      (SIDE_CM - 4_799, 10_000),
      (10_000, 30),
      (10_000, SIDE_CM - 30),
      (50_000, 50_000),
    ]);
    let layer = |zone| Layer { zone: ZoneId(zone), number: 1 };
    let expected = vec![
      (layer(11), vec![standing(1, 4_799 + SIDE_CM, 10_000)]),
      (layer(12), vec![standing(4, -4_799, 10_000)]),
      (layer(13), vec![standing(5, 10_000, 30 + SIDE_CM)]),
      (layer(14), vec![standing(6, 10_000, -30)]),
    ];
    assert_eq!(zone.edges(), expected);
    zone.layer.number = 4;
    assert_eq!(zone.edges().iter().map(|(layer, _)| layer.number).collect::<Vec<_>>(), [4; 4]);
  }

  #[test]
  fn a_layer_gives_nothing_across_a_border_that_has_no_zone_beyond_it() {
    let places = [(30, 10_000), (SIDE_CM - 30, 10_000)];
    let mut zone = Zone::new(LAYER, CAP);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &placed(1, places[0].0, places[0].1)), Ok(()));
    assert_eq!(zone.edges(), []);
    let only_east = Neighbours { east: Some(ZoneId(12)), ..Neighbours::default() };
    let zone = zone.beside(only_east);
    assert_eq!(zone.edges(), [(Layer { zone: ZoneId(12), number: 1 }, vec![])]);
  }

  /// The ids and the places on x of the entities that the one linked player sees at the next tick.
  fn sees(zone: &mut Zone) -> Vec<(i64, i32)> {
    let [(_, told)] = zone.tick().try_into().expect("one snapshot");
    told.message.entered.iter().map(|one| (one.id.0, one.position.x)).collect()
  }

  #[test]
  fn a_player_sees_the_entities_that_the_layers_across_its_borders_tell_for_1_s() {
    let mut zone = crowded(&[(30, 10_000)]);
    zone.seen(ZoneId(11), vec![standing(90, -1_000, 10_000)]);
    assert_eq!(sees(&mut zone), [(1, 30), (90, -1_000)]);
    drop(run(&mut zone, TICKS_PER_SECOND - 3));
    assert_eq!(sees(&mut zone), [(1, 30), (90, -1_000)]);
    assert_eq!(sees(&mut zone), [(1, 30)]);
  }

  #[test]
  fn a_new_list_of_a_layer_replaces_its_last_one_and_an_empty_list_takes_its_entities_away() {
    let mut zone = crowded(&[(30, 10_000)]);
    zone.seen(ZoneId(11), vec![standing(90, -1_000, 10_000)]);
    zone.seen(ZoneId(12), vec![standing(91, -2_000, 10_000)]);
    assert_eq!(sees(&mut zone), [(1, 30), (90, -1_000), (91, -2_000)]);
    zone.seen(ZoneId(11), vec![standing(92, -500, 10_000)]);
    assert_eq!(sees(&mut zone), [(1, 30), (92, -500), (91, -2_000)]);
    zone.seen(ZoneId(11), vec![]);
    assert_eq!(sees(&mut zone), [(1, 30), (91, -2_000)]);
  }

  #[test]
  fn a_zone_takes_the_entities_of_a_zone_across_one_of_its_borders_alone() {
    let mut zone = crowded(&[(30, 10_000)]);
    zone.seen(ZoneId(99), vec![standing(90, -1_000, 10_000)]);
    assert_eq!(sees(&mut zone), [(1, 30)]);
  }

  #[test]
  fn a_player_of_the_zone_hides_an_entity_across_a_border_with_its_id() {
    let mut zone = crowded(&[(30, 10_000)]);
    let twice =
      [standing(1, -500, 10_000), standing(90, -1_000, 10_000), standing(90, -900, 10_000)];
    zone.seen(ZoneId(11), twice.to_vec());
    assert_eq!(sees(&mut zone), [(1, 30), (90, -1_000)]);
  }

  #[test]
  fn a_player_that_walks_past_the_east_border_stops_and_waits_for_the_next_zone() {
    let mut zone = bordered(AROUND, SIDE_CM - 60, 10_000);
    walk(&mut zone, MEADOW, EAST, 1);
    assert_eq!(zone.crossings(), []);
    walk(&mut zone, MEADOW, EAST, 1);
    let state = entity(7, 0, 0, NORTH);
    let crossing = Crossing { zone: ZoneId(12), account: ACCOUNT, state, version: 0 };
    assert_eq!(zone.crossings(), [crossing]);
    assert_eq!(zone.crossings(), []);
    zone.intents(GATE, Link(1), &intents(None, [3, 4, 5], EAST));
    let [(_, held)] = zone.tick().try_into().expect("one snapshot");
    assert_eq!(held.message.entered, [entity(7, SIDE_CM, 0, NORTH)]);
    assert_eq!((zone.dirty(), zone.linked(CharacterId(7))), (vec![], Some((GATE, Link(1)))));
    assert_eq!(zone.linked(CharacterId(99)), None);
    assert_eq!(zone.depart(CharacterId(7)), Some((GATE, Link(1))));
    assert_eq!((zone.count(), zone.leavers(), zone.depart(CharacterId(7))), (0, vec![], None));
  }

  /// A walk over one border: where the player starts and its heading, then the zone across the
  /// border and the place of the player in that zone, after 2 ticks of 30 cm.
  struct Walk {
    from: (i32, i32),
    heading: Angle,
    across: ZoneId,
    enters: (i32, i32),
  }

  const BORDERS: [Walk; 4] = [
    Walk {
      from: (30, 10_000),
      heading: Angle(32768),
      across: ZoneId(11),
      enters: (SIDE_CM - 30, 10_000),
    },
    Walk {
      from: (SIDE_CM - 60, 10_000),
      heading: Angle(0),
      across: ZoneId(12),
      enters: (0, 10_000),
    },
    Walk {
      from: (10_000, 30),
      heading: Angle(49152),
      across: ZoneId(13),
      enters: (10_000, SIDE_CM - 30),
    },
    Walk {
      from: (10_000, SIDE_CM - 60),
      heading: Angle(16384),
      across: ZoneId(14),
      enters: (10_000, 0),
    },
  ];

  #[test]
  fn a_layer_that_drains_takes_no_new_player_and_stops_each_player_of_a_link_for_a_move() {
    let mut zone = Zone::new(LAYER, 8);
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &saved(7, 10_000)), Ok(()));
    assert_eq!(zone.join(GATE, Link(2), AccountId(4), &saved(8, 10_100)), Ok(()));
    assert!(zone.low(), "2 players are a quarter of a cap of 8");
    assert_eq!(zone.join(GATE, Link(3), AccountId(5), &saved(9, 10_200)), Ok(()));
    assert!(!zone.low());
    zone.leave(GATE, Link(3));
    zone.intents(GATE, Link(1), &intents(None, [1, 2, 3], EAST));
    zone.tick();
    assert_eq!(zone.moves(), [], "a layer that does not drain moves no player");
    zone.drain();
    assert!(!zone.low(), "a layer that drains asks no more");
    let moving = |account, state| Crossing { zone: MEADOW, account, state, version: 0 };
    let brom = moving(ACCOUNT, entity(7, 10_030, 0, NORTH));
    let cael = moving(AccountId(4), entity(8, 10_100, 0, Angle(0)));
    assert_eq!(zone.moves(), [brom, cael]);
    assert_eq!(zone.moves(), []);
    zone.intents(GATE, Link(1), &intents(None, [2, 3, 4], EAST));
    let [(_, held), _] = zone.tick().try_into().expect("two snapshots");
    assert!(held.message.entered.contains(&brom.state), "a player that moves stands");
    assert_eq!(zone.dirty(), []);
    zone.stay(CharacterId(8));
    zone.stay(CharacterId(99));
    assert_eq!(zone.moves(), [cael]);
    assert_eq!(zone.join(GATE, Link(4), AccountId(5), &saved(9, 0)), Ok(()), "9 is in the layer");
    let refused = zone.join(GATE, Link(5), AccountId(6), &saved(10, 0));
    assert_eq!(refused, Err(Refusal::Draining));
    let arrived = zone.receive(AccountId(6), &entity(10, 0, 0, NORTH), 0);
    assert_eq!(arrived, Err(Refusal::Draining));
    assert_eq!(Refusal::Draining.to_string(), "the layer drains, and takes no new player");
  }

  #[test]
  fn a_player_that_walks_past_each_border_enters_the_opposite_edge_of_the_zone_across_it() {
    for Walk { from, heading, across, enters } in BORDERS {
      let mut zone = bordered(AROUND, from.0, from.1);
      walk(&mut zone, MEADOW, Some(heading), 2);
      let [crossing] = zone.crossings().try_into().expect("one crossing");
      let at = crossing.state.position;
      assert_eq!((crossing.zone, (at.x, at.z)), (across, enters));
    }
  }

  #[test]
  fn a_zone_with_no_zone_beyond_a_border_hands_no_player_over_on_it() {
    for Walk { from, heading, .. } in BORDERS {
      let mut zone = bordered(Neighbours::default(), from.0, from.1);
      walk(&mut zone, MEADOW, Some(heading), 2);
      assert_eq!(zone.crossings(), []);
    }
    let only_west = Neighbours { west: Some(ZoneId(11)), ..Neighbours::default() };
    let mut zone = bordered(only_west, SIDE_CM - 60, 10_000);
    walk(&mut zone, MEADOW, EAST, 2);
    assert_eq!(zone.crossings(), []);
  }

  #[test]
  fn a_player_with_no_link_stays_at_a_border() {
    let mut zone = bordered(AROUND, SIDE_CM - 30, 10_000);
    walk(&mut zone, MEADOW, EAST, 1);
    zone.leave(GATE, Link(1));
    assert_eq!(zone.crossings(), []);
  }

  #[test]
  fn a_player_that_logs_out_while_it_crosses_stays_until_the_next_zone_holds_it() {
    let mut zone = bordered(AROUND, SIDE_CM - 30, 10_000);
    walk(&mut zone, MEADOW, EAST, 1);
    assert_eq!(zone.crossings().len(), 1);
    zone.logout(GATE, Link(1));
    drop(run(&mut zone, KEPT));
    assert_eq!((zone.count(), zone.leavers(), zone.present()), (1, vec![], vec![]));
    assert_eq!((zone.linked(CharacterId(7)), zone.depart(CharacterId(7))), (None, None));
    assert_eq!(zone.count(), 0);
  }

  #[test]
  fn a_player_that_crossed_a_border_arrives_with_no_link_and_gets_its_save_in_this_zone() {
    let mut zone = Zone::new(Layer { zone: FOREST, number: 1 }, 2);
    let state = State { position: Position { x: 0, y: 0, z: 10_000 }, yaw: NORTH, hit_points: 100 };
    let save = |version| Save { character: CharacterId(7), state, version };
    let arrived = entity(7, 0, 0, NORTH);
    assert_eq!(zone.receive(ACCOUNT, &arrived, 4), Ok(Some(save(5))));
    assert_eq!(zone.receive(ACCOUNT, &arrived, 4), Ok(Some(save(6))));
    zone.saved(&[save(6)]);
    assert_eq!(zone.receive(ACCOUNT, &arrived, 4), Ok(None));
    assert_eq!((zone.count(), zone.present(), zone.linked(CharacterId(7))), (1, vec![], None));
    let second = Save { character: CharacterId(8), state, version: 1 };
    assert_eq!(zone.receive(AccountId(4), &entity(8, 0, 0, NORTH), 0), Ok(Some(second)));
    let full = zone.receive(AccountId(5), &entity(9, 0, 0, NORTH), 0);
    assert_eq!(full, Err(Refusal::Full { cap: 2 }));
    let row = Saved { character: CharacterId(7), zone: FOREST, state, version: 6 };
    assert_eq!(zone.join(GATE, Link(1), ACCOUNT, &row), Ok(()));
    assert_eq!(zone.present(), [(ACCOUNT, CharacterId(7))]);
  }
}
