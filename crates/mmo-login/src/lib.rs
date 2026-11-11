//! The login service of mmo-archi: it checks a name and a password against `mmo-store`, keeps the
//! admission queue of each world in Redis, and gives a session.
//!
//! [`Login`] keeps no state of a client in its memory. Each copy reads and writes Redis, so any
//! copy serves any client, and a copy that stops loses no place in the queue. A [`Client`] is the
//! state of one connection, and a client that comes back to another copy makes it again with
//! [`ToLogin::Resume`], with no new check of its password.
//!
//! Each login enters the queue, also when the world has a free slot, so no login passes an account
//! that waits. [`Login::admit`] runs the admission once, and each copy runs it each second.

mod queue;

use mmo_protocol::{AccountId, CharacterId, FromLogin, Password, Refusal, ToLogin, Token};
use mmo_store::{Store, keys};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};

use queue::Place;
pub use queue::World;

/// The failed logins of one name in one window, after which the login service refuses the name
/// until the window ends.
const FAILURES: u32 = 5;

/// The window of the failed logins of a name, in seconds.
const FAILURE_WINDOW: u64 = 60;

/// The life of a session, in seconds.
const SESSION: i64 = 24 * 60 * 60;

/// A fault of a store, which ends the connection of the client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// PostgreSQL refuses a query, or cannot be reached.
  #[error(transparent)]
  Store(#[from] mmo_store::Error),
  /// Redis refuses a command, or cannot be reached.
  #[error(transparent)]
  Redis(#[from] redis::RedisError),
}

/// The state of the connection of one client.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Client {
  /// The client has no account yet, or its session is made.
  #[default]
  Out,
  /// The account waits in the queue, and the client comes back to its place with `resume`.
  Queued {
    /// The account of the client.
    account: AccountId,
    /// The token of the place.
    resume: Token,
  },
  /// The account has a slot, and the client selects one of `characters`.
  Admitted {
    /// The account of the client.
    account: AccountId,
    /// The characters that the client got in [`FromLogin::Characters`].
    characters: Vec<CharacterId>,
  },
}

/// One copy of the login service of one world.
#[derive(Clone)]
pub struct Login {
  store: Store,
  redis: ConnectionManager,
  world: World,
}

impl Login {
  /// A copy that reads the accounts in `store` and keeps the queue of `world` in `redis`.
  pub fn new(store: Store, redis: ConnectionManager, world: World) -> Self {
    Self { store, redis, world }
  }

  /// Runs the admission of the world once: it renews each slot that ended while the presence of
  /// its account stands, ends each other, then admits the head of the queue into the free slots,
  /// 100 each second at most, and skips each account whose client stopped its waits. It gives the
  /// count of the accounts that it admitted.
  pub async fn admit(&self) -> Result<u32, Error> {
    self.world.admit(&mut self.redis.clone()).await
  }

  /// The answer to `message`, which moves `client` to its next state.
  pub async fn answer(&self, client: &mut Client, message: ToLogin) -> Result<FromLogin, Error> {
    match message {
      ToLogin::Login { name, password } => self.login(client, &name, password).await,
      ToLogin::Resume { token } => self.resume(client, token).await,
      ToLogin::Wait => match *client {
        Client::Queued { account, resume } => self.place(client, account, resume, false).await,
        Client::Admitted { account, .. } => self.admitted(client, account).await,
        Client::Out => Ok(Self::expired(client)),
      },
      ToLogin::Select { character } => self.select(client, character).await,
    }
  }

  /// The characters of the account of a queued `client` once the admission gave it a slot, and
  /// else nothing. The copy that serves the client asks it each second.
  pub async fn ready(&self, client: &mut Client) -> Result<Option<FromLogin>, Error> {
    let Client::Queued { account, .. } = *client else {
      return Ok(None);
    };
    if !self.redis.clone().exists::<_, bool>(keys::admit(account)).await? {
      return Ok(None);
    }
    self.admitted(client, account).await.map(Some)
  }

  async fn login(
    &self,
    client: &mut Client,
    name: &str,
    password: Password,
  ) -> Result<FromLogin, Error> {
    let mut redis = self.redis.clone();
    let failed = keys::failed(name);
    if redis.get::<_, Option<u32>>(&failed).await?.unwrap_or(0) >= FAILURES {
      return Ok(FromLogin::Refused(Refusal::Throttled));
    }
    let Some(account) = self.store.sign_in(name, password).await? else {
      let window = SetOptions::default()
        .conditional_set(ExistenceCheck::NX)
        .with_expiration(SetExpiry::EX(FAILURE_WINDOW));
      redis::pipe()
        .atomic()
        .set_options(&failed, 0, window)
        .ignore()
        .incr(&failed, 1)
        .ignore()
        .exec_async(&mut redis)
        .await?;
      return Ok(FromLogin::Refused(Refusal::Password));
    };
    self.place(client, account, token(), true).await
  }

