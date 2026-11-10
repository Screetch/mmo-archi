//! The binary of the keeper, as the process manager of the operator starts it.

use std::process::Command;

#[test]
fn the_keeper_stops_at_once_on_a_variable_that_is_not_set() {
  let output = Command::new(env!("CARGO_BIN_EXE_mmo-keeper"))
    .env_remove("KEEPER_ADDRESS")
    .output()
    .expect("the keeper runs");
  assert_eq!(
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned()),
    (Some(1), "the keeper stops: the variable KEEPER_ADDRESS is not set\n".to_owned())
  );
}
