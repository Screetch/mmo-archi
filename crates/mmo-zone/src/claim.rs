//! The claim of a layer: the layer that an idle writer takes from the directory of its shard, and
//! the entries that its writer keeps in Redis: the lease of the layer with its count of players,
//! and the presence of each of its players.

use std::collections::BTreeMap;
use std::time::Duration;

use mmo_protocol::{AccountId, Batch, CharacterId, Epoch, Layer, Save, ShardId};
use mmo_store::{Directory, Lease, keys};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::time;

/// The time between two claims of an idle writer.
const IDLE: Duration = Duration::from_secs(1);

/// The life of the presence of a player, which the writer renews each 10 s.
const PRESENCE_LIFE_S: i64 = 30;

/// The life of the ticks of a layer, which each write renews: 10 minutes.
const TICKS_LIFE_S: i64 = 10 * 60;

/// Why the claim of a layer fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error(
    "layer {}:{} is held by a writer of epoch {held}, which is newer than the epoch {}",
    layer.zone.0,
    layer.number,
    own.0
  )]
  Superseded { layer: Layer, held: i64, own: Epoch },
  #[error("shard {} holds no cap of a layer, which the login service writes", .0.0)]
  NoCap(ShardId),
  #[error("Redis refuses the layer writer: {0}")]
  Redis(#[from] redis::RedisError),
}

/// A write of the writer in Redis.
pub(crate) enum Entry {
  /// The lease of the layer, with the count of its players, and the accounts whose players the
  /// writer took since its last lease, which end their places.
  Layer { players: usize, taken: Vec<AccountId> },
  /// The count of the ticks of the layer in each bucket of [`keys::ticks`], to add to the counts
  /// that Redis holds.
  Ticks(BTreeMap<u32, u64>),
  /// The presence of these players, which the writer holds now.
  Present(Vec<(AccountId, CharacterId)>),
  /// The end of the presence of these accounts.
  Absent(Vec<AccountId>),
}

/// The layer that a writer holds, in its shard, with the epoch that the keeper gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Held {
  pub(crate) shard: ShardId,
  pub(crate) layer: Layer,
  pub(crate) epoch: Epoch,
}

/// A layer that an idle writer took from the directory, with the cap of its shard, before the
/// keeper gives it its epoch.
pub(crate) struct Taken {
  redis: ConnectionManager,
  address: String,
  pub(crate) shard: ShardId,
  pub(crate) layer: Layer,
  pub(crate) cap: usize,
}

impl Taken {
  /// Runs the claim of the directory of `shard` each second, until it gives a layer to the writer
  /// at `address`, which the gateways dial, and reads the cap of a layer of the shard.
  pub(crate) async fn wait(url: &str, shard: ShardId, address: String) -> Result<Self, Error> {
    let mut redis = ConnectionManager::new(redis::Client::open(url)?).await?;
    let directory = Directory { shard };
    let layer = loop {
      if let Some(layer) = directory.claim(&mut redis, &address).await? {
        break layer;
      }
      time::sleep(IDLE).await;
    };
    let cap: Option<usize> = redis.hget(keys::shard(shard), keys::LAYER_CAP).await?;
    let cap = cap.ok_or(Error::NoCap(shard))?;
    Ok(Self { redis, address, shard, layer, cap })
  }

  /// The claim of the layer with `epoch`, which the keeper gave. The layer opens at the first
  /// lease of the writer with that epoch.
  pub(crate) fn hold(self, epoch: Epoch) -> Claim {
    let held = Held { shard: self.shard, layer: self.layer, epoch };
    Claim { redis: self.redis, held, address: self.address }
  }
}

/// The layer of a writer, and its connection to Redis.
#[derive(Clone)]
pub(crate) struct Claim {
  redis: ConnectionManager,
  held: Held,
  address: String,
}

impl Claim {
  /// The batch of `saves` for the keeper, with the layer and the epoch of this writer.
  pub(crate) fn batch(&self, saves: Vec<Save>) -> Batch {
    Batch { shard: self.held.shard, layer: self.held.layer, epoch: self.held.epoch, saves }
  }