  async fn resume(&self, client: &mut Client, token: Token) -> Result<FromLogin, Error> {
    match self.redis.clone().get::<_, Option<i64>>(keys::resume(&token)).await? {
      Some(account) => self.place(client, AccountId(account), token, false).await,
      None => Ok(Self::expired(client)),
    }
  }

  async fn place(
    &self,
    client: &mut Client,
    account: AccountId,
    resume: Token,
    enter: bool,
  ) -> Result<FromLogin, Error> {
    let mut redis = self.redis.clone();
    match self.world.place(&mut redis, account, &resume, enter).await? {
      Place::Admitted => self.admitted(client, account).await,
      Place::Online => {
        *client = Client::Out;
        Ok(FromLogin::Refused(Refusal::Online))
      }
      Place::Gone => Ok(Self::expired(client)),
      Place::Queued { position, length, wait } => {
        *client = Client::Queued { account, resume };
        Ok(FromLogin::Queued { position, length, wait, resume })
      }
    }
  }

  async fn admitted(&self, client: &mut Client, account: AccountId) -> Result<FromLogin, Error> {
    let listed = self.store.characters(account).await?;
    let characters = listed.iter().map(|one| one.id).collect();
    *client = Client::Admitted { account, characters };
    Ok(FromLogin::Characters(listed))
  }

  /// Makes the session of `character`. The slot of the account gives one session at most, so two
  /// clients of one account never hold two sessions.
  async fn select(&self, client: &mut Client, character: CharacterId) -> Result<FromLogin, Error> {
    let Client::Admitted { account, characters } = client else {
      return Ok(Self::expired(client));
    };
    let (account, owned) = (*account, characters.contains(&character));
    let mut redis = self.redis.clone();
    if redis.exists(keys::presence(account)).await? {
      return Ok(FromLogin::Refused(Refusal::Online));
    }
    let saved = if owned { self.store.load(&[character]).await? } else { Vec::new() };
    let Some(saved) = saved.first() else {
      return Ok(FromLogin::Refused(Refusal::Character));
    };
    if !redis.del::<_, bool>(keys::admit(account)).await? {
      return Ok(Self::expired(client));
    }
    let token = token();
    let session = keys::session(&token);
    let fields =
      [("account", account.0), ("character", character.0), ("zone", saved.zone.0.into())];
    redis::pipe()
      .atomic()
      .hset_multiple(&session, &fields)
      .ignore()
      .expire(&session, SESSION)
      .ignore()
      .exec_async(&mut redis)
      .await?;
    *client = Client::Out;
    Ok(FromLogin::Session { token })
  }

  fn expired(client: &mut Client) -> FromLogin {
    *client = Client::Out;
    FromLogin::Refused(Refusal::Expired)
  }
}

#[expect(clippy::expect_used, reason = "a server whose system gives no random bytes cannot run")]
fn token() -> Token {
  let mut bytes = [0; 32];
  getrandom::fill(&mut bytes).expect("the random source of the system gives 32 bytes");
  Token(bytes)
}

#[cfg(test)]
mod tests {
  use std::net::TcpListener;
  use std::process::Stdio;

  use mmo_protocol::{
    AccountId, Angle, Character, CharacterId, FromLogin, Password, Position, Refusal, ToLogin,
    Token, ZoneId,
  };
  use mmo_store::{State, Store, keys};
  use redis::AsyncCommands;
  use redis::aio::ConnectionManager;
  use sqlx::PgPool;
  use tokio::io::{AsyncBufReadExt, BufReader, Lines};
  use tokio::process::{Child, ChildStdout, Command};

  use super::{Client, Error, Login, World};

