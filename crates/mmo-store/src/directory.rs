//! The directory of the layers of one shard in Redis, and the scripts that change it.
//!
//! Section 6 of `docs/architecture.md` gives its rules. Redis runs each script as one atomic step,
//! so two callers never take one decision twice: two writers never claim one layer, and two
//! placements never put a party on two layers of one zone. A writer is the one authority of the
//! count of its layer, and it refuses a player when a placement raced a change of that count.

use std::sync::LazyLock;
use std::time::Duration;

use mmo_protocol::{AccountId, Epoch, Layer, ShardId, ZoneId};
use redis::aio::ConnectionLike;
use redis::{RedisError, Script};

use crate::keys;

/// The life of the lease of a writer in milliseconds, which [`LEASE`] gives.
const LEASE_MS: u64 = 3000;

/// The life of the lease of a writer on its layer, which it renews each second with
/// [`Directory::lease`]. A layer whose lease ended has no writer, and an idle writer claims it.
pub const LEASE: Duration = Duration::from_millis(LEASE_MS);

/// The most members of a party.
pub const PARTY: u32 = 5;

/// The life of a place that [`Directory::place`] gives in milliseconds, the time that a client
/// has to reach a gateway with its session.
const PLACE_MS: u64 = 60_000;

static PLACE: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/place.lua")));
static CLAIM: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/claim.lua")));
static RENEW: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/lease.lua")));
static DRAIN: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/drain.lua")));
static CLOSE: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/close.lua")));
static GROUP: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("directory/party.lua")));

/// The id of a party in its shard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartyId(pub u64);

/// What the writer of a layer writes with [`Directory::lease`] each second and at each change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease<'a> {
  /// The layer of the writer.
  pub layer: Layer,
  /// The epoch that the keeper gave the writer.
  pub epoch: Epoch,
  /// The address that the gateways dial.
  pub address: &'a str,
  /// The count of the players of the layer.
  pub players: u32,
  /// The accounts whose players the writer took since its last lease. Their places end.
  pub taken: &'a [AccountId],
}

/// The answer of [`Directory::place`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
  /// The player enters this layer.
  On(Layer),
  /// No open layer of the zone takes the player now: the caller keeps the player where it is,
  /// and asks again after 1 s.
  Wait,
}

/// The directory of the layers of one shard: the layers of each zone with their counts of
/// players, the layers that drain, and the lease of the writer of each layer.
///
/// A layer is open when it stands in the directory, a writer holds its lease with an epoch that
/// the keeper gave, and it does not drain. The cap of each layer is the
/// [`LAYER_CAP`](keys::LAYER_CAP) of the shard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Directory {
  /// The shard of the directory.
  pub shard: ShardId,
}

impl Directory {
  /// The layer of `zone` that `account` enters, as the placement of section 6 gives it: the
  /// layer of a member of its party in the zone, else the layer with the number of `leaving` when
  /// the player crosses from another zone, else the open layer with the most players that has
  /// room for the members of its party, and of these the lowest number.
  ///
  /// The layer holds the place of the player for 60 s, or until its writer takes the player and
  /// names it in [`Lease::taken`], so placements that come faster than the writer writes its count
  /// never pass the cap. When no layer has room, the player waits, and the directory opens the
  /// least free number of the zone for a writer to claim, unless a layer of the zone waits for a
  /// writer already.
  /// `leaving` is the layer that the player leaves: in another zone when it crosses, and in its
  /// zone when it moves out of a layer that drains.
  pub async fn place(
    self,
    redis: &mut impl ConnectionLike,
    account: AccountId,
    zone: ZoneId,
    leaving: Option<Layer>,
  ) -> Result<Placed, RedisError> {
    let number: u32 = PLACE
      .key(keys::layers(self.shard))
      .key(keys::draining(self.shard))
      .key(keys::shard(self.shard))
      .key(keys::presence(account))
      .arg(zone.0)
      .arg(account.0)
      .arg(self.shard.0)
      .arg(keys::writers(self.shard))
      .arg(keys::PRESENCE)
      .arg(keys::parties(self.shard))
      .arg(keys::DIRECTORY)
      .arg(keys::places(self.shard))
      .arg(PLACE_MS)
      .arg(leaving.map(|layer| (layer.zone.0, layer.number)))
      .invoke_async(redis)
      .await?;
    Ok(match number {
      0 => Placed::Wait,
      number => Placed::On(Layer { zone, number }),
    })
  }

