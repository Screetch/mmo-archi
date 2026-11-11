//! A skinned model from a binary glTF, and its pose at a time of its clip `Idle`.

use glam::{Mat4, Quat, Vec3};
use gltf::animation::Interpolation;
use gltf::animation::util::ReadOutputs;
use gltf::image::{Data, Format};
use gltf::{Document, Node, Skin, buffer};

/// What a binary glTF lacks, or holds in a form that the client does not draw.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The bytes are no valid binary glTF.
  #[error("the model is not a valid binary glTF: {0}")]
  Gltf(#[from] gltf::Error),
  /// No node of the model has both a skin and a mesh.
  #[error("no node of the model has both a skin and a mesh")]
  NoSkin,
  /// The mesh of the skinned node has no primitive, or more than one.
  #[error("the mesh of the model must hold exactly one primitive")]
  Primitives,
  /// The primitive lacks an attribute that the shader reads.
  #[error("the mesh has no {0}")]
  Missing(&'static str),
  /// The material lacks a map that the shader reads.
  #[error("the material has no {0} map")]
  NoMap(&'static str),
  /// A map is not an image of 8 bits for each of red, green, blue and alpha.
  #[error("the maps must be images of 8 bits for each of red, green, blue and alpha")]
  Format,
  /// The skin does not give one inverse bind matrix to each joint.
  #[error("the skin must give one inverse bind matrix to each joint")]
  Skin,
  /// The model has no clip named Idle that moves a joint.
  #[error("the model has no clip named Idle that moves a joint")]
  NoIdle,
  /// A channel of the clip Idle cannot be played.
  #[error("a channel of Idle cannot be played: {0}")]
  Clip(&'static str),
}

/// The point of a mesh, as the shader reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
  /// The place in the rest pose, in metres.
  pub position: [f32; 3],
  /// The direction that leaves the surface.
  pub normal: [f32; 3],
  /// The place on the maps.
  pub uv: [f32; 2],
  /// The direction of the axis u of the maps, and in w the side of the axis v: 1 or -1.
  pub tangent: [f32; 4],
  /// The four joints that move the point.
  pub joints: [u16; 4],
  /// How much each of the four joints moves the point. The four add up to 1.
  pub weights: [f32; 4],
}

/// An image of 8 bits for each of red, green, blue and alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
  /// The count of pixels of a row.
  pub width: u32,
  /// The count of rows.
  pub height: u32,
  /// The pixels, row by row, four bytes each.
  pub rgba: Vec<u8>,
}

/// How a surface looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
  /// The colour of the surface. A colour map in sRGB.
  pub base_color: Image,
  /// The relief of the surface, as a normal map in the space of the tangent.
  pub normal: Image,
  /// How much of the surface is metal, from 0 to 1.
  pub metallic: f32,
  /// How rough the surface is, from 0 (a mirror) to 1.
  pub roughness: f32,
}

/// The triangles of a model, with their material.
#[derive(Clone, Debug, PartialEq)]
pub struct Mesh {
  /// The points.
  pub vertices: Vec<Vertex>,
  /// Three points for each triangle, counter-clockwise from the side that the normals face.
  pub indices: Vec<u32>,
  /// How the triangles look.
  pub material: Material,
}

/// The place of a joint relative to its parent: the scale, then the rotation, then the move.
#[derive(Clone, Copy, Debug)]
struct Local {
  translation: Vec3,
  rotation: Quat,
  scale: Vec3,
}

impl Local {
  fn matrix(self) -> Mat4 {
    Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
  }
}

#[derive(Clone, Debug)]
struct Joint {
  parent: Option<usize>,
  rest: Local,
  inverse_bind: Mat4,
}

#[derive(Clone, Debug)]
enum Track {
  Translation(Vec<Vec3>),
  Rotation(Vec<Quat>),
  Scale(Vec<Vec3>),
}

#[derive(Clone, Debug)]
struct Channel {
  joint: usize,
  times: Vec<f32>,
  track: Track,
}

/// A skinned model: its mesh, its skeleton and its clip `Idle`.
///
/// The clip loops. Only the joints of the skin move, and a node above the root joint is taken as
/// the identity, so a model faces the way that its rest pose says.
#[derive(Clone, Debug)]
pub struct Model {
  /// The triangles with their material.
  pub mesh: Mesh,
  joints: Vec<Joint>,
  idle: Vec<Channel>,
  duration: f32,
}

