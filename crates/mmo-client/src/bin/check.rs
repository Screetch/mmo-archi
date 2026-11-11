//! The binary of the check of a local stack. `mmo_client::check` says the variables of its
//! environment.

#[expect(clippy::disallowed_methods, reason = "the entry of the binary starts the reactor once")]
#[tokio::main]
async fn main() -> Result<(), mmo_client::Failure> {
  mmo_client::check(&|name| std::env::var(name).ok()).await
}
