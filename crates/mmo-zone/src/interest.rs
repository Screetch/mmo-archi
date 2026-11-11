//! The interest of each player: the entities that it sees.

use std::collections::BTreeMap;

use mmo_protocol::{Entity, Position};

/// The side of a cell of the grid, in centimetres: 32 m.
const CELL: i32 = 3200;

/// The most entities that a player sees.
const SEEN: usize = 64;

/// The entities of the zone, by the cell of the grid that holds the position of each.
pub(crate) struct Grid<'a> {
  cells: BTreeMap<(i32, i32), Vec<&'a Entity>>,
}

impl<'a> Grid<'a> {
  pub(crate) fn new(entities: &'a [Entity]) -> Self {
    let mut cells: BTreeMap<(i32, i32), Vec<&'a Entity>> = BTreeMap::new();
    for entity in entities {
      cells.entry(cell(entity.position)).or_default().push(entity);
    }
    Self { cells }
  }

  /// The entities in the 9 cells around `of`, `of` too, nearest first, and the lower id first at
  /// an equal distance. When more than 64 are there, it gives the 64 nearest.
  pub(crate) fn visible(&self, of: &Entity) -> Vec<Entity> {
    let (x, z) = cell(of.position);
    let mut seen: Vec<&Entity> = (x - 1..=x + 1)
      .flat_map(|x| (z - 1..=z + 1).filter_map(move |z| self.cells.get(&(x, z))))
      .flatten()
      .copied()
      .collect();
    seen.sort_by_key(|entity| (distance(of.position, entity.position), entity.id.0));
    seen.into_iter().take(SEEN).copied().collect()
  }
}

fn cell(at: Position) -> (i32, i32) {
  (at.x.div_euclid(CELL), at.z.div_euclid(CELL))
}

/// The square of the distance between two positions, in square centimetres.
fn distance(one: Position, other: Position) -> i64 {
  [one.x - other.x, one.y - other.y, one.z - other.z]
    .map(|axis| i64::from(axis).pow(2))
    .iter()
    .sum()
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{Angle, CharacterId, Entity, Position, Velocity};

  use super::Grid;

  fn at(id: i64, x: i32, z: i32) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x, y: 0, z },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    }
  }

  fn ids(entities: &[Entity]) -> Vec<i64> {
    entities.iter().map(|entity| entity.id.0).collect()
  }

  /// The player stands in the cell from 0 to 32 m on each axis.
  #[test]
  fn a_player_sees_the_9_cells_around_it_nearest_first() {
    let entities = [
      at(1, 100, 100),
      at(2, 6300, 100),
      at(3, -100, -100),
      at(4, 6500, 100),
      at(5, 100, -3100),
      at(6, 100, 300),
      at(7, 300, 100),
    ];
    let grid = Grid::new(&entities);
    assert_eq!(ids(&grid.visible(&entities[0])), [1, 6, 7, 3, 5, 2]);
  }

  #[test]
  fn a_player_in_a_crowd_sees_the_64_nearest() {
    let entities: Vec<Entity> =
      (0..70).map(|n| at(n, 1600 + i32::try_from(n).unwrap() * 10, 1600)).collect();
    let grid = Grid::new(&entities);
    assert_eq!(ids(&grid.visible(&entities[0])), (0..64).collect::<Vec<i64>>());
  }
}
