//! What the client of each player holds, and the snapshot that brings it to the present tick.

use std::collections::VecDeque;
use std::mem;

use mmo_protocol::{Change, CharacterId, DATAGRAM, Entity, Offset, Snapshot, Tick, encode};

/// The ticks that the writer keeps, so a delta starts from a snapshot of the last 0.8 s at most.
const KEPT: usize = 16;

/// The state of each entity of the zone at the present tick and at each of the 16 ticks before it,
/// each in the order of the ids, so each snapshot that a delta starts from is in it.
pub(crate) struct History {
  tick: Tick,
  entities: Vec<Entity>,
  past: VecDeque<(Tick, Vec<Entity>)>,
}

impl History {
  pub(crate) fn new() -> Self {
    Self { tick: Tick(0), entities: Vec::new(), past: VecDeque::new() }
  }

  /// Makes `entities`, in the order of their ids, the state of the present tick `tick`.
  pub(crate) fn push(&mut self, tick: Tick, entities: Vec<Entity>) {
    let was = (mem::replace(&mut self.tick, tick), mem::replace(&mut self.entities, entities));
    if self.past.len() == KEPT {
      self.past.pop_front();
    }
    self.past.push_back(was);
  }

  /// The entities at the present tick, in the order of their ids.
  pub(crate) fn entities(&self) -> &[Entity] {
    &self.entities
  }

  /// The state of `id` at `tick`, a tick before the present one.
  fn at(&self, tick: Tick, id: CharacterId) -> Option<&Entity> {
    self.past.iter().find(|(at, _)| *at == tick).and_then(|(_, entities)| {
      entities.binary_search_by_key(&id.0, |entity| entity.id.0).ok().map(|at| &entities[at])
    })
  }
}

/// What the client of one player holds after each of the last snapshots that the writer sent to
/// it: each entity that it holds, in the order of the ids, with the tick whose state it holds.
#[derive(Default)]
pub(crate) struct Sent(VecDeque<(Tick, Vec<(CharacterId, Tick)>)>);

/// What a snapshot says of one visible entity.
enum Part {
  Same(CharacterId),
  Changed(Change),
  Entered(Entity),
}

impl Sent {
  /// The snapshot of the present tick for a player who sees `visible`, nearest first.
  ///
  /// It is a delta against the snapshot of `ack` while the writer keeps that snapshot, and else a
  /// full snapshot. When it passes the size of a datagram, it leaves out the farthest entities:
  /// the client keeps what it held of them, and a next snapshot gives them.
  pub(crate) fn snapshot(
    &mut self,
    history: &History,
    ack: Option<Tick>,
    visible: &[Entity],
  ) -> Snapshot {
    let base = ack.and_then(|ack| self.0.iter().find(|(tick, _)| *tick == ack));
    let held = |id: CharacterId| {
      let (_, held) = base?;
      held.binary_search_by_key(&id.0, |(one, _)| one.0).ok().map(|at| held[at])
    };
    let parts: Vec<(Part, Option<(CharacterId, Tick)>)> = visible
      .iter()
      .map(|entity| {
        let was = held(entity.id);
        let part = match was.and_then(|(id, tick)| history.at(tick, id)) {
          Some(old) if old == entity => Part::Same(entity.id),
          Some(old) => Part::Changed(change(old, entity)),
          None => Part::Entered(*entity),
        };
        (part, was)
      })
      .collect();
    let left: Vec<CharacterId> = base
      .iter()
      .flat_map(|(_, held)| held)
      .filter(|(id, _)| visible.iter().all(|entity| entity.id != *id))
      .map(|(id, _)| *id)
      .collect();
    let mut count = parts.len();
    let snapshot = loop {
      let snapshot = Snapshot {
        tick: history.tick,
        base: base.map(|(tick, _)| *tick),
        entered: parts[..count]
          .iter()
          .filter_map(
            |(part, _)| if let Part::Entered(entity) = part { Some(*entity) } else { None },
          )
          .collect(),
        changed: parts[..count]
          .iter()
          .filter_map(
            |(part, _)| if let Part::Changed(change) = part { Some(*change) } else { None },
          )
          .collect(),
        left: left.clone(),
      };
      if count == 0 || encode(&snapshot).len() <= DATAGRAM {
        break snapshot;
      }
      count -= 1;
    };
    let (shown, out) = parts.split_at(count);
    let mut holds: Vec<(CharacterId, Tick)> = shown
      .iter()
      .map(|(part, _)| (part.id(), history.tick))
      .chain(out.iter().filter_map(|(_, was)| *was))
      .collect();
    holds.sort_by_key(|(id, _)| id.0);
    if self.0.len() == KEPT {
      self.0.pop_front();
    }
    self.0.push_back((history.tick, holds));
    snapshot
  }
}

