//! The binary of the check, as the script of the local stack starts it.

use std::process::Command;

#[test]
fn the_binary_names_the_first_variable_that_it_lacks() {
  let ran = Command::new(env!("CARGO_BIN_EXE_mmo-check"))
    .env_remove("MMO_CHECK_AUTHORITY")
    .output()
    .expect("the binary runs");
  assert_eq!(
    (ran.status.code(), String::from_utf8_lossy(&ran.stderr).into_owned()),
    (Some(1), "Error: Unset(\"MMO_CHECK_AUTHORITY\")\n".to_owned())
  );
}

#[test]
fn the_game_binary_names_a_bad_option_and_then_the_first_variable_that_it_lacks() {
  let run = |args: &[&str]| {
    let ran = Command::new(env!("CARGO_BIN_EXE_mmo-play"))
      .args(args)
      .env_remove("MMO_PLAY_AUTHORITY")
      .output()
      .expect("the binary runs");
    (ran.status.code(), String::from_utf8_lossy(&ran.stderr).into_owned())
  };
  let option =
    "Error: Usage(\"--zoom is no option of the game: use --demo, or --frames <count>\")\n";
  assert_eq!(run(&["--zoom"]), (Some(1), option.to_owned()));
  assert_eq!(run(&[]), (Some(1), "Error: Unset(\"MMO_PLAY_AUTHORITY\")\n".to_owned()));
}
