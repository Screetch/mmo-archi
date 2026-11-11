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
//! - [`enter`] signs in to the login service and waits in its queue, and [`Play`] links to a
//!   gateway: snapshots and messages come down, and intents go up.
//! - [`check`] proves a stack on one machine: it makes an account, and waits for the first snapshot
//!   that shows its player. The binary `mmo-check` runs it.
//! - [`Camera`] orbits and zooms around the player, and [`Keys`] turns the keys of the player into
//!   a heading on the ground, from the view of the camera.

mod camera;
mod check;
mod connect;
mod fault;
mod input;
mod login;
mod play;
mod predict;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod world;

pub use camera::Camera;
pub use check::{Failure, check};
pub use connect::Trust;
pub use fault::Fault;
pub use input::Keys;
pub use login::{Account, Place, enter};
pub use play::{Down, Play, Up};
pub use predict::Predictor;
pub use world::{Gap, Seen, World};
