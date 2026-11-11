//! The client of mmo-archi, without a window.
//!
//! The client decides nothing: the zone writer is the one authority of the world. The client only
//! predicts its own body, to show it at once, and corrects the prediction when a snapshot of the
//! writer comes.
//!
//! - [`Predictor`] runs the step of `mmo-collide` on the inputs of the player, and runs the
//!   inputs that the writer did not answer yet again, from each snapshot.
//! - [`World`] holds the entities of the snapshots, and gives the entities that other players
//!   move, between two snapshots.
//! - [`Camera`] orbits and zooms around the player, and [`Keys`] turns the keys of the player into
//!   a heading on the ground, from the view of the camera.

mod camera;
mod input;
mod predict;
mod world;

pub use camera::Camera;
pub use input::Keys;
pub use predict::Predictor;
pub use world::{Gap, Seen, World};
