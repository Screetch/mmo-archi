//! The fixture of the tests of the crate: a Redis of each test alone, and its database.

use std::net::{IpAddr, Ipv4Addr};

use mmo_protocol::{
  AccountId, Angle, CharacterId, Epoch, FromLogin, Password, Position, ShardId, State, ToLogin,
  ZoneId,
};
use mmo_store::testing::Redis;
use mmo_store::{Directory, Lease, NewCharacter, Store, keys};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use sqlx::PgPool;

use crate::{Client, Login, Start, World};

pub(crate) const START: State =
  State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };

/// The start of the new characters of world 1, in the zone that [`Game::new`] adds.
pub(crate) const MEADOW: Start =
  Start { zone: ZoneId(7), position: Position { x: 1200, y: -300, z: 50 } };

/// The address of the clients of the tests.
pub(crate) const HOME: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A Redis of one test alone, which ends with the test, and a PostgreSQL database of the test.
/// `REDIS_SERVER` names the program of Redis, or else `redis-server` of the `PATH` runs.
pub(crate) struct Game {
  server: Redis,
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
    let server = Redis::start().await;
    let redis = Self::connect(server.port(), "default").await;
    Self { server, redis, store, database }
  }

  async fn connect(port: u16, user: &str) -> ConnectionManager {
    let url = format!("redis://{user}:any@127.0.0.1:{port}");
    let client = redis::Client::open(url).expect("a URL of Redis");
    ConnectionManager::new(client).await.expect("Redis accepts the user")
  }

  /// A copy of the login service of world 1 with a cap of `cap` accounts.
  pub(crate) fn copy(&self, cap: u32) -> Login {
    self.world(1, cap)
  }

  /// A copy of the login service of the world `id`, whose shard is `id`, with a cap of `cap`
  /// accounts.
  pub(crate) fn world(&self, id: u32, cap: u32) -> Login {
    Login::new(self.store.clone(), self.redis.clone(), World { id, cap, layer_cap: 20 }, MEADOW)
  }

  /// A copy whose user of Redis has each command but those that the rule of ACL `denied` takes.
  pub(crate) async fn denied(&self, denied: &str) -> Login {
    let user = format!("denied{denied}");
    self.deny(&user, denied).await;
    let redis = Self::connect(self.server.port(), &user).await;
    Login::new(self.store.clone(), redis, World { id: 1, cap: 1, layer_cap: 20 }, MEADOW)
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
    self.server.url()
  }

  /// Opens a layer of `zone` in `shard` with a writer, as a placement and a layer writer do, so a
  /// selection in that zone gets it.
  pub(crate) async fn layer(&self, shard: ShardId, zone: ZoneId) {
    let mut redis = self.redis.clone();
    let directory = Directory { shard };
    let () = redis.hset(keys::shard(shard), keys::LAYER_CAP, 20).await.expect("a cap");
    directory.place(&mut redis, AccountId(0), zone, None).await.expect("a placement");
    let layer = directory.claim(&mut redis, "w").await.expect("a claim").expect("a layer");
    let lease = Lease { layer, epoch: Epoch(1), address: "w", players: 0, taken: &[] };
    directory.lease(&mut redis, &lease).await.expect("a lease");
  }

  pub(crate) async fn account(&self, name: &str) -> AccountId {
    self.store.create_account(name, secret()).await.expect("a free name")
  }

  /// Makes the character `name` of `account` in `shard`, at `START` in the meadow.
  pub(crate) async fn character(
    &self,
    account: AccountId,
    shard: ShardId,
    name: &str,
  ) -> CharacterId {
    let new = NewCharacter { account, shard, name, zone: MEADOW.zone, state: START };
    self.store.create_character(&new).await.expect("a free name")
  }

  pub(crate) async fn enter(&self, copy: &Login, names: &[&str]) -> Vec<(Client, FromLogin)> {
    let mut entered = Vec::new();
    for name in names {
      let mut client = Client::default();
      let answer = copy.answer(&mut client, HOME, login(name)).await.expect("an answer");
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
      answers.push(copy.answer(client, HOME, ToLogin::Wait).await.expect("an answer"));
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
