//! The static world that a body collides with.

use parry3d::math::{Pose, Vector};
use parry3d::query::{ShapeCastOptions, cast_shapes};
use parry3d::shape::{Capsule, SharedShape};

/// A part of a static world. The coordinates are in metres, and the axis y points up.
#[derive(Clone, Debug)]
pub enum Solid {
  /// A box whose faces are parallel to the axes.
  Cuboid {
    /// The point at the middle of the box.
    center: [f32; 3],
    /// Half of the size of the box along each axis.
    half_extents: [f32; 3],
  },
  /// A sphere.
  Ball {
    /// The point at the middle of the sphere.
    center: [f32; 3],
    /// The radius of the sphere.
    radius: f32,
  },
  /// A mesh of triangles, as the ground of a zone or a building.
  Mesh {
    /// The corners of the triangles.
    vertices: Vec<[f32; 3]>,
    /// The three indices in `vertices` of the corners of each triangle.
    triangles: Vec<[u32; 3]>,
  },
}

/// The reason why [`Terrain::new`] refuses its solids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TerrainError {
  /// A triangle of a mesh names a corner that the mesh does not hold.
  #[error("solid {solid} names the corner {index}, but it holds {vertices} corners")]
  Corner {
    /// The position of the mesh in the list of solids.
    solid: usize,
    /// The index that the mesh does not hold.
    index: u32,
    /// The count of the corners of the mesh.
    vertices: usize,
  },
}

/// The static world that each body collides with. Make it from the same solids on the server and
/// on each client, since the solids and their order go into each result of [`crate::step`].
pub struct Terrain {
  /// The simple solids in one compound, and the triangles of every mesh in one mesh. A compound
  /// of parry3d cannot hold a mesh.
  parts: Vec<SharedShape>,
}

/// The place where a cast of the capsule of a body stops.
pub(crate) struct Hit {
  /// The part of the motion that the capsule makes before it stops, from 0 to 1.
  pub(crate) fraction: f32,
  /// The normal of the surface that stops the capsule, which points away from the terrain.
  pub(crate) normal: Vector,
  /// The point of the surface that stops the capsule.
  pub(crate) point: Vector,
}

impl Terrain {
  /// Makes the static world of `solids`. An empty list gives a world with no solid, in which a body
  /// falls with no end.
  #[expect(
    clippy::expect_used,
    reason = "parry3d refuses only a mesh with no triangle, and no memory holds 2^32 corners"
  )]
  pub fn new(solids: &[Solid]) -> Result<Self, TerrainError> {
    let mut simple = Vec::new();
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (solid, part) in solids.iter().enumerate() {
      match part {
        Solid::Cuboid { center, half_extents: [x, y, z] } => {
          simple
            .push((Pose::from_translation(Vector::from(*center)), SharedShape::cuboid(*x, *y, *z)));
        }
        Solid::Ball { center, radius } => {
          simple.push((Pose::from_translation(Vector::from(*center)), SharedShape::ball(*radius)));
        }
        Solid::Mesh { vertices: corners, triangles: faces } => {
          let base =
            u32::try_from(vertices.len()).expect("a terrain holds fewer than 2^32 corners");
          for &index in faces.iter().flatten() {
            if index as usize >= corners.len() {
              return Err(TerrainError::Corner { solid, index, vertices: corners.len() });
            }
          }
          vertices.extend(corners.iter().copied().map(Vector::from));
          triangles.extend(faces.iter().map(|face| face.map(|index| base + index)));
        }
      }
    }
    let mut parts = Vec::new();
    if !simple.is_empty() {
      parts.push(SharedShape::compound(simple));
    }
    if !triangles.is_empty() {
      parts.push(
        SharedShape::trimesh(vertices, triangles).expect("a mesh with a triangle has no error"),
      );
    }
    Ok(Self { parts })
  }

  /// Casts `capsule`, whose center stands at `from`, along `motion`, and gives the first surface
  /// that stops it at the distance `skin`. A surface that the motion leaves, or that the motion
  /// goes along, stops nothing.
  #[expect(clippy::expect_used, reason = "parry3d casts a capsule against each part that it holds")]
  pub(crate) fn cast(
    &self,
    capsule: &Capsule,
    from: Vector,
    motion: Vector,
    skin: f32,
  ) -> Option<Hit> {
    let options = ShapeCastOptions {
      max_time_of_impact: 1.0,
      target_distance: skin,
      stop_at_penetration: false,
      compute_impact_geometry_on_penetration: true,
    };
    let at = Pose::from_translation(from);
    self
      .parts
      .iter()
      .filter_map(|part| {
        cast_shapes(&at, motion, capsule, &Pose::IDENTITY, Vector::ZERO, part.as_ref(), options)
          .expect("parry3d casts a capsule against a compound and a mesh")
      })
      .min_by(|one, other| one.time_of_impact.total_cmp(&other.time_of_impact))
      .map(|hit| Hit { fraction: hit.time_of_impact, normal: -hit.normal1, point: hit.witness2 })
  }
}

#[cfg(test)]
mod tests {
  use super::{Solid, Terrain, TerrainError};

  #[test]
  fn a_mesh_that_names_a_missing_corner_is_refused() {
    let solids = [
      Solid::Ball { center: [0.0, 0.0, 0.0], radius: 1.0 },
      Solid::Mesh {
        vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
      },
    ];
    let refused = Terrain::new(&solids).err();
    assert_eq!(refused, Some(TerrainError::Corner { solid: 1, index: 3, vertices: 3 }));
    assert_eq!(
      refused.map(|why| why.to_string()).as_deref(),
      Some("solid 1 names the corner 3, but it holds 3 corners")
    );
  }
}
