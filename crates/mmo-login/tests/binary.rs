//! The binary of the login service, as the process manager of the operator starts it.

use std::process::Command;

#[test]
fn the_binary_names_the_first_variable_that_it_lacks() {
  let ran = Command::new(env!("CARGO_BIN_EXE_mmo-login"))
    .env_remove("MMO_LOGIN_ADDRESS")
    .output()
    .expect("the binary runs");
  assert_eq!(
    (ran.status.code(), String::from_utf8_lossy(&ran.stderr).into_owned()),
    (Some(1), "Error: Unset(\"MMO_LOGIN_ADDRESS\")\n".to_owned())
  );
}