  /// The layer with no writer that an idle writer at `address` takes: of the lowest zone, then of
  /// the lowest number. It writes the lease of the layer with the epoch 0, so no other writer
  /// takes it, and the writer then renews it with the epoch that the keeper gives.
  pub async fn claim(
    self,
    redis: &mut impl ConnectionLike,
    address: &str,
  ) -> Result<Option<Layer>, RedisError> {
    let found: Option<(i32, u32)> = CLAIM
      .key(keys::layers(self.shard))
      .arg(keys::writers(self.shard))
      .arg(address)
      .arg(LEASE_MS)
      .arg(keys::DIRECTORY)
      .arg(self.shard.0)
      .invoke_async(redis)
      .await?;
    Ok(found.map(|(zone, number)| Layer { zone: ZoneId(zone), number }))
  }

  /// Renews the lease of the writer of a layer and writes its count of players, as `lease` says.
  /// It gives the epoch of the lease and writes nothing when a writer of a newer epoch holds it.
  pub async fn lease(
    self,
    redis: &mut impl ConnectionLike,
    lease: &Lease<'_>,
  ) -> Result<Option<Epoch>, RedisError> {
    let held: i64 = RENEW
      .key(keys::writer(self.shard, lease.layer))
      .key(keys::layers(self.shard))
      .key(keys::placed(self.shard, lease.layer))
      .arg(LEASE_MS)
      .arg(lease.epoch.0)
      .arg(lease.address)
      .arg(keys::field(lease.layer))
      .arg(lease.players)
      .arg(lease.taken.iter().map(|account| account.0).collect::<Vec<i64>>())
      .invoke_async(redis)
      .await?;
    Ok((held != 0).then_some(Epoch(held)))
  }

  /// Puts `layer` in drain when the other open layers of its zone have room for all its players
  /// together, and gives whether it drains now. A layer that drains takes no new player.
  pub async fn drain(
    self,
    redis: &mut impl ConnectionLike,
    layer: Layer,
  ) -> Result<bool, RedisError> {
    DRAIN
      .key(keys::layers(self.shard))
      .key(keys::draining(self.shard))
      .key(keys::shard(self.shard))
      .arg(layer.zone.0)
      .arg(layer.number)
      .arg(keys::writers(self.shard))
      .arg(keys::DIRECTORY)
      .arg(self.shard.0)
      .invoke_async(redis)
      .await
  }

  /// Removes `layer` and its lease when it holds no player and no place, and the lease holds
  /// `epoch`, the epoch of its writer, and gives whether it did. The number of the layer is then
  /// free again.
  pub async fn close(
    self,
    redis: &mut impl ConnectionLike,
    layer: Layer,
    epoch: Epoch,
  ) -> Result<bool, RedisError> {
    CLOSE
      .key(keys::layers(self.shard))
      .key(keys::draining(self.shard))
      .key(keys::writer(self.shard, layer))
      .key(keys::placed(self.shard, layer))
      .arg(keys::field(layer))
      .arg(epoch.0)
      .arg(keys::DIRECTORY)
      .arg(keys::layer(self.shard, layer))
      .invoke_async(redis)
      .await
  }

  /// Adds the player of `account` to `party`, and gives whether it did. It refuses a player that
  /// does not play in the shard or has a party, a party of [`PARTY`] members, and a player on
  /// another layer of a zone where a member of the party plays, so a party never stands on two
  /// layers of one zone.
  pub async fn join(
    self,
    redis: &mut impl ConnectionLike,
    party: PartyId,
    account: AccountId,
  ) -> Result<bool, RedisError> {
    self.group(redis, account, Some(party)).await
  }

  /// Takes the player of `account` out of its party, and gives whether it had one. A party with
  /// one member left ends.
  pub async fn leave(
    self,
    redis: &mut impl ConnectionLike,
    account: AccountId,
  ) -> Result<bool, RedisError> {
    self.group(redis, account, None).await
  }