impl Part {
  fn id(&self) -> CharacterId {
    match self {
      Part::Same(id) => *id,
      Part::Changed(change) => change.id,
      Part::Entered(entity) => entity.id,
    }
  }
}

fn change(old: &Entity, new: &Entity) -> Change {
  Change {
    id: new.id,
    movement: Offset {
      x: new.position.x - old.position.x,
      y: new.position.y - old.position.y,
      z: new.position.z - old.position.z,
    },
    velocity: new.velocity,
    view: new.view,
    animation: new.animation,
    hit_points: new.hit_points,
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Angle, Change, CharacterId, DATAGRAM, Entity, Offset, Position, Snapshot, Tick, Velocity,
    encode,
  };

  use super::{History, Sent};

  fn at(id: i64, x: i32) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z: 1000 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    }
  }

  #[test]
  fn a_delta_gives_what_entered_changed_and_left_since_the_acknowledged_snapshot() {
    let mut history = History::new();
    let mut sent = Sent::default();
    let first = [at(1, 0), at(2, 500), at(3, 900)];
    history.push(Tick(1), first.to_vec());
    let full = sent.snapshot(&history, None, &first);
    assert_eq!(
      full,
      Snapshot {
        tick: Tick(1),
        base: None,
        entered: first.to_vec(),
        changed: vec![],
        left: vec![]
      }
    );
    let moved = Entity { velocity: Velocity { x: 600, y: 0, z: 0 }, ..at(1, 60) };
    let second = [moved, at(2, 500), at(4, 700)];
    history.push(Tick(2), vec![moved, at(2, 500), at(3, 900), at(4, 700)]);
    let delta = sent.snapshot(&history, Some(Tick(1)), &second);
    let change = Change {
      id: CharacterId(1),
      movement: Offset { x: 60, y: 0, z: 0 },
      velocity: Velocity { x: 600, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    };
    assert_eq!(
      delta,
      Snapshot {
        tick: Tick(2),
        base: Some(Tick(1)),
        entered: vec![at(4, 700)],
        changed: vec![change],
        left: vec![CharacterId(3)],
      }
    );
    history.push(Tick(3), second.to_vec());
    let unknown = sent.snapshot(&history, Some(Tick(9)), &second);
    assert_eq!(
      unknown,
      Snapshot {
        tick: Tick(3),
        base: None,
        entered: second.to_vec(),
        changed: vec![],
        left: vec![]
      }
    );
  }

  #[test]
  fn a_delta_starts_from_a_snapshot_of_the_last_16_ticks_alone() {
    let mut history = History::new();
    let mut sent = Sent::default();
    let still = [at(1, 0)];
    for tick in 1..=17 {
      history.push(Tick(tick), still.to_vec());
      sent.snapshot(&history, None, &still);
    }
    let bases = [2, 1].map(|ack| sent.snapshot(&history, Some(Tick(ack)), &still).base);
    assert_eq!(bases, [Some(Tick(2)), None]);
  }

  /// Each entity has values far apart, so bitcode packs them badly.
  #[test]
  fn a_snapshot_past_a_datagram_leaves_out_the_farthest_entities_for_a_next_one() {
    let crowd: Vec<Entity> = (0..120)
      .map(|n: i32| Entity {
        position: Position { x: n * 7_919_011, y: -n * 104_729, z: n * 1_299_709 },
        hit_points: n * 15_485_863,
        view: Angle(u16::try_from(n * 541).unwrap()),
        ..at(i64::from(n), 0)
      })
      .collect();
    let mut history = History::new();
    let mut sent = Sent::default();
    history.push(Tick(1), crowd.clone());
    let first = sent.snapshot(&history, None, &crowd);
    let shown = first.entered.len();
    assert_eq!(first.entered, crowd[..shown]);
    assert!(encode(&first).len() <= DATAGRAM && shown < crowd.len(), "{shown}");
    history.push(Tick(2), crowd.clone());
    let second = sent.snapshot(&history, Some(Tick(1)), &crowd);
    assert_eq!(second.entered, crowd[shown..shown + second.entered.len()]);
    assert_eq!((second.base, second.changed, second.left), (Some(Tick(1)), vec![], vec![]));
  }
}
