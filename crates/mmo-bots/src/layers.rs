//! The layers that the writers of each shard opened, and the players on each, from the directory
//! in Redis.

use std::collections::BTreeMap;

use mmo_protocol::{ShardId, ZoneId};
use mmo_store::keys;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

/// The players of one layer at the last look, and the most that it held at any look.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Players {
  pub(crate) now: u32,
  pub(crate) most: u32,
}

/// What the looks at the directory showed of one shard: the cap of its layers, which its login
/// service writes, and each layer that stood, by its number. A shard with no cap shows 0.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Shard {
  pub(crate) cap: u32,
  pub(crate) layers: BTreeMap<u32, Players>,
}

/// What the looks showed, by the id of the shard.
pub(crate) type Seen = BTreeMap<u32, Shard>;

/// The players of each layer of `zone` in the directory of `shard`, by the number of the layer.
pub(crate) async fn read(
  redis: &mut MultiplexedConnection,
  shard: u32,
  zone: ZoneId,
) -> redis::RedisResult<BTreeMap<u32, u32>> {
  let fields: Vec<(String, u32)> = redis.hgetall(keys::layers(ShardId(shard))).await?;
  let layer = |(field, players): (String, u32)| {
    let (found, number) = field.split_once(':')?;
    (found.parse::<i32>().ok()? == zone.0).then_some(())?;
    Some((number.parse().ok()?, players))
  };
  Ok(fields.into_iter().filter_map(layer).collect())
}

/// Looks at the directory of each of the `shards` first shards, and adds what it shows to `seen`.
/// A layer that no longer stands holds no player now.
pub(crate) async fn look(
  redis: &mut MultiplexedConnection,
  shards: u32,
  zone: ZoneId,
  seen: &mut Seen,
) -> redis::RedisResult<()> {
  for id in 1..=shards {
    let shard = seen.entry(id).or_default();
    let cap: Option<u32> = redis.hget(keys::shard(ShardId(id)), keys::LAYER_CAP).await?;
    shard.cap = cap.unwrap_or_default();
    shard.layers.values_mut().for_each(|layer| layer.now = 0);
    for (number, now) in read(redis, id, zone).await? {
      let players = shard.layers.entry(number).or_default();
      (players.now, players.most) = (now, players.most.max(now));
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use mmo_protocol::ZoneId;
  use mmo_store::testing::Redis;
  use redis::AsyncCommands;

  use super::{Players, Seen, Shard, look, read};

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_looks_keep_the_layers_of_each_shard_with_their_players_and_their_most() {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
    let zone = ZoneId(1);
    assert_eq!(read(&mut connection, 1, zone).await.expect("a read"), BTreeMap::new());
    let () = connection.hset("shard:1", "layer_cap", 20).await.expect("a cap");
    for (field, players) in
      [("1:1", 5), ("1:2", 3), ("2:1", 9), ("strange", 1), ("x:1", 1), ("1:x", 1)]
    {
      let () = connection.hset("layers:1", field, players).await.expect("a layer");
    }
    let mut seen = Seen::new();
    look(&mut connection, 2, zone, &mut seen).await.expect("a look");
    let players = |now, most| Players { now, most };
    let first = Shard { cap: 20, layers: BTreeMap::from([(1, players(5, 5)), (2, players(3, 3))]) };
    assert_eq!(seen, Seen::from([(1, first), (2, Shard::default())]));
    let () = connection.hset("layers:1", "1:1", 2).await.expect("a count");
    let () = connection.hdel("layers:1", "1:2").await.expect("a close");
    look(&mut connection, 2, zone, &mut seen).await.expect("a look");
    let layers = BTreeMap::from([(1, players(2, 5)), (2, players(0, 3))]);
    assert_eq!(seen.get(&1), Some(&Shard { cap: 20, layers }));
    redis::cmd("ACL")
      .arg(&["SETUSER", "default", "-hget"])
      .query_async::<()>(&mut connection)
      .await
      .expect("a user with no hget");
    let mut denied = client.get_multiplexed_async_connection().await.expect("a connection");
    assert!(look(&mut denied, 1, zone, &mut seen).await.is_err(), "Redis refuses the cap");
    redis::cmd("ACL")
      .arg(&["SETUSER", "default", "+hget", "-hgetall"])
      .query_async::<()>(&mut denied)
      .await
      .expect("a user with no hgetall");
    let mut blind = client.get_multiplexed_async_connection().await.expect("a connection");
    assert!(look(&mut blind, 1, zone, &mut seen).await.is_err(), "Redis refuses the layers");
  }
}