impl Model {
  /// Reads a binary glTF with one skinned mesh of one primitive. The primitive holds the
  /// attributes `POSITION`, `NORMAL`, `TEXCOORD_0`, `TANGENT`, `JOINTS_0` and `WEIGHTS_0` and its
  /// indices, and
  /// its material holds a colour map and a normal map of 8 bits each. The model holds a clip named
  /// `Idle` that moves joints with linear keys.
  pub fn from_glb(bytes: &[u8]) -> Result<Self, Error> {
    let (document, buffers, images) = gltf::import_slice(bytes)?;
    let (skin, mesh) =
      document.nodes().find_map(|node| Some((node.skin()?, node.mesh()?))).ok_or(Error::NoSkin)?;
    let mesh = mesh_of(&mesh, &buffers, &images)?;
    let joints = joints(&skin, &buffers)?;
    let (idle, duration) = idle(&document, &skin, &buffers)?;
    Ok(Self { mesh, joints, idle, duration })
  }

  /// The count of joints, which is the count of matrices that [`Model::pose`] gives.
  pub fn joints(&self) -> usize {
    self.joints.len()
  }

  /// The matrix of each joint at `seconds` of the clip `Idle`, which takes a point of the rest
  /// pose to its place. The clip loops.
  pub fn pose(&self, seconds: f32) -> Vec<Mat4> {
    let time = seconds.rem_euclid(self.duration);
    let mut local: Vec<Local> = self.joints.iter().map(|joint| joint.rest).collect();
    for channel in &self.idle {
      let place = &mut local[channel.joint];
      match &channel.track {
        Track::Translation(values) => {
          place.translation = sample(&channel.times, values, time, Vec3::lerp);
        }
        Track::Rotation(values) => {
          place.rotation = sample(&channel.times, values, time, Quat::slerp);
        }
        Track::Scale(values) => place.scale = sample(&channel.times, values, time, Vec3::lerp),
      }
    }
    (0..local.len())
      .map(|joint| self.world(&local, joint) * self.joints[joint].inverse_bind)
      .collect()
  }

  fn world(&self, local: &[Local], joint: usize) -> Mat4 {
    let own = local[joint].matrix();
    self.joints[joint].parent.map_or(own, |parent| self.world(local, parent) * own)
  }
}

/// The value at `time` between the two keys that hold it. `times` rises, holds at least 2 keys,
/// and `time` lies in its range.
fn sample<T: Copy>(times: &[f32], values: &[T], time: f32, blend: fn(T, T, f32) -> T) -> T {
  let next = times.partition_point(|&key| key <= time).clamp(1, times.len() - 1);
  let (from, to) = (times[next - 1], times[next]);
  blend(values[next - 1], values[next], ((time - from) / (to - from)).clamp(0.0, 1.0))
}

#[expect(
  clippy::expect_used,
  reason = "the importer refuses a primitive that has no POSITION, so the reader finds it"
)]
fn mesh_of(mesh: &gltf::Mesh, buffers: &[buffer::Data], images: &[Data]) -> Result<Mesh, Error> {
  let mut primitives = mesh.primitives();
  let (Some(primitive), None) = (primitives.next(), primitives.next()) else {
    return Err(Error::Primitives);
  };
  let reader = primitive.reader(|data| buffers.get(data.index()).map(|data| &data.0[..]));
  let positions = reader.read_positions().expect("each primitive has a POSITION");
  let normals = reader.read_normals().ok_or(Error::Missing("NORMAL"))?;
  let uvs = reader.read_tex_coords(0).ok_or(Error::Missing("TEXCOORD_0"))?.into_f32();
  let tangents = reader.read_tangents().ok_or(Error::Missing("TANGENT"))?;
  let joints = reader.read_joints(0).ok_or(Error::Missing("JOINTS_0"))?.into_u16();
  let weights = reader.read_weights(0).ok_or(Error::Missing("WEIGHTS_0"))?.into_f32();
  let indices = reader.read_indices().ok_or(Error::Missing("indices"))?.into_u32().collect();
  let vertices = positions
    .zip(normals)
    .zip(uvs)
    .zip(tangents)
    .zip(joints)
    .zip(weights)
    .map(|(((((position, normal), uv), tangent), joints), weights)| Vertex {
      position,
      normal,
      uv,
      tangent,
      joints,
      weights,
    })
    .collect();
  let material = primitive.material();
  let pbr = material.pbr_metallic_roughness();
  let base = pbr.base_color_texture().ok_or(Error::NoMap("colour"))?;
  let normal = material.normal_texture().ok_or(Error::NoMap("normal"))?;
  Ok(Mesh {
    vertices,
    indices,
    material: Material {
      base_color: image(images, base.texture().source().index())?,
      normal: image(images, normal.texture().source().index())?,
      metallic: pbr.metallic_factor(),
      roughness: pbr.roughness_factor(),
    },
  })
}

