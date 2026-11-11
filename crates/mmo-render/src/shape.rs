//! The mesh of the solids of a ground, and the look of that ground.

use core::f32::consts::{PI, TAU};

use glam::Vec3;
use mmo_collide::Solid;

use crate::model::{Image, Material, Mesh, Vertex};

/// The side of one square of the maps of the ground, in metres.
const TILE: f32 = 2.0;
/// The count of rings of a sphere, from its top to its bottom.
const RINGS: u16 = 12;
/// The count of sectors of a sphere around its axis y.
const SECTORS: u16 = 24;

/// The faces of a box, as the direction of its normal and the direction of its tangent.
const FACES: [([f32; 3], [f32; 3]); 6] = [
  ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
  ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
  ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
  ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
  ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
  ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
];

/// The mesh of all the solids, which stand in the rest pose and move with no joint. The maps
/// repeat each `TILE` metres on each surface.
pub(crate) fn ground(solids: &[Solid]) -> Mesh {
  let mut builder = Builder::default();
  for solid in solids {
    match solid {
      Solid::Cuboid { center, half_extents } => {
        builder.cuboid(Vec3::from(*center), Vec3::from(*half_extents));
      }
      Solid::Ball { center, radius } => builder.ball(Vec3::from(*center), *radius),
      Solid::Mesh { vertices, triangles } => builder.triangles(vertices, triangles),
    }
  }
  Mesh { vertices: builder.vertices, indices: builder.indices, material: material() }
}

/// Matt earth, with a map of two shades and a flat relief.
fn material() -> Material {
  let pixel = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], 255];
  let (light, dark) = (pixel([92, 132, 74]), pixel([80, 118, 66]));
  Material {
    base_color: Image { width: 2, height: 2, rgba: [light, dark, dark, light].concat() },
    normal: Image { width: 1, height: 1, rgba: vec![128, 128, 255, 255] },
    metallic: 0.0,
    roughness: 0.9,
  }
}

#[derive(Default)]
struct Builder {
  vertices: Vec<Vertex>,
  indices: Vec<u32>,
  count: u32,
}

impl Builder {
  fn point(&mut self, position: Vec3, normal: Vec3, tangent: Vec3) -> u32 {
    let bitangent = normal.cross(tangent);
    self.vertices.push(Vertex {
      position: position.to_array(),
      normal: normal.to_array(),
      uv: [position.dot(tangent) / TILE, -position.dot(bitangent) / TILE],
      tangent: tangent.extend(1.0).to_array(),
      joints: [0; 4],
      weights: [1.0, 0.0, 0.0, 0.0],
    });
    self.count += 1;
    self.count - 1
  }

  /// Two triangles for the corners of a quad, which go counter-clockwise from outside.
  fn quad(&mut self, corners: [Vec3; 4], normal: Vec3, tangent: Vec3) {
    let [a, b, c, d] = corners.map(|corner| self.point(corner, normal, tangent));
    self.indices.extend([a, b, c, a, c, d]);
  }

  fn cuboid(&mut self, center: Vec3, half: Vec3) {
    for (normal, tangent) in FACES {
      let (normal, tangent) = (Vec3::from(normal), Vec3::from(tangent));
      let bitangent = normal.cross(tangent);
      let corner = |t: f32, b: f32| center + half * (normal + tangent * t + bitangent * b);
      self.quad(
        [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)],
        normal,
        tangent,
      );
    }
  }

  fn ball(&mut self, center: Vec3, radius: f32) {
    let first = self.count;
    for ring in 0..=RINGS {
      let theta = PI * f32::from(ring) / f32::from(RINGS);
      for sector in 0..=SECTORS {
        let phi = TAU * f32::from(sector) / f32::from(SECTORS);
        let normal = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
        self.point(center + normal * radius, normal, Vec3::new(-phi.sin(), 0.0, phi.cos()));
      }
    }
    let width = u32::from(SECTORS) + 1;
    for ring in 0..RINGS {
      for sector in 0..SECTORS {
        let at = first + u32::from(ring) * width + u32::from(sector);
        let below = at + width;
        self.indices.extend([at, at + 1, below + 1, at, below + 1, below]);
      }
    }
  }

  /// Each triangle has its own flat normal.
  fn triangles(&mut self, vertices: &[[f32; 3]], triangles: &[[u32; 3]]) {
    for triangle in triangles {
      let [a, b, c] = triangle.map(|index| Vec3::from(vertices[index as usize]));
      let normal = (b - a).cross(c - a).normalize_or_zero();
      let tangent = (b - a).normalize_or_zero();
      let corners = [a, b, c].map(|corner| self.point(corner, normal, tangent));
      self.indices.extend(corners);
    }
  }
}

#[cfg(test)]
mod tests {
  use glam::Vec3;
  use mmo_collide::Solid;

