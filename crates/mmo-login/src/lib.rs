//! The login service of mmo-archi: it makes the accounts and the characters in `mmo-store`, checks
//! a name and a password, keeps the admission queue of each world in Redis, and gives a session.
//!
//! [`Login`] keeps no state of a client in its memory. Each copy reads and writes Redis, so any
//! copy serves any client, and a copy that stops loses no place in the queue. A [`Client`] is the
//! state of one connection, and a client that comes back to another copy makes it again with
//! [`ToLogin::Resume`], with no new check of its password.
//!
//! Each login enters the queue, also when the world has a free slot, so no login passes an account
//! that waits. [`Login::admit`] runs the admission once, and each copy runs it each second.

#[cfg(test)]
mod game;
mod queue;
mod server;

use std::net::IpAddr;
use std::ops::RangeInclusive;

use mmo_protocol::{
  AccountId, Angle, CharacterId, FromLogin, Password, Position, Refusal, State, ToLogin, Token,
  ZoneId,
};
use mmo_store::{Store, keys};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};

use queue::Place;
pub use queue::World;
pub use server::{Fault, Stop, run};

/// The failed logins of one name from one address in one window, after which the login service
/// refuses the name to that address until the window ends. The name stays open to each other
/// address, so a stranger cannot lock an account out.
const FAILURES: u32 = 5;

/// The window of the failed logins of a name from an address, in seconds.
const FAILURE_WINDOW: u64 = 60;

/// The failed logins of one address over all names in one window, after which the login service
/// refuses each login of the address until the window ends, so a guesser of many names stays slow.
const GUESSES: u32 = 30;

/// The window of the failed logins of an address, in seconds.
const GUESS_WINDOW: u64 = 10 * 60;

/// The new accounts of one address in one window, after which the login service refuses each new
/// account of the address until the window ends.
const CREATIONS: u32 = 5;

/// The window of the new accounts of an address, in seconds.
const CREATION_WINDOW: u64 = 60 * 60;

/// The count of characters in the name of a new account or of a new character.
const NAME: RangeInclusive<usize> = 3..=16;

/// The count of characters that the password of a new account holds at least.
const PASSWORD_MIN: usize = 8;

/// The count of bytes that the password of a new account holds at most, which bounds the input of
/// one hash.
const PASSWORD_MAX: usize = 128;

/// The hit points of a new character.
const HIT_POINTS: i32 = 100;

/// The life of a session, in seconds.
const SESSION: i64 = 24 * 60 * 60;

/// The place where each new character of a world starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
  /// The zone of each new character.
  pub zone: ZoneId,
  /// The point of the zone where each new character stands.
  pub position: Position,
}

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
  start: Start,
}

impl Login {
  /// A copy that keeps the accounts and the characters in `store`, keeps the queue of `world` in
  /// `redis`, and makes each new character at `start`.
  pub fn new(store: Store, redis: ConnectionManager, world: World, start: Start) -> Self {
    Self { store, redis, world, start }
  }

  /// Runs the admission of the world once: it renews each slot that ended while the presence of
  /// its account stands, ends each other, then admits the head of the queue into the free slots,
  /// 100 each second at most, and skips each account whose client stopped its waits. It gives the
  /// count of the accounts that it admitted.
  pub async fn admit(&self) -> Result<u32, Error> {
    self.world.admit(&mut self.redis.clone()).await
  }