  pub(crate) async fn write(&self, entry: Entry) -> Result<(), Error> {
    let mut redis = self.redis.clone();
    let Held { shard, layer, epoch } = self.held;
    match entry {
      Entry::Layer { players, taken } => {
        let players = u32::try_from(players).unwrap_or(u32::MAX);
        let lease = Lease { layer, epoch, address: &self.address, players, taken: &taken };
        match (Directory { shard }).lease(&mut redis, &lease).await? {
          None => Ok(()),
          Some(held) => Err(Error::Superseded { layer, held: held.0, own: epoch }),
        }
      }
      Entry::Ticks(counts) if counts.is_empty() => Ok(()),
      Entry::Present(players) if players.is_empty() => Ok(()),
      Entry::Absent(accounts) if accounts.is_empty() => Ok(()),
      Entry::Ticks(counts) => {
        let key = keys::ticks(shard, layer);
        let mut batch = redis::pipe();
        for (bucket, count) in counts {
          batch.hincr(&key, bucket, count).ignore();
        }
        batch.expire(&key, TICKS_LIFE_S).ignore();
        Ok(batch.exec_async(&mut redis).await?)
      }
      Entry::Present(players) => {
        let mut batch = redis::pipe();
        for (account, character) in players {
          let key = keys::presence(account);
          let fields = [
            (keys::CHARACTER, character.0),
            (keys::SHARD, i64::from(shard.0)),
            (keys::ZONE, i64::from(layer.zone.0)),
            (keys::LAYER, i64::from(layer.number)),
            (keys::EPOCH, epoch.0),
          ];
          batch.hset_multiple(&key, &fields).ignore().expire(&key, PRESENCE_LIFE_S).ignore();
        }
        Ok(batch.exec_async(&mut redis).await?)
      }
      Entry::Absent(accounts) => {
        let keys: Vec<String> = accounts.into_iter().map(keys::presence).collect();
        Ok(redis.del::<_, ()>(keys).await?)
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use futures_util::StreamExt;
  use mmo_protocol::{AccountId, Batch, CharacterId, Epoch, Layer, ShardId, ZoneId};
  use mmo_store::testing::Redis;
  use mmo_store::{Directory, keys};
  use redis::AsyncCommands;
  use sqlx::PgPool;

  use super::{Claim, Entry, Error, Taken};
  use crate::peer::{hash, life};

  const SHARD: ShardId = ShardId(1);
  const MEADOW: Layer = Layer { zone: ZoneId(1), number: 1 };

  /// Gives the shard a cap of a layer and the layer 1 of zone 1 with no writer, as the login
  /// service and a placement do.
  async fn opened(redis: &Redis, cap: Option<u32>) {
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut admin = client.get_multiplexed_async_connection().await.expect("a connection");
    if let Some(cap) = cap {
      let () = admin.hset(keys::shard(SHARD), keys::LAYER_CAP, cap).await.expect("a cap");
    }
    let () = admin.hset(keys::layers(SHARD), keys::field(MEADOW), 0).await.expect("a layer");
  }

  async fn taken(url: &str) -> Result<Taken, Error> {
    Taken::wait(url, SHARD, "zone-1:4433".to_owned()).await
  }

  /// The claim of a writer of `epoch` on the layer, after its first lease.
  async fn claim(redis: &Redis, epoch: i64) -> Claim {
    opened(redis, Some(20)).await;
    let claim = taken(&redis.url()).await.expect("a layer").hold(Epoch(epoch));
    claim.write(Entry::Layer { players: 0, taken: Vec::new() }).await.expect("a lease");
    claim
  }

  fn fields<const N: usize>(fields: [(&str, &str); N]) -> BTreeMap<String, String> {
    fields.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
  }

  #[sqlx::test(migrations = false)]
  async fn an_idle_writer_takes_the_layer_that_a_placement_opens_and_leases_it_with_its_epoch(
    _: PgPool,
  ) {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut admin = client.get_multiplexed_async_connection().await.expect("a connection");
    let () = admin.hset(keys::shard(SHARD), keys::LAYER_CAP, 20).await.expect("a cap");
    let mut channel = client.get_async_pubsub().await.expect("a connection");
    channel.subscribe(keys::DIRECTORY).await.expect("a channel");
    let url = redis.url();
    let idle = tokio::spawn(async move { taken(&url).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let placed = Directory { shard: SHARD }.place(&mut admin, AccountId(5), ZoneId(1), None).await;
    assert_eq!(placed.expect("a placement"), mmo_store::Placed::Wait);
    let taken = idle.await.expect("a writer").expect("a layer");
    assert_eq!((taken.shard, taken.layer, taken.cap), (SHARD, MEADOW, 20));
    let claim = taken.hold(Epoch(1));
    claim.write(Entry::Layer { players: 0, taken: Vec::new() }).await.expect("a lease");
    let mut messages = channel.on_message();
    let mut named = Vec::new();
    for _ in 0..2 {
      named
        .push(messages.next().await.expect("a message").get_payload::<String>().expect("a layer"));
    }
    assert_eq!(named, ["1:1:1", "1:1:1"]);
    let lease = fields([("address", "zone-1:4433"), ("epoch", "1")]);
    assert_eq!(hash(&redis, "writer:1:1:1").await, lease);
    let () =
      admin.zadd_multiple("placed:1:1:1", &[(u64::MAX, 5), (u64::MAX, 9)]).await.expect("places");
    let taken = vec![AccountId(5)];
    claim.write(Entry::Layer { players: 3, taken }).await.expect("the lease again");
    assert_eq!(hash(&redis, "layers:1").await, fields([("1:1", "3")]));
    let places: Vec<i64> = admin.zrange("placed:1:1:1", 0, -1).await.expect("the places");
    assert_eq!(places, [9], "the writer took the player of the place of 5");
    assert!((1..=3000).contains(&life(&redis, "writer:1:1:1").await));
    let batch = Batch { shard: SHARD, layer: MEADOW, epoch: Epoch(1), saves: Vec::new() };
    assert_eq!(claim.batch(Vec::new()), batch);
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_stops_when_the_lease_holds_a_newer_epoch_or_its_shard_holds_no_cap(_: PgPool) {
    let redis = Redis::start().await;
    let newer = claim(&redis, 9).await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut admin = client.get_multiplexed_async_connection().await.expect("a connection");
    let () = admin.del(keys::writer(SHARD, MEADOW)).await.expect("an end of the lease");
    let older = taken(&redis.url()).await.expect("a layer");
    let () = admin
      .hset(keys::writer(SHARD, MEADOW), keys::EPOCH, 9)
      .await
      .expect("the lease of the newer writer");
    let older = older.hold(Epoch(4)).write(Entry::Layer { players: 0, taken: Vec::new() }).await;
    let older = older.err().map(|error| error.to_string());
    assert_eq!(
      older.as_deref(),
      Some("layer 1:1 is held by a writer of epoch 9, which is newer than the epoch 4")
    );
    newer
      .write(Entry::Layer { players: 1, taken: Vec::new() })
      .await
      .expect("the newest epoch writes");
    let other = Redis::start().await;
    opened(&other, None).await;
    let no_cap = taken(&other.url()).await.err().map(|error| error.to_string());
    let held = "shard 1 holds no cap of a layer, which the login service writes";
    assert_eq!(no_cap.as_deref(), Some(held));
    let bad = taken("nope").await.err();
    assert!(matches!(bad, Some(Error::Redis(_))), "{bad:?}");
  }

  #[sqlx::test(migrations = false)]
  async fn the_presence_of_a_player_names_its_layer_lives_30_s_and_ends_with_its_account(
    _: PgPool,
  ) {
    let redis = Redis::start().await;
    let claim = claim(&redis, 2).await;
    let players = vec![(AccountId(5), CharacterId(7)), (AccountId(6), CharacterId(8))];
    claim.write(Entry::Present(players)).await.expect("two players");
    claim.write(Entry::Present(Vec::new())).await.expect("no player");
    claim.write(Entry::Absent(Vec::new())).await.expect("no account");
    let present = |character| {
      fields([
        ("character", character),
        ("shard", "1"),
        ("zone", "1"),
        ("layer", "1"),
        ("epoch", "2"),
      ])
    };
    assert_eq!(hash(&redis, "presence:5").await, present("7"));
    assert_eq!(hash(&redis, "presence:6").await, present("8"));
    assert!(((29_000..=30_000).contains(&life(&redis, "presence:5").await)));
    claim.write(Entry::Absent(vec![AccountId(5), AccountId(99)])).await.expect("an end");
    assert_eq!(hash(&redis, "presence:5").await, fields([]));
    assert_eq!(hash(&redis, "presence:6").await, present("8"));
  }

  /// A Redis user that may run each command but those that `rules` take away.
  async fn user(redis: &Redis, name: &str, rules: &[&str]) -> String {
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut admin = client.get_multiplexed_async_connection().await.expect("a connection");
    redis::cmd("ACL")
      .arg("SETUSER")
      .arg(name)
      .arg(&["on", "nopass", "~*", "&*", "+@all"])
      .arg(rules)
      .query_async::<()>(&mut admin)
      .await
      .expect("a user");
    format!("redis://{name}:any@127.0.0.1:{}", redis.port())
  }

  #[sqlx::test(migrations = false)]
  async fn the_ticks_of_a_writer_add_up_in_one_hash_of_its_layer_that_lives_10_minutes(_: PgPool) {
    let redis = Redis::start().await;
    let claim = claim(&redis, 1).await;
    assert_eq!(hash(&redis, "ticks:1:1:1").await, fields([]));
    claim.write(Entry::Ticks(BTreeMap::new())).await.expect("no tick");
    assert_eq!(life(&redis, "ticks:1:1:1").await, -2);
    claim.write(Entry::Ticks(BTreeMap::from([(3, 18), (7, 2)]))).await.expect("20 ticks");
    claim.write(Entry::Ticks(BTreeMap::from([(3, 20)]))).await.expect("20 ticks more");
    assert_eq!(hash(&redis, "ticks:1:1:1").await, fields([("3", "38"), ("7", "2")]));
    assert!((599_000..=600_000).contains(&life(&redis, "ticks:1:1:1").await));
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_names_each_command_that_redis_refuses(_: PgPool) {
    let redis = Redis::start().await;
    opened(&redis, Some(20)).await;
    let stranger = format!("redis://nobody:any@127.0.0.1:{}", redis.port());
    let mut faults = vec![taken(&stranger).await.err()];
    for (name, rules) in [("noscript", "-@scripting"), ("nohget", "-hget")] {
      faults.push(taken(&user(&redis, name, &[rules]).await).await.err());
    }
    assert!(faults.iter().all(|fault| matches!(fault, Some(Error::Redis(_)))), "{faults:?}");
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut admin = client.get_multiplexed_async_connection().await.expect("a connection");
    let () = admin.del(keys::writer(SHARD, MEADOW)).await.expect("an end of the lease");

    let present = vec![(AccountId(5), CharacterId(7))];
    let url = user(&redis, "noexpire", &["-expire"]).await;
    let claim = taken(&url).await.expect("a layer").hold(Epoch(1));
    let refused = claim.write(Entry::Present(present)).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
    let ticks = BTreeMap::from([(3, 1)]);
    let refused = claim.write(Entry::Ticks(ticks)).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
    let () = admin.del(keys::writer(SHARD, MEADOW)).await.expect("an end of the lease");
    let url = user(&redis, "nodel", &["-del"]).await;
    let claim = taken(&url).await.expect("a layer").hold(Epoch(2));
    let refused = claim.write(Entry::Absent(vec![AccountId(5)])).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
    user(&redis, "nodel", &["-@scripting"]).await;
    let refused = claim.write(Entry::Layer { players: 0, taken: Vec::new() }).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
  }
}
