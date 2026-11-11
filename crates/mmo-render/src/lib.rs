//! The drawing of the client, with no window.
//!
//! - [`Model`] is a skinned model from a binary glTF: its mesh with the maps of its material, its
//!   skeleton and its clip `Idle`. [`Model::pose`] gives the matrices of the joints at a time.

mod model;

pub use model::{Error, Image, Material, Mesh, Model, Vertex};

/// The character of the game, as a binary glTF.
pub const CHARACTER: &[u8] = include_bytes!("../assets/character.glb");
