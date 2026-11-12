//! The entities that the snapshots of the zone writer give.

use std::collections::{BTreeMap, VecDeque};

use mmo_protocol::{Angle, CharacterId, Entity, Position, Snapshot, Tick};

/// The snapshots that the client keeps, so a delta that starts from an old one finds it.
const KEPT: usize = 32;

/// Why [`World::apply`] keeps a snapshot out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Gap {
  /// The snapshot is not newer than the last one: a datagram can come late or twice.
  #[error("the snapshot of tick {} is not newer than the tick {}", .tick.0, .held.0)]
  Late {
    /// The tick of the snapshot.
    tick: Tick,
    /// The tick of the last snapshot that the client holds.
    held: Tick,
  },
  /// The delta starts from a snapshot that the client does not hold. The client keeps its last
  /// acknowledgement, so the writer sends a full snapshot after 0.8 s at most.
  #[error("the snapshot of tick {} starts from the tick {}, which the client does not hold", .tick.0, .base.0)]
  Base {
    /// The tick of the snapshot.
    tick: Tick,
    /// The tick of the snapshot that the delta starts from.
    base: Tick,
  },
}

/// An entity as the client draws it: its place between two snapshots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
  /// The id of the entity.
  pub id: CharacterId,
  /// The place of its feet, in metres.
  pub position: [f32; 3],
  /// The direction in which it looks, in radians, as [`Angle::radians`] gives it.
  pub view: f32,
  /// Its speed on the ground, in metres each second.
  pub speed: f32,
  /// The animation that it plays, by its number in the content of the game.
  pub animation: u8,
}

/// The state of each entity that the player sees, as the last snapshots of the zone writer give
/// it.
#[derive(Debug, Default)]
pub struct World {
  held: VecDeque<(Tick, BTreeMap<i64, Entity>)>,
}

impl World {
  /// Forgets each snapshot. The player is in another zone, whose writer counts its own ticks.
  pub fn clear(&mut self) {
    self.held.clear();
  }

  /// Adds the snapshot to the state. A full snapshot gives the state alone. A delta starts from
  /// the state that the client held after the snapshot of its base, with each entity ahead by the
  /// ticks since the base: it removes the entities that left, moves the entities that changed,
  /// and adds the entities that entered.
  pub fn apply(&mut self, snapshot: &Snapshot) -> Result<(), Gap> {
    let tick = snapshot.tick;
    if let Some(held) = self.ack().filter(|held| held.0 >= tick.0) {
      return Err(Gap::Late { tick, held });
    }
    let mut state = match snapshot.base {
      None => BTreeMap::new(),
      Some(base) => self
        .held
        .iter()
        .find(|(held, _)| *held == base)
        .map(|(_, state)| {
          let ticks = tick.0.saturating_sub(base.0);
          state.iter().map(|(id, entity)| (*id, entity.ahead(ticks))).collect()
        })
        .ok_or(Gap::Base { tick, base })?,
    };
    for id in &snapshot.left {
      state.remove(&id.0);
    }
    for change in &snapshot.changed {
      if let Some(entity) = state.get_mut(&change.id.0) {
        let Position { x, y, z } = entity.position;
        let by = change.movement;
        entity.position = Position { x: x + by.x, y: y + by.y, z: z + by.z };
        entity.velocity = change.velocity.unwrap_or(entity.velocity);
        entity.view = change.view.unwrap_or(entity.view);
        entity.animation = change.animation.unwrap_or(entity.animation);
        entity.hit_points = change.hit_points.unwrap_or(entity.hit_points);
      }
    }
    for entity in &snapshot.entered {
      state.insert(entity.id.0, *entity);
    }
    if self.held.len() == KEPT {
      self.held.pop_front();
    }
    self.held.push_back((tick, state));
    Ok(())
  }

  /// The tick of the last snapshot that the client holds, which it acknowledges to the writer.
  pub fn ack(&self) -> Option<Tick> {
    self.held.back().map(|(tick, _)| *tick)
  }