  use super::{RINGS, SECTORS, TILE, ground};
  use crate::model::Mesh;

  fn point(mesh: &Mesh, index: u32) -> Vec3 {
    Vec3::from(mesh.vertices[index as usize].position)
  }

  /// The triangles of the mesh, as the corners of each.
  fn triangles(mesh: &Mesh) -> Vec<[Vec3; 3]> {
    mesh.indices.chunks(3).map(|corners| [0, 1, 2].map(|at| point(mesh, corners[at]))).collect()
  }

  /// How many triangles face away from `center`, and how many face it.
  fn facing(mesh: &Mesh, center: Vec3) -> (usize, usize) {
    let sides = triangles(mesh)
      .into_iter()
      .map(|[a, b, c]| (b - a).cross(c - a).dot((a + b + c) / 3.0 - center));
    (sides.clone().filter(|side| *side < -1e-6).count(), sides.filter(|side| *side > 1e-6).count())
  }

  fn near(one: f32, other: f32) -> bool {
    (one - other).abs() < 1e-4
  }

  #[test]
  fn a_cuboid_has_six_faces_that_face_outward_with_a_tangent_on_each() {
    let mesh = ground(&[Solid::Cuboid { center: [1.0, 2.0, 3.0], half_extents: [0.5, 1.0, 2.0] }]);
    assert_eq!((mesh.vertices.len(), mesh.indices.len()), (24, 36));
    assert_eq!(facing(&mesh, Vec3::new(1.0, 2.0, 3.0)), (0, 12));
    for vertex in &mesh.vertices {
      let (normal, tangent) = (Vec3::from(vertex.normal), Vec3::from_slice(&vertex.tangent));
      assert!(
        near(normal.length(), 1.0) && near(tangent.length(), 1.0) && near(normal.dot(tangent), 0.0)
      );
      assert_eq!(
        (vertex.tangent[3], vertex.joints, vertex.weights),
        (1.0, [0; 4], [1.0, 0.0, 0.0, 0.0])
      );
    }
  }

  #[test]
  fn the_maps_repeat_each_tile_along_the_tangent_of_a_face() {
    let mesh = ground(&[Solid::Cuboid { center: [0.0, 0.0, 0.0], half_extents: [1.0, 1.0, 1.0] }]);
    let top: Vec<_> =
      mesh.vertices.iter().filter(|vertex| Vec3::from(vertex.normal) == Vec3::Y).collect();
    let (least, most) = top.iter().fold((f32::MAX, f32::MIN), |(least, most), vertex| {
      (least.min(vertex.uv[0]), most.max(vertex.uv[0]))
    });
    assert!(near(most - least, 2.0 / TILE));
  }

  #[test]
  fn a_ball_is_a_sphere_of_unit_normals_that_faces_outward() {
    let mesh = ground(&[Solid::Ball { center: [0.0, 5.0, 0.0], radius: 2.0 }]);
    let (rings, sectors) = (usize::from(RINGS), usize::from(SECTORS));
    assert_eq!(
      (mesh.vertices.len(), mesh.indices.len()),
      ((rings + 1) * (sectors + 1), 6 * rings * sectors)
    );
    for vertex in &mesh.vertices {
      let place = Vec3::from(vertex.position) - Vec3::new(0.0, 5.0, 0.0);
      assert!(near(place.length(), 2.0) && near(Vec3::from(vertex.normal).length(), 1.0));
    }
    let (inward, outward) = facing(&mesh, Vec3::new(0.0, 5.0, 0.0));
    assert_eq!(inward, 0);
    assert!(outward > 0);
  }

  #[test]
  fn each_triangle_of_a_mesh_has_its_flat_normal_and_a_flat_one_has_none() {
    let solid = Solid::Mesh {
      vertices: vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [1.0, 1.0, 1.0]],
      triangles: vec![[0, 1, 2], [3, 3, 3]],
    };
    let mesh = ground(&[solid]);
    assert_eq!(mesh.indices, [0, 1, 2, 3, 4, 5]);
    assert!(
      mesh.vertices[..3].iter().all(|vertex| Vec3::from(vertex.normal).abs_diff_eq(Vec3::Z, 1e-6))
    );
    assert!(mesh.vertices[3..].iter().all(|vertex| Vec3::from(vertex.normal) == Vec3::ZERO));
  }

  #[test]
  fn the_ground_is_matt_with_a_map_of_two_shades_and_a_flat_relief() {
    let material = ground(&[]).material;
    assert_eq!((material.metallic, material.roughness), (0.0, 0.9));
    assert_eq!(material.base_color.rgba.len(), 16);
    assert_ne!(material.base_color.rgba[..4], material.base_color.rgba[4..8]);
    assert_eq!(material.normal.rgba, [128, 128, 255, 255]);
  }
}
