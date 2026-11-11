//! The ground of a zone, which the zone writer and each client build in the same way.

use crate::Solid;

/// The side of the square of a zone, in metres. Its corner is at the origin.
const SIDE: f32 = 1024.0;

/// The blocks on the floor: the x and the z of the middle of each, and its height, in metres. Each
/// is 4 m wide on the axes x and z.
const BLOCKS: [[f32; 3]; 4] =
  [[500.0, 500.0, 2.0], [540.0, 520.0, 0.3], [470.0, 560.0, 1.0], [600.0, 450.0, 3.0]];

/// The solids of the ground of each zone: a flat square floor, with a few blocks on it.
///
/// The zone writer collides its players with this ground, and a client draws it and predicts its
/// own body on it, so both read one list. A change of the list changes the result of a step.
pub fn ground() -> Vec<Solid> {
  let half = SIDE / 2.0;
  let floor = Solid::Cuboid { center: [half, -0.5, half], half_extents: [half, 0.5, half] };
  let blocks = BLOCKS.map(|[x, z, top]| Solid::Cuboid {
    center: [x, top / 2.0, z],
    half_extents: [2.0, top / 2.0, 2.0],
  });
  [floor].into_iter().chain(blocks).collect()
}

#[cfg(test)]
mod tests {
  use super::ground;
  use crate::{Body, Input, Terrain, step};

  fn settle(position: [i32; 3]) -> Body {
    let terrain = Terrain::new(&ground()).expect("the ground is valid");
    (0..40).fold(Body { position, vertical_speed: 0 }, |body, _| step(&terrain, body, Input::Idle))
  }

  #[test]
  fn a_body_that_falls_lands_on_the_floor_or_on_the_top_of_a_block() {
    assert_eq!(settle([10_000, 800, 10_000]).position, [10_000, 0, 10_000]);
    assert_eq!(settle([50_000, 800, 50_000]).position, [50_000, 200, 50_000]);
    assert_eq!(settle([60_000, 800, 45_000]).position, [60_000, 300, 45_000]);
  }
}
