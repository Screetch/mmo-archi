//! The binary of the game client. `mmo_client::play` says its options and the variables of its
//! environment.

fn main() -> Result<(), mmo_client::Failure> {
  let args: Vec<String> = std::env::args().skip(1).collect();
  mmo_client::play(&args, &|name| std::env::var(name).ok())
}
