//! A Redis of one test alone, for the tests of each crate that reads or writes the keys of Redis.

#![expect(clippy::expect_used, reason = "a fixture of tests: a fault ends the test that needs it")]

use std::net::TcpListener;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

/// How many times `Redis::start` takes a free port and starts Redis on it.
const ATTEMPTS: usize = 3;

/// How many lines of its log a Redis that did not start shows.
const LOG_LINES: usize = 10;

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
  ///
  /// Another process can take the port between the moment that it is free and the moment that
  /// Redis binds it. Then Redis ends, and the start runs again on a new port.
  ///
  /// # Panics
  ///
  /// When no attempt gives a Redis that takes connections. The message holds the last lines of
  /// the log.
  pub async fn start() -> Self {
    let program = std::env::var("REDIS_SERVER").unwrap_or(String::from("redis-server"));
    Self::start_with(&program, ATTEMPTS).await
  }

  async fn start_with(program: &str, attempts: usize) -> Self {
    let mut last = Vec::new();
    for _ in 0..attempts {
      match Self::launch(program, free_port()).await {
        Ok(redis) => return redis,
        Err(log) => last = log,
      }
    }
    panic!("Redis did not start in {attempts} attempts. The end of its log:\n{}", last.join("\n"));
  }

  /// Starts Redis on `port`. When its log ends before it takes connections, the last lines of the
  /// log are the error.
  async fn launch(program: &str, port: u16) -> Result<Self, Vec<String>> {
    let mut process = Command::new(program)
      .args(["--port", &port.to_string(), "--bind", "127.0.0.1", "--save", ""])
      .args(["--appendonly", "no"])
      .stdout(Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .expect("REDIS_SERVER, or redis-server of the PATH, starts");
    let mut log = BufReader::new(process.stdout.take().expect("a piped log")).lines();
    let mut seen = Vec::new();
    while let Some(line) = log.next_line().await.expect("the log of Redis reads") {
      if line.contains("Ready to accept connections") {
        return Ok(Self { _process: process, _log: log, port });
      }
      seen.push(line);
    }
    Err(seen.split_off(seen.len().saturating_sub(LOG_LINES)))
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

fn free_port() -> u16 {
  TcpListener::bind("127.0.0.1:0")
    .and_then(|free| free.local_addr())
    .expect("the system gives a free port")
    .port()
}

#[cfg(test)]
mod tests {
  use super::Redis;

  // The test program ends at once on the arguments of Redis, with no line of readiness, as a
  // Redis does that cannot bind its port.
  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_program_that_never_takes_connections_ends_the_start_after_its_attempts() {
    let program = std::env::current_exe().expect("the test binary has a path");
    let program = program.to_str().expect("the path is text").to_owned();
    let fault = tokio::spawn(async move { Redis::start_with(&program, 2).await })
      .await
      .expect_err("the start panics");
    let message = fault.into_panic().downcast::<String>().expect("a formatted message");
    assert!(message.starts_with("Redis did not start in 2 attempts."), "{message}");
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_started_redis_gives_its_port_in_its_url() {
    let redis = Redis::start().await;
    assert_eq!(redis.url(), format!("redis://127.0.0.1:{}", redis.port()));
  }
}