fn image(images: &[Data], index: usize) -> Result<Image, Error> {
  let data = &images[index];
  (data.format == Format::R8G8B8A8)
    .then(|| Image { width: data.width, height: data.height, rgba: data.pixels.clone() })
    .ok_or(Error::Format)
}

fn joints(skin: &Skin, buffers: &[buffer::Data]) -> Result<Vec<Joint>, Error> {
  let nodes: Vec<Node> = skin.joints().collect();
  let reader = skin.reader(|data| buffers.get(data.index()).map(|data| &data.0[..]));
  let inverse: Vec<Mat4> = reader
    .read_inverse_bind_matrices()
    .ok_or(Error::Skin)?
    .map(|matrix| Mat4::from_cols_array_2d(&matrix))
    .collect();
  if inverse.len() != nodes.len() {
    return Err(Error::Skin);
  }
  let parent = |node: &Node| {
    nodes.iter().position(|other| other.children().any(|child| child.index() == node.index()))
  };
  Ok(
    nodes
      .iter()
      .zip(inverse)
      .map(|(node, inverse_bind)| {
        let (translation, rotation, scale) = node.transform().decomposed();
        Joint {
          parent: parent(node),
          rest: Local {
            translation: Vec3::from(translation),
            rotation: Quat::from_array(rotation),
            scale: Vec3::from(scale),
          },
          inverse_bind,
        }
      })
      .collect(),
  )
}

#[expect(
  clippy::expect_used,
  reason = "the importer checked each accessor and each buffer, so the readers find their data"
)]
fn idle(
  document: &Document,
  skin: &Skin,
  buffers: &[buffer::Data],
) -> Result<(Vec<Channel>, f32), Error> {
  let clip = document
    .animations()
    .find(|clip| clip.name() == Some("Idle") && clip.channels().next().is_some())
    .ok_or(Error::NoIdle)?;
  let nodes: Vec<usize> = skin.joints().map(|node| node.index()).collect();
  let mut channels = Vec::new();
  for channel in clip.channels() {
    if channel.sampler().interpolation() != Interpolation::Linear {
      return Err(Error::Clip("only linear keys play"));
    }
    let joint = nodes
      .iter()
      .position(|&node| node == channel.target().node().index())
      .ok_or(Error::Clip("it moves a node that is no joint of the skin"))?;
    let reader = channel.reader(|data| buffers.get(data.index()).map(|data| &data.0[..]));
    let times: Vec<f32> =
      reader.read_inputs().expect("the importer checked the accessor of the times").collect();
    let track =
      match reader.read_outputs().expect("the importer checked the accessor of the values") {
        ReadOutputs::Translations(values) => Track::Translation(values.map(Vec3::from).collect()),
        ReadOutputs::Rotations(values) => {
          Track::Rotation(values.into_f32().map(Quat::from_array).collect())
        }
        ReadOutputs::Scales(values) => Track::Scale(values.map(Vec3::from).collect()),
        ReadOutputs::MorphTargetWeights(_) => return Err(Error::Clip("it moves morph targets")),
      };
    let values = match &track {
      Track::Translation(values) | Track::Scale(values) => values.len(),
      Track::Rotation(values) => values.len(),
    };
    if times.len() < 2 || times.len() != values {
      return Err(Error::Clip("it needs 2 keys or more, and one value for each time"));
    }
    channels.push(Channel { joint, times, track });
  }
  let duration = channels
    .iter()
    .filter_map(|channel| channel.times.last())
    .fold(0.0, |most, &time| f32::max(most, time));
  Ok((channels, duration))
}

#[cfg(test)]
mod tests {
  use glam::{Mat4, Vec3, Vec4};
  use serde_json::{Value, json};

  use super::{Error, Model};
  use crate::CHARACTER;

  fn split(glb: &[u8]) -> (Value, Vec<u8>) {
    let size = |at: usize| {
      usize::try_from(u32::from_le_bytes(glb[at..at + 4].try_into().expect("4 bytes")))
        .expect("a chunk fits memory")
    };
    let text = size(12);
    let document = serde_json::from_slice(&glb[20..20 + text]).expect("the json chunk parses");
    (document, glb[28 + text..28 + text + size(20 + text)].to_vec())
  }

