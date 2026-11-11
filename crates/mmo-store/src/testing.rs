//! A Redis of one test alone, for the tests of each crate that reads or writes the keys of Redis.

#![expect(clippy::expect_used, reason = "a fixture of tests: a fault ends the test that needs it")]

use std::net::TcpListener;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

/// A Redis that ends with its value. `REDIS_SERVER` names the program of Redis, or else
/// `redis-server` of the `PATH` runs.
#[derive(Debug)]
pub struct Redis {
  _process: Child,
  _log: Lines<BufReader<ChildStdout>>,
  port: u16,
}

impl Redis {
  /// Starts a Redis on a free port of the loopback, and waits until it takes connections.
  pub async fn start() -> Self {
    let port = TcpListener::bind("127.0.0.1:0")
      .and_then(|free| free.local_addr())
      .expect("the system gives a free port")
      .port();
    let program = std::env::var("REDIS_SERVER").unwrap_or(String::from("redis-server"));
    let mut process = Command::new(program)
      .args(["--port", &port.to_string(), "--bind", "127.0.0.1", "--save", ""])
      .args(["--appendonly", "no"])
      .stdout(Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .expect("REDIS_SERVER, or redis-server of the PATH, starts");
    let mut log = BufReader::new(process.stdout.take().expect("a piped log")).lines();
    while let Some(line) = log.next_line().await.expect("the log of Redis reads") {
      if line.contains("Ready to accept connections") {
        break;
      }
    }
    Self { _process: process, _log: log, port }
  }

  /// The port of the Redis.
  pub fn port(&self) -> u16 {
    self.port
  }

  /// The URL of the Redis.
  pub fn url(&self) -> String {
    format!("redis://127.0.0.1:{}", self.port)
  }
}
