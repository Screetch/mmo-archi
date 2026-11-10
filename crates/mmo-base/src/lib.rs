//! The base of the workspace.
//!
//! This crate holds no code of the game. It gives each gate a target, and the coverage a crate
//! that it can count, until the first crate of the game takes its place.

/// The name of the project that each crate of the workspace serves.
pub const NAME: &str = "mmo-archi";

/// Says that the project stands, with its name.
#[must_use]
pub fn greeting() -> String {
  format!("{NAME} stands")
}

#[cfg(test)]
mod tests {
  use super::{NAME, greeting};

  #[test]
  fn the_greeting_holds_the_name_of_the_project() {
    assert_eq!(greeting(), "mmo-archi stands");
    assert_eq!(NAME, "mmo-archi");
  }
}