  fn join(mut document: Value, mut binary: Vec<u8>) -> Vec<u8> {
    binary.resize(binary.len().next_multiple_of(4), 0);
    document["buffers"][0]["byteLength"] = json!(binary.len());
    let mut text = serde_json::to_vec(&document).expect("the document serialises");
    text.resize(text.len().next_multiple_of(4), b' ');
    let length = |bytes: &[u8]| u32::try_from(bytes.len()).expect("a chunk fits 4 GiB");
    let mut glb = Vec::from(*b"glTF");
    glb.extend(2_u32.to_le_bytes());
    glb.extend((28 + length(&text) + length(&binary)).to_le_bytes());
    glb.extend(length(&text).to_le_bytes());
    glb.extend(*b"JSON");
    glb.extend(text);
    glb.extend(length(&binary).to_le_bytes());
    glb.extend(*b"BIN\0");
    glb.extend(binary);
    glb
  }

  /// The character, after `change` edits its document and its binary chunk.
  fn changed(change: impl FnOnce(&mut Value, &mut Vec<u8>)) -> Result<Model, Error> {
    let (mut document, mut binary) = split(CHARACTER);
    change(&mut document, &mut binary);
    Model::from_glb(&join(document, binary))
  }

  fn refused(change: impl FnOnce(&mut Value, &mut Vec<u8>)) -> String {
    changed(change).expect_err("the model is refused").to_string()
  }

  fn character() -> Model {
    Model::from_glb(CHARACTER).expect("the character loads")
  }

  #[test]
  fn the_character_loads_with_its_mesh_its_maps_and_its_skeleton() {
    let model = character();
    assert_eq!(model.mesh.vertices.len(), 7 * 24);
    assert_eq!(model.mesh.indices.len(), 7 * 36);
    assert_eq!(model.joints(), 7);
    let material = &model.mesh.material;
    assert_eq!((material.base_color.width, material.base_color.height), (16, 16));
    assert_eq!(material.normal.rgba.len(), 16 * 16 * 4);
    assert_eq!((material.metallic, material.roughness), (0.2, 0.6));
    let first = model.mesh.vertices[0];
    assert_eq!(first.joints, [0, 0, 0, 0]);
    assert!(first.weights.iter().zip([1.0, 0.0, 0.0, 0.0]).all(|(a, b)| (a - b).abs() < 1e-6));
    assert!((first.tangent[3] - 1.0).abs() < 1e-6);
  }

  #[test]
  fn the_rest_pose_moves_no_point_and_the_clip_loops() {
    let model = character();
    for matrix in model.pose(0.0) {
      assert!(matrix.abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }
    let middle = model.pose(1.0);
    let hips = middle[0].w_axis;
    assert!(hips.abs_diff_eq(Vec4::new(0.0, 0.01, 0.0, 1.0), 1e-5));
    let breath = 3.0_f32.to_radians();
    let head = middle[2].transform_point3(Vec3::new(0.0, 1.6, 0.0));
    let tilted = Vec3::new(0.0, 1.21 + 0.4 * breath.cos(), 0.4 * breath.sin());
    assert!(head.abs_diff_eq(tilted, 1e-4), "{head} against {tilted}");
    assert!(model.pose(0.5)[0].abs_diff_eq(model.pose(2.5)[0], 1e-5));
    assert!(model.pose(0.5)[0].abs_diff_eq(model.pose(-1.5)[0], 1e-5));
  }

  #[test]
  fn a_bad_file_and_a_model_with_no_skinned_mesh_are_refused() {
    let bad = Model::from_glb(b"not a model").expect_err("not a glTF").to_string();
    assert!(bad.starts_with("the model is not a valid binary glTF: "), "{bad}");
    let unskinned = refused(|document, _| {
      document["nodes"][7].as_object_mut().expect("a node").remove("skin");
    });
    assert_eq!(unskinned, "no node of the model has both a skin and a mesh");
    let meshless = refused(|document, _| {
      document["nodes"][7].as_object_mut().expect("a node").remove("mesh");
    });
    assert_eq!(meshless, unskinned);
    let double = refused(|document, _| {
      let primitive = document["meshes"][0]["primitives"][0].clone();
      document["meshes"][0]["primitives"] = json!([primitive.clone(), primitive]);
    });
    assert_eq!(double, "the mesh of the model must hold exactly one primitive");
  }

  #[test]
  fn each_attribute_that_the_shader_reads_is_needed() {
    for name in ["NORMAL", "TEXCOORD_0", "TANGENT", "JOINTS_0", "WEIGHTS_0"] {
      let said = refused(|document, _| {
        document["meshes"][0]["primitives"][0]["attributes"]
          .as_object_mut()
          .expect("attributes")
          .remove(name);
      });
      assert_eq!(said, format!("the mesh has no {name}"));
    }
    let said = refused(|document, _| {
      document["meshes"][0]["primitives"][0]
        .as_object_mut()
        .expect("a primitive")
        .remove("indices");
    });
    assert_eq!(said, "the mesh has no indices");
  }

  fn rgb_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let picture = image::RgbImage::new(1, 1);
    picture
      .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
      .expect("the picture encodes");
    bytes
  }

