//! The binary of the layer writer, as the process manager of the operator starts it.

use std::process::Command;

#[test]
fn the_layer_writer_stops_at_once_on_a_variable_that_is_not_set() {
  let output = Command::new(env!("CARGO_BIN_EXE_mmo-zone"))
    .env_remove("ZONE_ID")
    .output()
    .expect("the layer writer runs");
  assert_eq!(
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned()),
    (Some(1), "the layer writer stops: the variable ZONE_SHARD is not set\n".to_owned())
  );
}