  async fn group(
    self,
    redis: &mut impl ConnectionLike,
    account: AccountId,
    party: Option<PartyId>,
  ) -> Result<bool, RedisError> {
    GROUP
      .key(keys::presence(account))
      .arg(keys::parties(self.shard))
      .arg(keys::PRESENCE)
      .arg(account.0)
      .arg(self.shard.0)
      .arg(PARTY)
      .arg(party.map(|party| party.0))
      .invoke_async(redis)
      .await
  }
}
#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use futures_util::StreamExt;
  use mmo_protocol::{AccountId, Epoch, Layer, ShardId, ZoneId};
  use redis::AsyncCommands;
  use redis::aio::MultiplexedConnection;

  use super::{Directory, Lease, PartyId, Placed};
  use crate::keys;
  use crate::testing::Redis;

  const SHARD: Directory = Directory { shard: ShardId(1) };

  const fn on(zone: i32, number: u32) -> Layer {
    Layer { zone: ZoneId(zone), number }
  }

  /// A Redis of the test with a shard whose layers hold `cap` players.
  async fn shard(cap: u32) -> (Redis, MultiplexedConnection) {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut db = client.get_multiplexed_async_connection().await.expect("a connection");
    let () = db.hset(keys::shard(SHARD.shard), keys::LAYER_CAP, cap).await.expect("a cap");
    (redis, db)
  }

  async fn place(db: &mut MultiplexedConnection, account: i64, zone: i32) -> Placed {
    SHARD.place(db, AccountId(account), ZoneId(zone), None).await.expect("a placement")
  }

  async fn cross(db: &mut MultiplexedConnection, account: i64, zone: i32, from: Layer) -> Placed {
    SHARD.place(db, AccountId(account), ZoneId(zone), Some(from)).await.expect("a placement")
  }

  /// Claims the next layer for a writer at `address`, and renews its lease with `epoch` and a
  /// count of 0, as a writer does once the keeper gave it the epoch.
  async fn claimed(db: &mut MultiplexedConnection, address: &str, epoch: i64) -> Layer {
    let layer = SHARD.claim(db, address).await.expect("a claim").expect("a layer to claim");
    count(db, layer, epoch, 0).await;
    layer
  }

  /// Renews the lease of `layer` with a count of `players`, as its writer does once it took each
  /// player of a place.
  async fn count(db: &mut MultiplexedConnection, layer: Layer, epoch: i64, players: u32) {
    let lease = Lease { layer, epoch: Epoch(epoch), address: "w", players, taken: &[] };
    assert_eq!(SHARD.lease(db, &lease).await.expect("a lease"), None);
    let () = db.del(keys::placed(SHARD.shard, layer)).await.expect("the places taken");
  }

  async fn hash(db: &mut MultiplexedConnection, key: &str) -> BTreeMap<String, String> {
    db.hgetall(key).await.expect("a hash")
  }

  fn fields<const N: usize>(fields: [(&str, &str); N]) -> BTreeMap<String, String> {
    fields.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
  }

  /// The presence of `account` on `layer`, with its `party`, as its writer writes it.
  async fn present(db: &mut MultiplexedConnection, account: i64, layer: Layer) {
    let fields = [
      (keys::SHARD, SHARD.shard.0.to_string()),
      (keys::ZONE, layer.zone.0.to_string()),
      (keys::LAYER, layer.number.to_string()),
    ];
    let () = db.hset_multiple(keys::presence(AccountId(account)), &fields).await.expect("a write");
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_full_layer_opens_the_next_number_which_takes_players_once_a_writer_claims_it() {
    let (_redis, mut db) = shard(2).await;
    assert_eq!(place(&mut db, 1, 1).await, Placed::Wait);
    assert_eq!(place(&mut db, 2, 1).await, Placed::Wait);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "0")]));
    assert_eq!(SHARD.claim(&mut db, "w1:4433").await.expect("a claim"), Some(on(1, 1)));
    assert_eq!(
      hash(&mut db, "writer:1:1:1").await,
      fields([("address", "w1:4433"), ("epoch", "0")])
    );
    let life: i64 = db.pttl("writer:1:1:1").await.expect("a life");
    assert!((2000..=3000).contains(&life), "{life}");
    assert_eq!(SHARD.claim(&mut db, "w2:4433").await.expect("a claim"), None);
    assert_eq!(place(&mut db, 1, 1).await, Placed::Wait, "the keeper did not answer the writer");
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "0")]));
    count(&mut db, on(1, 1), 1, 0).await;
    assert_eq!(place(&mut db, 1, 1).await, Placed::On(on(1, 1)));
    count(&mut db, on(1, 1), 1, 2).await;
    assert_eq!(place(&mut db, 3, 1).await, Placed::Wait);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "2"), ("1:2", "0")]));
    assert_eq!(claimed(&mut db, "w2:4433", 1).await, on(1, 2));
    assert_eq!(place(&mut db, 3, 1).await, Placed::On(on(1, 2)));
    count(&mut db, on(1, 1), 1, 1).await;
    count(&mut db, on(1, 2), 1, 1).await;
    assert_eq!(place(&mut db, 4, 1).await, Placed::On(on(1, 1)));
    count(&mut db, on(1, 1), 1, 0).await;
    assert_eq!(place(&mut db, 4, 1).await, Placed::On(on(1, 2)));
    assert_eq!(place(&mut db, 4, 2).await, Placed::Wait);
    assert_eq!(claimed(&mut db, "w3:4433", 1).await, on(2, 1));
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_place_counts_until_the_writer_takes_its_player_so_a_rush_never_passes_the_cap() {
    let (_redis, mut db) = shard(2).await;
    place(&mut db, 9, 1).await;
    assert_eq!(claimed(&mut db, "w", 1).await, on(1, 1));
    let rush = [place(&mut db, 1, 1).await, place(&mut db, 2, 1).await, place(&mut db, 3, 1).await];
    assert_eq!(rush, [Placed::On(on(1, 1)), Placed::On(on(1, 1)), Placed::Wait]);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "0"), ("1:2", "0")]));
    let places: Vec<String> = db.zrange("placed:1:1:1", 0, -1).await.expect("the places");
    assert_eq!(places, ["1", "2"]);
    let life: Vec<(String, u64)> =
      db.zrange_withscores("placed:1:1:1", 0, 0).await.expect("a life");
    let time: (u64, u64) = redis::cmd("TIME").query_async(&mut db).await.expect("a time");
    let left = life[0].1 - (time.0 * 1000 + time.1 / 1000);
    assert!((59_000..=60_000).contains(&left), "{left}");
    assert!(!SHARD.close(&mut db, on(1, 1), Epoch(1)).await.expect("a close"), "a place holds it");
    let () = db.zadd("placed:1:1:1", "1", 1).await.expect("an old place");
    let taken = [AccountId(2)];
    let lease = Lease { layer: on(1, 1), epoch: Epoch(1), address: "w", players: 1, taken: &taken };
    assert_eq!(SHARD.lease(&mut db, &lease).await.expect("a lease"), None);
    let places: Vec<String> = db.zrange("placed:1:1:1", 0, -1).await.expect("the places");
    assert_eq!(places, ["1"], "the lease ends the place of the player that the writer took");
    assert_eq!(hash(&mut db, "layers:1").await["1:1"], "1");
    count(&mut db, on(1, 1), 1, 0).await;
    let () = db.zadd("placed:1:1:1", "1", 1).await.expect("an old place");
    assert_eq!(place(&mut db, 3, 1).await, Placed::On(on(1, 1)), "an old place ends");
    let () = db.del("placed:1:1:1").await.expect("the players taken");
    assert!(SHARD.close(&mut db, on(1, 1), Epoch(1)).await.expect("a close"));
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn two_writers_never_take_one_layer_and_a_newer_epoch_keeps_its_lease() {
    let (_redis, mut db) = shard(20).await;
    place(&mut db, 1, 1).await;
    let (mut one, mut two) = (db.clone(), db.clone());
    let (first, second) = tokio::join!(SHARD.claim(&mut one, "a"), SHARD.claim(&mut two, "b"));
    let mut claims = [first.expect("a claim"), second.expect("a claim")];
    claims.sort_by_key(Option::is_some);
    assert_eq!(claims, [None, Some(on(1, 1))]);
    let lease = async |db: &mut MultiplexedConnection, epoch, address| {
      let lease = Lease { layer: on(1, 1), epoch: Epoch(epoch), address, players: 3, taken: &[] };
      SHARD.lease(db, &lease).await.expect("a lease")
    };
    assert_eq!(lease(&mut db, 2, "a").await, None);
    assert_eq!(lease(&mut db, 1, "b").await, Some(Epoch(2)));
    assert_eq!(hash(&mut db, "writer:1:1:1").await, fields([("address", "a"), ("epoch", "2")]));
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "3")]));
    assert_eq!(lease(&mut db, 3, "b").await, None);
    assert_eq!(hash(&mut db, "writer:1:1:1").await, fields([("address", "b"), ("epoch", "3")]));
    let lease = Lease { layer: on(1, 9), epoch: Epoch(1), address: "c", players: 4, taken: &[] };
    let closed = SHARD.lease(&mut db, &lease).await.expect("a lease");
    assert_eq!(closed, None);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "3")]));
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn an_empty_layer_closes_under_the_epoch_of_its_writer_and_frees_its_number() {
    let (redis, mut db) = shard(2).await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut channel = client.get_async_pubsub().await.expect("a connection");
    channel.subscribe(keys::DIRECTORY).await.expect("a channel");
    for (number, epoch) in [(1, 4), (2, 1), (3, 1)] {
      assert_eq!(place(&mut db, 1, 1).await, Placed::Wait);
      assert_eq!(claimed(&mut db, "w", epoch).await, on(1, number));
      count(&mut db, on(1, number), epoch, 2).await;
    }
    count(&mut db, on(1, 2), 1, 0).await;
    assert!(SHARD.drain(&mut db, on(1, 2)).await.expect("a drain"));
    assert!(!SHARD.close(&mut db, on(1, 1), Epoch(4)).await.expect("a close"));
    count(&mut db, on(1, 1), 4, 0).await;
    assert!(!SHARD.close(&mut db, on(1, 1), Epoch(3)).await.expect("a close"));
    assert!(SHARD.close(&mut db, on(1, 1), Epoch(4)).await.expect("a close"));
    assert!(SHARD.close(&mut db, on(1, 2), Epoch(1)).await.expect("a close"));
    assert!(!SHARD.close(&mut db, on(1, 2), Epoch(1)).await.expect("a close"));
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:3", "2")]));
    assert_eq!(hash(&mut db, "writer:1:1:1").await, fields([]));
    let draining: Vec<String> = db.smembers("draining:1").await.expect("a set");
    assert_eq!(draining, Vec::<String>::new());
    assert_eq!(place(&mut db, 1, 1).await, Placed::Wait);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "0"), ("1:3", "2")]));
    let mut named = Vec::new();
    let mut messages = channel.on_message();
    while named.len() < 10 {
      let message = messages.next().await.expect("a message");
      named.push(message.get_payload::<String>().expect("a layer"));
    }
    assert_eq!(
      named,
      ["1:1:1", "1:1:1", "1:1:2", "1:1:2", "1:1:3", "1:1:3", "1:1:2", "1:1:1", "1:1:2", "1:1:1"]
    );
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_layer_drains_only_when_the_other_open_layers_of_its_zone_have_room_for_its_players() {
    let (_redis, mut db) = shard(4).await;
    for number in [1, 2] {
      place(&mut db, 1, 1).await;
      assert_eq!(claimed(&mut db, "w", 1).await, on(1, number));
      count(&mut db, on(1, number), 1, 4).await;
    }
    count(&mut db, on(1, 1), 1, 3).await;
    count(&mut db, on(1, 2), 1, 2).await;
    assert!(!SHARD.drain(&mut db, on(1, 1)).await.expect("a drain"));
    assert!(!SHARD.drain(&mut db, on(1, 2)).await.expect("a drain"));
    assert!(!SHARD.drain(&mut db, on(1, 7)).await.expect("a drain"));
    count(&mut db, on(1, 2), 1, 1).await;
    assert!(SHARD.drain(&mut db, on(1, 2)).await.expect("a drain"));
    let draining: Vec<String> = db.smembers("draining:1").await.expect("a set");
    assert_eq!(draining, ["1:2"]);
    assert_eq!(cross(&mut db, 9, 1, on(1, 2)).await, Placed::On(on(1, 1)));
    count(&mut db, on(1, 1), 1, 4).await;
    assert_eq!(cross(&mut db, 9, 1, on(1, 2)).await, Placed::Wait);
    assert_eq!(hash(&mut db, "layers:1").await, fields([("1:1", "4"), ("1:2", "1"), ("1:3", "0")]));
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_party_stays_on_one_layer_of_a_zone_and_needs_room_for_each_member() {
    let (_redis, mut db) = shard(5).await;
    for layer in [on(1, 1), on(1, 2), on(2, 1), on(2, 2), on(3, 1), on(3, 3)] {
      db.hset::<_, _, _, ()>("layers:1", keys::field(layer), 0).await.expect("a layer");
      assert_eq!(SHARD.claim(&mut db, "w").await.expect("a claim"), Some(layer));
      count(&mut db, layer, 1, 0).await;
    }
    for (layer, players) in [(on(1, 1), 1), (on(1, 2), 1), (on(3, 1), 1), (on(3, 3), 3)] {
      count(&mut db, layer, 1, players).await;
    }
    let (a, b, c, d) = (1, 2, 3, 4);
    present(&mut db, a, on(1, 1)).await;
    present(&mut db, b, on(2, 2)).await;
    present(&mut db, c, on(1, 2)).await;
    present(&mut db, d, on(2, 2)).await;
    let join = async |db: &mut MultiplexedConnection, account| {
      SHARD.join(db, PartyId(7), AccountId(account)).await.expect("a join")
    };
    assert_eq!([join(&mut db, a).await, join(&mut db, b).await], [true, true]);
    assert!(!join(&mut db, c).await, "c is on another layer of the zone of a");
    assert!(!join(&mut db, b).await, "b has a party");
    assert!(!join(&mut db, 99).await, "99 does not play");
    assert!(join(&mut db, d).await);
    assert_eq!(cross(&mut db, b, 1, on(2, 2)).await, Placed::On(on(1, 1)));
    count(&mut db, on(1, 1), 1, 5).await;
    assert_eq!(cross(&mut db, b, 1, on(2, 2)).await, Placed::Wait);
    assert_eq!(cross(&mut db, b, 3, on(2, 2)).await, Placed::On(on(3, 1)));
    assert_eq!(cross(&mut db, c, 3, on(1, 2)).await, Placed::On(on(3, 3)));
    assert_eq!(cross(&mut db, c, 2, on(1, 2)).await, Placed::On(on(2, 2)));
    let leave = async |db: &mut MultiplexedConnection, account| {
      SHARD.leave(db, AccountId(account)).await.expect("a leave")
    };
    assert_eq!([leave(&mut db, b).await, leave(&mut db, c).await], [true, false]);
    let members: Vec<i64> = db.smembers("party:1:7").await.expect("a set");
    assert_eq!(members.len(), 2);
    assert!(leave(&mut db, d).await);
    assert_eq!(
      hash(&mut db, "presence:1").await,
      fields([("layer", "1"), ("shard", "1"), ("zone", "1")])
    );
    let gone: bool = db.exists("party:1:7").await.expect("a key");
    assert!(!gone);
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn a_party_holds_5_players_at_most() {
    let (_redis, mut db) = shard(5).await;
    for account in 1..=6 {
      present(&mut db, account, on(1, 1)).await;
    }
    let mut joined = Vec::new();
    for account in 1..=6 {
      joined.push(SHARD.join(&mut db, PartyId(1), AccountId(account)).await.expect("a join"));
    }
    assert_eq!(joined, [true, true, true, true, true, false]);
  }

  #[tokio::test]
  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  async fn each_script_gives_the_refusal_of_redis() {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut db = client.get_multiplexed_async_connection().await.expect("a connection");
    let said = |error: redis::RedisError| {
      (error.code().map(str::to_owned), error.detail().map(str::to_owned))
    };
    let no_cap = SHARD.place(&mut db, AccountId(1), ZoneId(1), None).await.err().map(said);
    let drained = SHARD.drain(&mut db, on(1, 1)).await.err().map(said);
    let expected = (Some("NOCAP".to_owned()), Some("shard 1 has no layer_cap".to_owned()));
    assert_eq!([no_cap, drained], [Some(expected.clone()), Some(expected)]);
    redis::cmd("ACL")
      .arg(&["SETUSER", "default", "-@scripting"])
      .query_async::<()>(&mut db)
      .await
      .expect("a user with no script");
    let mut db = client.get_multiplexed_async_connection().await.expect("a connection");
    let refusals = [
      SHARD.claim(&mut db, "w").await.is_err(),
      SHARD
        .lease(
          &mut db,
          &Lease { layer: on(1, 1), epoch: Epoch(1), address: "w", players: 0, taken: &[] },
        )
        .await
        .is_err(),
      SHARD.close(&mut db, on(1, 1), Epoch(1)).await.is_err(),
      SHARD.leave(&mut db, AccountId(1)).await.is_err(),
    ];
    assert_eq!(refusals, [true; 4]);
  }
}
