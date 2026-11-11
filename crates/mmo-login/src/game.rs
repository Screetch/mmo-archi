//! The fixture of the tests of the crate: a Redis of each test alone, and its database.

use std::net::TcpListener;
use std::process::Stdio;

use mmo_protocol::{AccountId, Angle, FromLogin, Password, Position, ToLogin, ZoneId};
use mmo_store::{State, Store};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use crate::{Client, Login, World};

pub(crate) const START: State =
  State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };

/// A Redis of one test alone, which ends with the test, and a PostgreSQL database of the test.
/// `REDIS_SERVER` names the program of Redis, or else `redis-server` of the `PATH` runs.
pub(crate) struct Game {
  _process: Child,
  _log: Lines<BufReader<ChildStdout>>,
  port: u16,
  pub(crate) redis: ConnectionManager,
  pub(crate) store: Store,
  /// The URL of the database of the test, which the keeper migrated.
  pub(crate) database: String,
}

impl Game {
  pub(crate) async fn new(pool: PgPool) -> Self {
    let name = pool.connect_options().get_database().unwrap_or_default().to_owned();
    let base =
      std::env::var("DATABASE_URL").expect("DATABASE_URL names the PostgreSQL of the tests");
    let database = format!("{}/{name}", base.rsplit_once('/').expect("a URL with a database").0);
    let store = Store::migrate(pool).await.expect("the keeper migrates the database");
    store.add_zone(ZoneId(7), "meadow").await.expect("a zone");
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
    let redis = Self::connect(port, "default").await;
    Self { _process: process, _log: log, port, redis, store, database }
  }

  async fn connect(port: u16, user: &str) -> ConnectionManager {
    let url = format!("redis://{user}:any@127.0.0.1:{port}");
    let client = redis::Client::open(url).expect("a URL of Redis");
    ConnectionManager::new(client).await.expect("Redis accepts the user")
  }

  /// A copy of the login service of world 1 with a cap of `cap` accounts.
  pub(crate) fn copy(&self, cap: u32) -> Login {
    Login::new(self.store.clone(), self.redis.clone(), World { id: 1, cap })
  }

  /// A copy whose user of Redis has each command but those that the rule of ACL `denied` takes.
  pub(crate) async fn denied(&self, denied: &str) -> Login {
    let user = format!("denied{denied}");
    self.deny(&user, denied).await;
    let redis = Self::connect(self.port, &user).await;
    Login::new(self.store.clone(), redis, World { id: 1, cap: 1 })
  }

  /// Takes from `user` of Redis the commands that the rule of ACL `denied` names.
  pub(crate) async fn deny(&self, user: &str, denied: &str) {
    redis::cmd("ACL")
      .arg(&["SETUSER", user, "on", "nopass", "~*", "&*", "+@all", denied])
      .query_async::<()>(&mut self.redis.clone())
      .await
      .expect("Redis makes the user");
  }

  /// The URL of the Redis of the test.
  pub(crate) fn redis_url(&self) -> String {
    format!("redis://127.0.0.1:{}", self.port)
  }

  pub(crate) async fn account(&self, name: &str) -> AccountId {
    self.store.create_account(name, secret()).await.expect("a free name")
  }

  pub(crate) async fn enter(&self, copy: &Login, names: &[&str]) -> Vec<(Client, FromLogin)> {
    let mut entered = Vec::new();
    for name in names {
      let mut client = Client::default();
      let answer = copy.answer(&mut client, login(name)).await.expect("an answer");
      entered.push((client, answer));
    }
    entered
  }

  pub(crate) async fn waits(
    &self,
    copy: &Login,
    clients: &mut [(Client, FromLogin)],
  ) -> Vec<FromLogin> {
    let mut answers = Vec::new();
    for (client, _) in clients {
      answers.push(copy.answer(client, ToLogin::Wait).await.expect("an answer"));
    }
    answers
  }
}

pub(crate) fn secret() -> Password {
  Password("a long passphrase".to_owned())
}

pub(crate) fn login(name: &str) -> ToLogin {
  ToLogin::Login { name: name.to_owned(), password: secret() }
}

/// The position, the length and the wait of a `Queued` answer.
pub(crate) fn place(answer: &FromLogin) -> Option<(u32, u32, u32)> {
  match *answer {
    FromLogin::Queued { position, length, wait, .. } => Some((position, length, wait)),
    _ => None,
  }
}
