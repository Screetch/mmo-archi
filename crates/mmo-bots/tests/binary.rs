//! The binary of the load test, as the script of the local stack starts it.

use std::process::Command;

#[test]
fn the_binary_names_the_first_variable_that_it_lacks() {
  let ran = Command::new(env!("CARGO_BIN_EXE_mmo-bots"))
    .env_remove("MMO_BOTS_AUTHORITY")
    .output()
    .expect("the binary runs");
  assert_eq!(
    (ran.status.code(), String::from_utf8_lossy(&ran.stderr).into_owned()),
    (Some(1), "Error: Config(Unset(\"MMO_BOTS_AUTHORITY\"))\n".to_owned())
  );
}
