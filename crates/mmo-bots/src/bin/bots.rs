//! The binary of the load test. `mmo_bots::run` says the variables of its environment.

#[expect(clippy::disallowed_methods, reason = "the entry of the binary starts the reactor once")]
#[tokio::main]
async fn main() -> Result<(), mmo_bots::Failure> {
  mmo_bots::run(&|name| std::env::var(name).ok()).await
}