  /// The answer to `message` of a client at the address `from`, which moves `client` to its next
  /// state. The throttles of new accounts and of failed logins count by `from`.
  pub async fn answer(
    &self,
    client: &mut Client,
    from: IpAddr,
    message: ToLogin,
  ) -> Result<FromLogin, Error> {
    match message {
      ToLogin::NewAccount { name, password } => {
        self.new_account(client, from, &name, password).await
      }
      ToLogin::Login { name, password } => self.login(client, from, &name, password).await,
      ToLogin::Resume { token } => self.resume(client, token).await,
      ToLogin::Wait => match *client {
        Client::Queued { account, resume } => self.place(client, account, resume, false).await,
        Client::Admitted { account, .. } => self.admitted(client, account).await,
        Client::Out => Ok(Self::expired(client)),
      },
      ToLogin::NewCharacter { name } => self.new_character(client, &name).await,
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

  /// Makes the account of `name` and enters it in the queue, as a login does. Each try of an
  /// address with a valid name and a valid password counts, also a try of a taken name, so the
  /// throttle also slows a search of the names that exist.
  async fn new_account(
    &self,
    client: &mut Client,
    from: IpAddr,
    name: &str,
    password: Password,
  ) -> Result<FromLogin, Error> {
    if !NAME.contains(&name.len()) || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
      return Ok(FromLogin::Refused(Refusal::Name));
    }
    if password.0.chars().count() < PASSWORD_MIN || password.0.len() > PASSWORD_MAX {
      return Ok(FromLogin::Refused(Refusal::Weak));
    }
    let created = [(keys::created(from), CREATION_WINDOW)];
    if counted(&mut self.redis.clone(), &created).await?.iter().any(|&made| made > CREATIONS) {
      return Ok(FromLogin::Refused(Refusal::Throttled));
    }
    match self.store.create_account(name, password).await {
      Ok(account) => self.place(client, account, token(), true).await,
      Err(mmo_store::Error::Taken { .. }) => Ok(FromLogin::Refused(Refusal::Taken)),
      Err(fault) => Err(fault.into()),
    }
  }

  /// Signs in to the account of `name` and enters it in the queue. A failed login counts for the
  /// pair of `name` and `from`, and for `from` over all names.
  async fn login(
    &self,
    client: &mut Client,
    from: IpAddr,
    name: &str,
    password: Password,
  ) -> Result<FromLogin, Error> {
    let mut redis = self.redis.clone();
    let (failed, guessed) = (keys::failed(from, name), keys::guessed(from));
    let (pair, address): (Option<u32>, Option<u32>) =
      redis::pipe().get(&failed).get(&guessed).query_async(&mut redis).await?;
    if pair.unwrap_or(0) >= FAILURES || address.unwrap_or(0) >= GUESSES {
      return Ok(FromLogin::Refused(Refusal::Throttled));
    }
    let Some(account) = self.store.sign_in(name, password).await? else {
      counted(&mut redis, &[(failed, FAILURE_WINDOW), (guessed, GUESS_WINDOW)]).await?;
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

  /// Makes a character of the admitted account at the start of the world, and lists the characters
  /// of the account again. The name is written with its first letter in upper case and the rest
  /// in lower case, so two names never differ by their case alone. An account that holds 8
  /// characters gets no more: the list of the client answers at once, and the store decides for
  /// two clients of one account that make a character together.
  async fn new_character(&self, client: &mut Client, name: &str) -> Result<FromLogin, Error> {
    let Client::Admitted { account, characters } = client else {
      return Ok(Self::expired(client));
    };
    let (account, held) = (*account, characters.len());
    if !NAME.contains(&name.len()) || !name.bytes().all(|byte| byte.is_ascii_alphabetic()) {
      return Ok(FromLogin::Refused(Refusal::Name));
    }
    if !self.redis.clone().exists::<_, bool>(keys::admit(account)).await? {
      return Ok(Self::expired(client));
    }
    if held >= mmo_store::CHARACTERS {
      return Ok(FromLogin::Refused(Refusal::Limit));
    }
    let name = name[..1].to_ascii_uppercase() + &name[1..].to_ascii_lowercase();
    let state = State { position: self.start.position, yaw: Angle(0), hit_points: HIT_POINTS };
    match self.store.create_character(account, &name, self.start.zone, state).await {
      Ok(_) => self.admitted(client, account).await,
      Err(mmo_store::Error::Full { .. }) => Ok(FromLogin::Refused(Refusal::Limit)),
      Err(mmo_store::Error::Taken { .. }) => Ok(FromLogin::Refused(Refusal::Taken)),
      Err(fault) => Err(fault.into()),
    }
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

/// Counts one more event in each counter of `counters`, a key and the window in seconds that the
/// first event of the key starts, and gives the count of each.
async fn counted(
  redis: &mut ConnectionManager,
  counters: &[(String, u64)],
) -> Result<Vec<u32>, Error> {
  let mut pipe = redis::pipe();
  pipe.atomic();
  for (key, window) in counters {
    let window = SetOptions::default()
      .conditional_set(ExistenceCheck::NX)
      .with_expiration(SetExpiry::EX(*window));
    pipe.set_options(key, 0, window).ignore().incr(key, 1);
  }
  Ok(pipe.query_async(redis).await?)
}

#[expect(clippy::expect_used, reason = "a server whose system gives no random bytes cannot run")]
fn token() -> Token {
  let mut bytes = [0; 32];
  getrandom::fill(&mut bytes).expect("the random source of the system gives 32 bytes");
  Token(bytes)
}

#[cfg(test)]
mod tests {
  use std::net::{IpAddr, Ipv4Addr};

  use mmo_protocol::{
    Angle, Character, CharacterId, FromLogin, Password, Refusal, State, ToLogin, Token, ZoneId,
  };
  use mmo_store::{Saved, Store, keys};
  use redis::AsyncCommands;
  use sqlx::PgPool;

  use super::{Client, Error, Login, World};
  use crate::game::{Game, HOME, MEADOW, START, login, place, secret};

  /// The address of a client that guesses the passwords of other players.
  const STRANGER: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

  fn new_account(name: &str) -> ToLogin {
    ToLogin::NewAccount { name: name.to_owned(), password: secret() }
  }

  fn new_character(name: &str) -> ToLogin {
    ToLogin::NewCharacter { name: name.to_owned() }
  }

  #[sqlx::test(migrations = false)]
  async fn a_new_account_enters_the_queue_and_an_address_makes_5_each_hour(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    game.account("ada").await;
    let copy = game.copy(1);
    let mut client = Client::default();
    let made = copy.answer(&mut client, HOME, new_account("Bob")).await?;
    let FromLogin::Queued { position: 0, length: 1, wait: u32::MAX, resume } = made else {
      panic!("the first place of the queue, and not {made:?}");
    };
    let bob = game.store.sign_in("bob", secret()).await?.expect("the account of bob");
    assert_eq!(client, Client::Queued { account: bob, resume });
    let long = "c".repeat(17);
    let mut answers = Vec::new();
    for name in ["ADA", "cy", &long, "cy-d", "dée", "dee", "eve", "fay", "gil"] {
      let answer = copy.answer(&mut Client::default(), HOME, new_account(name)).await?;
      answers.push(place(&answer).ok_or(answer));
    }
    let other = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    let answer = copy.answer(&mut Client::default(), other, new_account("gil")).await?;
    answers.push(place(&answer).ok_or(answer));
    let none = u32::MAX;
    let name = Err(FromLogin::Refused(Refusal::Name));
    assert_eq!(
      answers,
      [
        Err(FromLogin::Refused(Refusal::Taken)),
        name.clone(),
        name.clone(),
        name.clone(),
        name,
        Ok((1, 2, none)),
        Ok((2, 3, none)),
        Ok((3, 4, none)),
        Err(FromLogin::Refused(Refusal::Throttled)),
        Ok((4, 5, none)),
      ]
    );
    let window: i64 = game.redis.clone().ttl(keys::created(HOME)).await?;
    assert!((3590..=3600).contains(&window), "the window ends in {window} s");
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_new_account_needs_a_password_of_8_characters_and_128_bytes(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let copy = game.copy(1);
    let (wide, wider) = ("é".repeat(64), "é".repeat(65));
    let (long, longer) = ("a".repeat(128), "a".repeat(129));
    let weak = Some(Refusal::Weak);
    let tries = [
      ("", weak),
      ("1234567", weak),
      ("ééééééé", weak),
      (longer.as_str(), weak),
      (wider.as_str(), weak),
      ("12345678", None),
      (wide.as_str(), None),
      (long.as_str(), None),
    ];
    let mut answers = Vec::new();
    for (number, (password, _)) in tries.iter().enumerate() {
      let message = ToLogin::NewAccount {
        name: format!("user{number}"),
        password: Password((*password).to_owned()),
      };
      let answer = copy.answer(&mut Client::default(), HOME, message).await?;
      answers.push(if let FromLogin::Refused(why) = answer { Some(why) } else { None });
    }
    assert_eq!(answers, tries.map(|(_, refused)| refused));
    let made = [
      game.store.sign_in("user0", Password(String::new())).await?,
      game.store.sign_in("user7", Password(long)).await?,
    ];
    assert_eq!(made.map(|account| account.is_some()), [false, true]);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn an_account_holds_8_characters_at_most(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let copy = game.copy(1);
    let mut client = Client::default();
    copy.answer(&mut client, HOME, login("ada")).await?;
    copy.admit().await?;
    copy.ready(&mut client).await?;
    let names = ["Alpha", "Bravo", "Cael", "Dara", "Elan", "Finn", "Gale", "Hana", "Iris"];
    let mut answers = Vec::new();
    for name in names {
      let answer = copy.answer(&mut client, HOME, new_character(name)).await?;
      answers.push(match answer {
        FromLogin::Characters(listed) => Ok(listed.len()),
        other => Err(other),
      });
    }
    let mut expected: Vec<_> = (1..=8).map(Ok).collect();
    expected.push(Err(FromLogin::Refused(Refusal::Limit)));
    assert_eq!(answers, expected);
    assert_eq!(game.store.characters(ada).await?.len(), 8);
    assert!(matches!(&client, Client::Admitted { characters, .. } if characters.len() == 8));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn two_admitted_clients_of_one_account_make_a_ninth_character_never(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let copy = game.copy(1);
    let (mut first, mut second) = (Client::default(), Client::default());
    copy.answer(&mut first, HOME, login("ada")).await?;
    copy.answer(&mut second, HOME, login("ada")).await?;
    copy.admit().await?;
    let (one, two) = (copy.ready(&mut first).await?, copy.ready(&mut second).await?);
    let empty = Some(FromLogin::Characters(Vec::new()));
    assert_eq!([one, two], [empty.clone(), empty]);
    for name in ["Alpha", "Bravo", "Cael", "Dara", "Elan", "Finn", "Gale"] {
      copy.answer(&mut first, HOME, new_character(name)).await?;
    }
    assert!(matches!(&second, Client::Admitted { characters, .. } if characters.is_empty()));
    let (hana, iris) = tokio::join!(
      copy.answer(&mut first, HOME, new_character("Hana")),
      copy.answer(&mut second, HOME, new_character("Iris")),
    );
    let answers = [hana?, iris?];
    let refused = answers.iter().filter(|answer| **answer == FromLogin::Refused(Refusal::Limit));
    assert_eq!(refused.count(), 1);
    assert_eq!(game.store.characters(ada).await?.len(), 8);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn an_admitted_account_makes_its_characters_at_the_start_of_the_world(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let bob = game.account("bob").await;
    game.store.create_character(bob, "Dara", ZoneId(7), START).await?;
    let copy = game.copy(1);
    let mut client = Client::default();
    copy.answer(&mut client, HOME, login("ada")).await?;
    let queued = copy.answer(&mut client.clone(), HOME, new_character("Brom")).await?;
    copy.admit().await?;
    copy.ready(&mut client).await?;
    let answers = [
      queued,
      copy.answer(&mut client, HOME, new_character("bROM")).await?,
      copy.answer(&mut client, HOME, new_character("DARA")).await?,
      copy.answer(&mut client, HOME, new_character("Br")).await?,
      copy.answer(&mut client, HOME, new_character("Br0m")).await?,
    ];
    let listed = game.store.characters(ada).await?;
    let brom = listed.first().map(|one| one.id).expect("a character of ada");
    assert_eq!(
      answers,
      [
        FromLogin::Refused(Refusal::Expired),
        FromLogin::Characters(vec![Character { id: brom, name: "Brom".to_owned() }]),
        FromLogin::Refused(Refusal::Taken),
        FromLogin::Refused(Refusal::Name),
        FromLogin::Refused(Refusal::Name),
      ]
    );
    assert_eq!(client, Client::Admitted { account: ada, characters: vec![brom] });
    let state = State { position: MEADOW.position, yaw: Angle(0), hit_points: 100 };
    assert_eq!(
      game.store.load(&[brom]).await?,
      [Saved { character: brom, zone: MEADOW.zone, state, version: 0 }]
    );
    let () = game.redis.clone().del(keys::admit(ada)).await?;
    let ended = copy.answer(&mut client, HOME, new_character("Cael")).await?;
    assert_eq!(ended, FromLogin::Refused(Refusal::Expired));
    assert_eq!((client, game.store.characters(ada).await?.len()), (Client::Out, 1));
    Ok(())
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
    let last = game.copy(3).answer(&mut clients[3].0, HOME, ToLogin::Wait).await?;
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
      let answer =
        game.copy(0).answer(&mut client, HOME, ToLogin::Resume { token: *token }).await?;
      if let FromLogin::Queued { position, length, resume, .. } = answer {
        places.push((position, length, resume));
      }
    }
    let expected: Vec<_> = (0..3).rev().map(|at| (at, 3, tokens[at as usize])).collect();
    assert_eq!(places, expected);
    let mut stranger = Client::default();
    let unknown = ToLogin::Resume { token: Token([0; 32]) };
    let unknown = game.copy(0).answer(&mut stranger, HOME, unknown).await?;
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

  fn wrong(name: &str) -> ToLogin {
    ToLogin::Login { name: name.to_owned(), password: Password("guess".to_owned()) }
  }

  #[sqlx::test(migrations = false)]
  async fn a_name_refused_to_one_address_signs_in_from_another(pool: PgPool) -> Result<(), Error> {
    let game = Game::new(pool).await;
    game.account("ada").await;
    let copy = game.copy(1);
    let mut client = Client::default();
    let mut answers = Vec::new();
    for _ in 0..5 {
      answers.push(copy.answer(&mut client, STRANGER, wrong("ADA")).await?);
    }
    answers.push(copy.answer(&mut client, STRANGER, login("ada")).await?);
    let mut expected = vec![FromLogin::Refused(Refusal::Password); 5];
    expected.push(FromLogin::Refused(Refusal::Throttled));
    assert_eq!(answers, expected);
    assert_eq!(client, Client::Out);
    let mut redis = game.redis.clone();
    let windows: (i64, i64) =
      (redis.ttl(keys::failed(STRANGER, "ada")).await?, redis.ttl(keys::guessed(STRANGER)).await?);
    assert!((1..=60).contains(&windows.0), "the window of the pair ends in {} s", windows.0);
    assert!((590..=600).contains(&windows.1), "the window of the address ends in {} s", windows.1);
    let owner = copy.answer(&mut client, HOME, login("ada")).await?;
    assert_eq!(place(&owner), Some((0, 1, u32::MAX)));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn an_address_with_30_failed_logins_over_all_names_waits_for_its_window(
    pool: PgPool,
  ) -> Result<(), Error> {
    let game = Game::new(pool).await;
    game.account("ada").await;
    let copy = game.copy(1);
    let mut answers = Vec::new();
    for guess in 0..30 {
      answers.push(copy.answer(&mut Client::Out, STRANGER, wrong(&format!("n{guess}"))).await?);
    }
    assert_eq!(answers, vec![FromLogin::Refused(Refusal::Password); 30]);
    let refused = copy.answer(&mut Client::Out, STRANGER, login("ada")).await?;
    assert_eq!(refused, FromLogin::Refused(Refusal::Throttled));
    let owner = copy.answer(&mut Client::Out, HOME, login("ada")).await?;
    assert_eq!(place(&owner), Some((0, 1, u32::MAX)));
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
      copy.answer(&mut client, HOME, login("ada")).await?,
      copy.answer(&mut client, HOME, ToLogin::Wait).await?,
      copy.answer(&mut client, HOME, ToLogin::Select { character: CharacterId(1) }).await?,
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
    assert_eq!(copy.answer(&mut clients[1].0, HOME, ToLogin::Wait).await?, characters);
    let mut redis = game.redis.clone();
    let () = redis.hset(keys::presence(ada), "zone", 7).await?;
    let first = &mut clients[0].0;
    let online = copy.answer(first, HOME, ToLogin::Select { character: brom }).await?;
    let () = redis.del(keys::presence(ada)).await?;
    let foreign = copy.answer(first, HOME, ToLogin::Select { character: dara }).await?;
    let made = copy.answer(first, HOME, ToLogin::Select { character: brom }).await?;
    let second = copy.answer(&mut clients[1].0, HOME, ToLogin::Select { character: brom }).await?;
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
    copy.answer(&mut queued, HOME, login("ada")).await?;
    copy.admit().await?;
    let mut admitted = queued.clone();
    copy.ready(&mut admitted).await?;
    let closed = PgPool::connect_with(pool.connect_options().as_ref().clone()).await;
    let closed = closed.map_err(mmo_store::Error::from)?;
    let store = Store::open(closed.clone()).await?;
    let unreachable = Login::new(store, game.redis.clone(), World { id: 1, cap: 1 }, MEADOW);
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
      no_get.answer(&mut Client::Out, HOME, login("ada")).await.err(),
      unreachable.answer(&mut Client::Out, HOME, login("ada")).await.err(),
      no_set.answer(&mut Client::Out, HOME, wrong).await.err(),
      no_script.answer(&mut Client::Out, HOME, login("ada")).await.err(),
      no_script.admit().await.err(),
      no_get.answer(&mut Client::Out, HOME, resume).await.err(),
      unreachable.ready(&mut queued.clone()).await.err(),
      no_exists.ready(&mut queued.clone()).await.err(),
      no_exists.answer(&mut admitted.clone(), HOME, select.clone()).await.err(),
      unreachable.answer(&mut admitted.clone(), HOME, select.clone()).await.err(),
      no_exists.answer(&mut admitted.clone(), HOME, new_character("Cael")).await.err(),
      unreachable.answer(&mut admitted.clone(), HOME, new_character("Cael")).await.err(),
      no_del.answer(&mut admitted.clone(), HOME, select.clone()).await.err(),
      no_hmset.answer(&mut admitted.clone(), HOME, select).await.err(),
      no_set.answer(&mut Client::Out, HOME, new_account("eve")).await.err(),
      unreachable.answer(&mut Client::Out, HOME, new_account("eve")).await.err(),
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
        denied("get", "get").map(|text| {
          format!("Pipeline failures: [(Index 0, error: {text}), (Index 1, error: {text})]")
        }),
        closed.clone(),
        denied("set", "set").map(|text| {
          format!("Transaction aborted: [(Index 0, error: {text}), (Index 2, error: {text})]")
        }),
        denied("@scripting", "evalsha"),
        denied("@scripting", "evalsha"),
        denied("get", "get"),
        closed.clone(),
        denied("exists", "exists"),
        denied("exists", "exists"),
        closed.clone(),
        denied("exists", "exists"),
        closed.clone(),
        denied("del", "del"),
        aborted("hmset", "hmset"),
        aborted("set", "set"),
        closed,
      ]
    );
    Ok(())
  }
}
