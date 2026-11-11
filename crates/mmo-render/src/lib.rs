//! The drawing of the client, with no window.
//!
//! - [`Model`] is a skinned model from a binary glTF: its mesh with the maps of its material, its
//!   skeleton and its clip `Idle`. [`Model::pose`] gives the matrices of the joints at a time.
//! - [`Gpu`] is a graphics device, and [`Renderer`] draws a [`Frame`] to a texture with it: the
//!   ground that `mmo-collide` makes, and the characters, lit by a [`Light`].

mod draw;
mod gpu;
mod model;
mod shape;
#[cfg(test)]
mod testing;

pub use draw::{Frame, Light, MAX_CHARACTERS, Placed, Renderer};
pub use gpu::{Gpu, GpuError};
pub use model::{Error, Image, Material, Mesh, Model, Vertex};

/// The character of the game, as a binary glTF.
pub const CHARACTER: &[u8] = include_bytes!("../assets/character.glb");