  const START: State =
    State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };

  /// A Redis of one test alone, which ends with the test, and a PostgreSQL database of the test.
  /// `REDIS_SERVER` names the program of Redis, or else `redis-server` of the `PATH` runs.
  struct Game {
    _process: Child,
    _log: Lines<BufReader<ChildStdout>>,
    port: u16,
    redis: ConnectionManager,
    store: Store,
  }

  impl Game {
    async fn new(pool: PgPool) -> Self {
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
      Self { _process: process, _log: log, port, redis, store }
    }

    async fn connect(port: u16, user: &str) -> ConnectionManager {
      let url = format!("redis://{user}:any@127.0.0.1:{port}");
      let client = redis::Client::open(url).expect("a URL of Redis");
      ConnectionManager::new(client).await.expect("Redis accepts the user")
    }

    /// A copy of the login service of world 1 with a cap of `cap` accounts.
    fn copy(&self, cap: u32) -> Login {
      Login::new(self.store.clone(), self.redis.clone(), World { id: 1, cap })
    }

    /// A copy whose user of Redis has each command but those that the rule of ACL `denied` takes.
    async fn denied(&self, denied: &str) -> Login {
      let user = format!("denied{denied}");
      redis::cmd("ACL")
        .arg(&["SETUSER", &user, "on", "nopass", "~*", "&*", "+@all", denied])
        .query_async::<()>(&mut self.redis.clone())
        .await
        .expect("Redis makes the user");
      let redis = Self::connect(self.port, &user).await;
      Login::new(self.store.clone(), redis, World { id: 1, cap: 1 })
    }

    async fn account(&self, name: &str) -> AccountId {
      self.store.create_account(name, secret()).await.expect("a free name")
    }

    async fn enter(&self, copy: &Login, names: &[&str]) -> Vec<(Client, FromLogin)> {
      let mut entered = Vec::new();
      for name in names {
        let mut client = Client::default();
        let answer = copy.answer(&mut client, login(name)).await.expect("an answer");
        entered.push((client, answer));
      }
      entered
    }

    async fn waits(&self, copy: &Login, clients: &mut [(Client, FromLogin)]) -> Vec<FromLogin> {
      let mut answers = Vec::new();
      for (client, _) in clients {
        answers.push(copy.answer(client, ToLogin::Wait).await.expect("an answer"));
      }
      answers
    }
  }

  fn secret() -> Password {
    Password("a long passphrase".to_owned())
  }

  fn login(name: &str) -> ToLogin {
    ToLogin::Login { name: name.to_owned(), password: secret() }
  }

  /// The position, the length and the wait of a `Queued` answer.
  fn place(answer: &FromLogin) -> Option<(u32, u32, u32)> {
    match *answer {
      FromLogin::Queued { position, length, wait, .. } => Some((position, length, wait)),
      _ => None,
    }
  }

  #[sqlx::test(migrations = false)]
  async fn the_order_of_admission_is_the_order_of_entry(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let names = ["ada", "bob", "cy", "dee"];
    for name in names {
      game.account(name).await;
    }
    let mut clients = game.enter(&game.copy(2), &names).await;
    let entered: Vec<_> = clients.iter().map(|(_, answer)| place(answer)).collect();
    let none = u32::MAX;
    assert_eq!(
      entered,
      [Some((0, 1, none)), Some((1, 2, none)), Some((2, 3, none)), Some((3, 4, none))]
    );
    assert_eq!(game.copy(2).admit().await?, 2);
    let answers = game.waits(&game.copy(2), &mut clients).await;
    let places: Vec<_> = answers.iter().map(place).collect();
    assert_eq!(places, [None, None, Some((0, 2, 300)), Some((1, 2, 600))]);
    assert_eq!(
      answers[..2],
      [FromLogin::Characters(Vec::new()), FromLogin::Characters(Vec::new())]
    );
    assert_eq!(game.copy(3).admit().await?, 1);
    let ready =
      [game.copy(3).ready(&mut clients[2].0).await?, game.copy(3).ready(&mut clients[3].0).await?];
    assert_eq!(ready, [Some(FromLogin::Characters(Vec::new())), None]);
    let last = game.copy(3).answer(&mut clients[3].0, ToLogin::Wait).await?;
    assert_eq!(place(&last), Some((0, 1, 200)));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_restart_of_a_copy_loses_no_place(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let names = ["ada", "bob", "cy"];
    for name in names {
      game.account(name).await;
    }
    let mut tokens = Vec::new();
    for (_, answer) in game.enter(&game.copy(0), &names).await {
      if let FromLogin::Queued { resume, .. } = answer {
        tokens.push(resume);
      }
    }
    let mut places = Vec::new();
    for token in tokens.iter().rev() {
      let mut client = Client::default();
      let answer = game.copy(0).answer(&mut client, ToLogin::Resume { token: *token }).await?;
      if let FromLogin::Queued { position, length, resume, .. } = answer {
        places.push((position, length, resume));
      }
    }
    let expected: Vec<_> = (0..3).rev().map(|at| (at, 3, tokens[at as usize])).collect();
    assert_eq!(places, expected);
    let mut stranger = Client::default();
    let unknown = ToLogin::Resume { token: Token([0; 32]) };
    let unknown = game.copy(0).answer(&mut stranger, unknown).await?;
    assert_eq!(unknown, FromLogin::Refused(Refusal::Expired));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn the_admission_skips_a_silent_client_and_renews_a_slot_with_a_presence(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let accounts = [game.account("ada").await, game.account("bob").await, game.account("cy").await];
    let mut clients = game.enter(&game.copy(5), &["ada", "bob", "cy"]).await;
    let mut redis = game.redis.clone();
    let () = redis.del(keys::queued(accounts[1])).await?;
    assert_eq!(game.copy(5).admit().await?, 2);
    let answers = game.waits(&game.copy(5), &mut clients).await;
    assert_eq!(
      answers,
      [
        FromLogin::Characters(Vec::new()),
        FromLogin::Refused(Refusal::Expired),
        FromLogin::Characters(Vec::new())
      ]
    );
    assert_eq!(clients[1].0, Client::Out);
    let admitted = keys::admitted(1);
    let () = redis.zadd_multiple(&admitted, &[(0, accounts[0].0), (0, accounts[2].0)]).await?;
    let () = redis.hset(keys::presence(accounts[0]), "zone", 7).await?;
    assert_eq!(game.copy(5).admit().await?, 0);
    let slots: Vec<(i64, i64)> = redis.zrange_withscores(&admitted, 0, -1).await?;
    let held: Vec<_> = slots.iter().map(|(account, end)| (*account, *end > 30_000)).collect();
    assert_eq!(held, [(accounts[0].0, true)]);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_name_with_5_failed_logins_waits_for_its_window(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool).await;
    game.account("ada").await;
    let copy = game.copy(1);
    let mut client = Client::default();
    let wrong = ToLogin::Login { name: "ADA".to_owned(), password: Password("guess".to_owned()) };
    let mut answers = Vec::new();
    for _ in 0..5 {
      answers.push(copy.answer(&mut client, wrong.clone()).await?);
    }
    answers.push(copy.answer(&mut client, login("ada")).await?);
    let mut expected = vec![FromLogin::Refused(Refusal::Password); 5];
    expected.push(FromLogin::Refused(Refusal::Throttled));
    assert_eq!(answers, expected);
    let window: i64 = game.redis.clone().ttl(keys::failed("ada")).await?;
    assert!((1..=60).contains(&window), "the window ends in {window} s");
    assert_eq!(client, Client::Out);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn an_online_account_and_a_client_with_no_place_get_no_slot(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let () = game.redis.clone().hset(keys::presence(ada), "zone", 7).await?;
    let copy = game.copy(1);
    let mut client = Client::default();
    let answers = [
      copy.answer(&mut client, login("ada")).await?,
      copy.answer(&mut client, ToLogin::Wait).await?,
      copy.answer(&mut client, ToLogin::Select { character: CharacterId(1) }).await?,
    ];
    assert_eq!(
      answers,
      [
        FromLogin::Refused(Refusal::Online),
        FromLogin::Refused(Refusal::Expired),
        FromLogin::Refused(Refusal::Expired)
      ]
    );
    assert_eq!(copy.ready(&mut client).await?, None);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_slot_gives_one_session_of_a_character_of_the_account(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let bob = game.account("bob").await;
    let brom = game.store.create_character(ada, "Brom", ZoneId(7), START).await?;
    let dara = game.store.create_character(bob, "Dara", ZoneId(7), START).await?;
    let copy = game.copy(1);
    let mut clients = game.enter(&copy, &["ada", "ada"]).await;
    copy.admit().await?;
    let listed = [copy.ready(&mut clients[0].0).await?, copy.ready(&mut clients[1].0).await?];
    let characters = FromLogin::Characters(vec![Character { id: brom, name: "Brom".to_owned() }]);
    assert_eq!(listed, [Some(characters.clone()), Some(characters.clone())]);
    assert_eq!(copy.answer(&mut clients[1].0, ToLogin::Wait).await?, characters);
    let mut redis = game.redis.clone();
    let () = redis.hset(keys::presence(ada), "zone", 7).await?;
    let first = &mut clients[0].0;
    let online = copy.answer(first, ToLogin::Select { character: brom }).await?;
    let () = redis.del(keys::presence(ada)).await?;
    let foreign = copy.answer(first, ToLogin::Select { character: dara }).await?;
    let made = copy.answer(first, ToLogin::Select { character: brom }).await?;
    let second = copy.answer(&mut clients[1].0, ToLogin::Select { character: brom }).await?;
    assert_eq!(
      [online, foreign, second],
      [
        FromLogin::Refused(Refusal::Online),
        FromLogin::Refused(Refusal::Character),
        FromLogin::Refused(Refusal::Expired)
      ]
    );
    let FromLogin::Session { token } = made else {
      panic!("a session, and not {made:?}");
    };
    let session = keys::session(&token);
    let mut fields: Vec<(String, i64)> = redis.hgetall(&session).await?;
    fields.sort();
    assert_eq!(
      fields,
      [("account".to_owned(), ada.0), ("character".to_owned(), brom.0), ("zone".to_owned(), 7)]
    );
    let life: i64 = redis.ttl(&session).await?;
    assert!((86_390..=86_400).contains(&life), "the session ends in {life} s");
    assert_eq!([&clients[0].0, &clients[1].0], [&Client::Out; 2]);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_fault_of_a_store_ends_the_answer_with_its_error(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool.clone()).await;
    let ada = game.account("ada").await;
    let brom = game.store.create_character(ada, "Brom", ZoneId(7), START).await?;
    let copy = game.copy(1);
    let mut queued = Client::default();
    copy.answer(&mut queued, login("ada")).await?;
    copy.admit().await?;
    let mut admitted = queued.clone();
    copy.ready(&mut admitted).await?;
    let closed = PgPool::connect_with(pool.connect_options().as_ref().clone()).await;
    let closed = closed.map_err(mmo_store::Error::from)?;
    let unreachable =
      Login::new(Store::open(closed.clone()).await?, game.redis.clone(), World { id: 1, cap: 1 });
    closed.close().await;
    let [no_get, no_set, no_script, no_exists, no_del, no_hmset] = [
      game.denied("-get").await,
      game.denied("-set").await,
      game.denied("-@scripting").await,
      game.denied("-exists").await,
      game.denied("-del").await,
      game.denied("-hmset").await,
    ];
    let wrong = ToLogin::Login { name: "ada".to_owned(), password: Password("guess".to_owned()) };
    let select = ToLogin::Select { character: brom };
    let resume = ToLogin::Resume { token: Token([0; 32]) };
    let faults = [
      no_get.answer(&mut Client::Out, login("ada")).await.err(),
      unreachable.answer(&mut Client::Out, login("ada")).await.err(),
      no_set.answer(&mut Client::Out, wrong).await.err(),
      no_script.answer(&mut Client::Out, login("ada")).await.err(),
      no_script.admit().await.err(),
      no_get.answer(&mut Client::Out, resume).await.err(),
      unreachable.ready(&mut queued.clone()).await.err(),
      no_exists.ready(&mut queued.clone()).await.err(),
      no_exists.answer(&mut admitted.clone(), select.clone()).await.err(),
      unreachable.answer(&mut admitted.clone(), select.clone()).await.err(),
      no_del.answer(&mut admitted.clone(), select.clone()).await.err(),
      no_hmset.answer(&mut admitted.clone(), select).await.err(),
    ];
    let texts: Vec<_> =
      faults.iter().map(|fault| fault.as_ref().map(ToString::to_string)).collect();
    let denied = |user: &str, command: &str| {
      Some(format!("NoPerm: User denied-{user} has no permissions to run the '{command}' command"))
    };
    let aborted = |user: &str, command: &str| {
      denied(user, command).map(|text| format!("Transaction aborted: [(Index 0, error: {text})]"))
    };
    let closed = Some("attempted to acquire a connection on a closed pool".to_owned());
    assert_eq!(
      texts,
      [
        denied("get", "get"),
        closed.clone(),
        aborted("set", "set"),
        denied("@scripting", "evalsha"),
        denied("@scripting", "evalsha"),
        denied("get", "get"),
        closed.clone(),
        denied("exists", "exists"),
        denied("exists", "exists"),
        closed,
        denied("del", "del"),
        aborted("hmset", "hmset"),
      ]
    );
    Ok(())
  }
}
