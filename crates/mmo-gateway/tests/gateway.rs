//! The binary of the gateway, as the process manager of the operator starts it.

use std::process::Command;

#[test]
fn the_gateway_stops_at_once_on_a_variable_that_is_not_set() {
  let output = Command::new(env!("CARGO_BIN_EXE_mmo-gateway"))
    .env_remove("GATEWAY_ADDRESS")
    .output()
    .expect("the gateway runs");
  assert_eq!(
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned()),
    (Some(1), "the gateway stops: the variable GATEWAY_ADDRESS is not set\n".to_owned())
  );
}