  #[test]
  fn the_material_needs_its_two_maps_as_images_of_four_channels() {
    let colour = refused(|document, _| {
      let pbr = document["materials"][0]["pbrMetallicRoughness"].as_object_mut();
      pbr.expect("a pbr material").remove("baseColorTexture");
    });
    assert_eq!(colour, "the material has no colour map");
    let normal = refused(|document, _| {
      document["materials"][0].as_object_mut().expect("a material").remove("normalTexture");
    });
    assert_eq!(normal, "the material has no normal map");
    for map in 0..2 {
      let flat = refused(|document, binary| {
        let png = rgb_png();
        let views = document["bufferViews"].as_array_mut().expect("views");
        views.push(json!({"buffer": 0, "byteOffset": binary.len(), "byteLength": png.len()}));
        document["images"][map]["bufferView"] = json!(views.len() - 1);
        binary.extend(png);
      });
      assert_eq!(flat, "the maps must be images of 8 bits for each of red, green, blue and alpha");
    }
  }

  #[test]
  fn the_skin_needs_one_inverse_bind_matrix_for_each_joint() {
    let said = "the skin must give one inverse bind matrix to each joint";
    let none = refused(|document, _| {
      document["skins"][0].as_object_mut().expect("a skin").remove("inverseBindMatrices");
    });
    assert_eq!(none, said);
    let short = refused(|document, _| document["skins"][0]["joints"] = json!([0, 1, 2, 3, 4, 5]));
    assert_eq!(short, said);
  }

  fn output(document: &Value, channel: usize) -> usize {
    let at = &document["animations"][0]["samplers"][channel]["output"];
    usize::try_from(at.as_u64().expect("an accessor")).expect("an index")
  }

  #[test]
  fn the_clip_idle_must_be_there_and_play_with_linear_keys() {
    let none = "the model has no clip named Idle that moves a joint";
    assert_eq!(refused(|document, _| document["animations"][0]["name"] = json!("Walk")), none);
    assert_eq!(refused(|document, _| document["animations"][0]["channels"] = json!([])), none);
    let cannot = |why: &str| format!("a channel of Idle cannot be played: {why}");
    let step = refused(|document, _| {
      document["animations"][0]["samplers"][0]["interpolation"] = json!("STEP");
    });
    assert_eq!(step, cannot("only linear keys play"));
    let foreign = refused(|document, _| {
      document["animations"][0]["channels"][0]["target"]["node"] = json!(7);
    });
    assert_eq!(foreign, cannot("it moves a node that is no joint of the skin"));
    let lone = refused(|document, _| {
      let times = document["animations"][0]["samplers"][0]["input"].as_u64().expect("times");
      document["accessors"][usize::try_from(times).expect("an index")]["count"] = json!(1);
    });
    assert_eq!(lone, cannot("it needs 2 keys or more, and one value for each time"));
  }

  #[test]
  fn a_clip_that_moves_morph_targets_is_refused() {
    let cannot = |why: &str| format!("a channel of Idle cannot be played: {why}");
    let morph = refused(|document, _| {
      let at = output(document, 4);
      document["accessors"][at]["type"] = json!("SCALAR");
      document["animations"][0]["channels"][4]["target"]["path"] = json!("weights");
    });
    assert_eq!(morph, cannot("it moves morph targets"));
  }

  #[test]
  fn a_clip_can_scale_a_joint() {
    let model = changed(|document, _| {
      document["animations"][0]["channels"][4]["target"]["path"] = json!("scale");
    })
    .expect("a scale plays");
    assert!((model.pose(1.0)[0].y_axis.y - 0.96).abs() < 1e-5);
  }
}