  /// The state of the entity `id` at the last snapshot.
  pub fn entity(&self, id: CharacterId) -> Option<&Entity> {
    self.held.back().and_then(|(_, state)| state.get(&id.0))
  }

  /// Each entity of the last snapshot, as it stands `alpha` of the way from the snapshot before
  /// it. `alpha` is 0 at the moment that the last snapshot comes, and 1 one tick later. An entity
  /// that the snapshot before did not hold stands at its last place.
  pub fn seen(&self, alpha: f32) -> Vec<Seen> {
    let alpha = alpha.clamp(0.0, 1.0);
    let Some((_, last)) = self.held.back() else {
      return Vec::new();
    };
    let before = self.held.len().checked_sub(2).and_then(|at| self.held.get(at));
    last
      .values()
      .map(|entity| {
        let to = entity.position.metres();
        let from = before
          .and_then(|(_, state)| state.get(&entity.id.0))
          .map_or(to, |old| old.position.metres());
        let [vx, _, vz] = entity.velocity.metres_per_second();
        Seen {
          id: entity.id,
          position: [0, 1, 2].map(|axis| from[axis] + (to[axis] - from[axis]) * alpha),
          view: Angle::radians(entity.view),
          speed: vx.hypot(vz),
          animation: entity.animation,
        }
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Angle, Change, CharacterId, Entity, Offset, Position, Snapshot, Tick, Velocity,
  };

  use super::{Gap, Seen, World};

  fn entity(id: i64, x: i32) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z: 1000 },
      velocity: Velocity { x: 300, y: 0, z: 400 },
      view: Angle(16384),
      animation: 1,
      hit_points: 100,
    }
  }

  fn full(tick: u32, entered: Vec<Entity>) -> Snapshot {
    Snapshot { tick: Tick(tick), base: None, entered, changed: vec![], left: vec![] }
  }

  fn delta(tick: u32, base: u32) -> Snapshot {
    Snapshot { base: Some(Tick(base)), ..full(tick, vec![]) }
  }

  fn change(id: i64, by: i32) -> Change {
    Change {
      id: CharacterId(id),
      movement: Offset { x: by, y: 50, z: -25 },
      velocity: Some(Velocity { x: 0, y: 0, z: 0 }),
      view: Some(Angle(0)),
      animation: Some(0),
      hit_points: Some(90),
    }
  }

  #[test]
  fn a_delta_moves_adds_and_removes_entities_from_the_state_of_its_base() {
    let mut world = World::default();
    assert_eq!(world.apply(&full(10, vec![entity(1, 100), entity(2, 200)])), Ok(()));
    let next = Snapshot {
      changed: vec![change(1, 30), change(9, 1)],
      entered: vec![entity(3, 300)],
      left: vec![CharacterId(2)],
      ..delta(11, 10)
    };
    assert_eq!(world.apply(&next), Ok(()));
    let moved = Entity {
      position: Position { x: 145, y: 50, z: 995 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 90,
      ..entity(1, 0)
    };
    assert_eq!(world.entity(CharacterId(1)), Some(&moved));
    assert_eq!(world.entity(CharacterId(2)), None);
    assert_eq!(world.entity(CharacterId(3)), Some(&entity(3, 300)));
    assert_eq!(world.ack(), Some(Tick(11)));
  }

  #[test]
  fn an_entity_that_a_delta_does_not_name_walks_on_and_a_change_keeps_what_it_does_not_give() {
    let mut world = World::default();
    world.apply(&full(10, vec![entity(1, 100), entity(2, 200)])).expect("a full snapshot");
    let moved =
      Change { velocity: None, view: None, animation: None, hit_points: None, ..change(1, 30) };
    world.apply(&Snapshot { changed: vec![moved], ..delta(12, 10) }).expect("a delta");
    let kept = Entity { position: Position { x: 160, y: 50, z: 1015 }, ..entity(1, 0) };
    assert_eq!(world.entity(CharacterId(1)), Some(&kept));
    let walked = Entity { position: Position { x: 230, y: 0, z: 1040 }, ..entity(2, 0) };
    assert_eq!(world.entity(CharacterId(2)), Some(&walked));
  }

  #[test]
  fn a_delta_starts_from_its_base_and_not_from_the_last_snapshot() {
    let mut world = World::default();
    world.apply(&full(10, vec![entity(1, 100)])).expect("a full snapshot");
    world.apply(&Snapshot { changed: vec![change(1, 30)], ..delta(11, 10) }).expect("a delta");
    world.apply(&Snapshot { changed: vec![change(1, 60)], ..delta(12, 10) }).expect("a delta");
    assert_eq!(world.entity(CharacterId(1)).map(|one| one.position.x), Some(190));
  }

  #[test]
  fn a_late_snapshot_and_a_delta_of_an_unknown_base_are_kept_out() {
    let mut world = World::default();
    world.apply(&full(10, vec![entity(1, 100)])).expect("a full snapshot");
    assert_eq!(world.apply(&full(10, vec![])), Err(Gap::Late { tick: Tick(10), held: Tick(10) }));
    assert_eq!(world.apply(&delta(12, 7)), Err(Gap::Base { tick: Tick(12), base: Tick(7) }));
    assert_eq!(world.ack(), Some(Tick(10)));
    assert_eq!(
      Gap::Base { tick: Tick(12), base: Tick(7) }.to_string(),
      "the snapshot of tick 12 starts from the tick 7, which the client does not hold"
    );
    assert_eq!(
      Gap::Late { tick: Tick(3), held: Tick(4) }.to_string(),
      "the snapshot of tick 3 is not newer than the tick 4"
    );
  }

  #[test]
  fn the_client_keeps_the_last_32_snapshots() {
    let mut world = World::default();
    world.apply(&full(1, vec![entity(1, 0)])).expect("a full snapshot");
    for tick in 2..=33 {
      world.apply(&delta(tick, tick - 1)).expect("a delta");
    }
    assert_eq!(world.apply(&delta(34, 1)), Err(Gap::Base { tick: Tick(34), base: Tick(1) }));
    assert_eq!(world.apply(&delta(34, 2)), Ok(()));
  }

  #[test]
  fn an_entity_stands_between_its_place_at_the_snapshot_before_and_its_place_now() {
    let mut world = World::default();
    assert_eq!(world.seen(0.5), vec![]);
    let still = Entity { velocity: Velocity { x: 0, y: 0, z: 0 }, ..entity(1, 100) };
    world.apply(&full(10, vec![still])).expect("a full snapshot");
    let seen = |world: &World, alpha| world.seen(alpha)[0];
    assert_eq!(seen(&world, 0.5).position.map(f32::to_bits), [1.0, 0.0, 10.0].map(f32::to_bits));
    world
      .apply(&Snapshot {
        changed: vec![change(1, 100)],
        entered: vec![entity(2, 0)],
        ..delta(11, 10)
      })
      .expect("a delta");
    let both = world.seen(0.5);
    assert_eq!(both.len(), 2);
    assert_eq!(
      both[0],
      Seen {
        id: CharacterId(1),
        position: [1.5, 0.25, 9.875],
        view: 0.0,
        speed: 0.0,
        animation: 0,
      }
    );
    assert_eq!(both[1].position.map(f32::to_bits), [0.0, 0.0, 10.0].map(f32::to_bits));
    assert_eq!((both[1].speed, both[1].view), (5.0, Angle(16384).radians()));
    assert_eq!(seen(&world, 3.0).position.map(f32::to_bits), [2.0, 0.5, 9.75].map(f32::to_bits));
    assert_eq!(seen(&world, -1.0).position.map(f32::to_bits), [1.0, 0.0, 10.0].map(f32::to_bits));
    world.clear();
    assert_eq!((world.ack(), world.seen(0.0)), (None, vec![]));
  }
}
