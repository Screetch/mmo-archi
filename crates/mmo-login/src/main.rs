//! The binary of the login service. `mmo_login::run` says the variables of its environment.

#[expect(clippy::disallowed_methods, reason = "the entry of the binary starts the reactor once")]
#[tokio::main]
async fn main() -> Result<(), mmo_login::Fault> {
  mmo_login::run(&|name| std::env::var(name).ok(), Box::pin(tokio::signal::ctrl_c())).await
}
