//! The times of the ticks of the writers of the layers of a zone, from the hash that each keeps in
//! Redis.

use std::collections::BTreeMap;

use mmo_protocol::{Layer, ShardId, ZoneId};
use mmo_store::keys;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

/// The count of ticks in each bucket of `keys::ticks`.
pub(crate) type Counts = BTreeMap<u32, u64>;

/// The ticks of each layer of `zone` in the directory of `shard` as they stand now, added up in
/// each bucket.
pub(crate) async fn read(
  redis: &mut MultiplexedConnection,
  shard: ShardId,
  zone: ZoneId,
) -> redis::RedisResult<Counts> {
  let fields: Vec<String> = redis.hkeys(keys::layers(shard)).await?;
  let mut counts = Counts::new();
  for field in fields {
    let numbers = field.split_once(':').map(|(z, l)| (z.parse::<i32>(), l.parse::<u32>()));
    let Some((Ok(found), Ok(number))) = numbers else {
      continue;
    };
    if found == zone.0 {
      let layer: Counts = redis.hgetall(keys::ticks(shard, Layer { zone, number })).await?;
      for (bucket, count) in layer {
        *counts.entry(bucket).or_default() += count;
      }
    }
  }
  Ok(counts)
}

/// The ticks that came between two reads.
pub(crate) fn since(before: &Counts, after: &Counts) -> Counts {
  let grown = |(bucket, count): (&u32, &u64)| {
    (*bucket, count.saturating_sub(before.get(bucket).copied().unwrap_or_default()))
  };
  after.iter().map(grown).filter(|(_, count)| *count > 0).collect()
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use mmo_protocol::{ShardId, ZoneId};
  use mmo_store::testing::Redis;
  use redis::AsyncCommands;

  use super::{read, since};

  #[test]
  fn the_ticks_between_two_reads_are_the_growth_of_each_bucket() {
    let before = BTreeMap::from([(3, 10), (5, 4)]);
    let after = BTreeMap::from([(3, 25), (5, 4), (9, 2)]);
    assert_eq!(since(&before, &after), BTreeMap::from([(3, 15), (9, 2)]));
    assert_eq!(since(&after, &before), BTreeMap::new());
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_hashes_of_the_layers_of_a_zone_add_up_in_each_bucket() {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
    let (shard, zone) = (ShardId(1), ZoneId(1));
    assert_eq!(read(&mut connection, shard, zone).await.expect("a read"), BTreeMap::new());
    for field in ["1:1", "1:2", "2:1", "strange"] {
      let () = connection.hset("layers:1", field, 0).await.expect("a layer");
    }
    for (key, bucket, count) in
      [("ticks:1:1:1", 4, 7), ("ticks:1:1:2", 4, 1), ("ticks:1:1:2", 12, 2), ("ticks:1:2:1", 4, 9)]
    {
      let () = connection.hincr(key, bucket, count).await.expect("a count");
    }
    assert_eq!(
      read(&mut connection, shard, zone).await.expect("a read"),
      BTreeMap::from([(4, 8), (12, 2)])
    );
    redis::cmd("ACL")
      .arg(&["SETUSER", "default", "-hgetall"])
      .query_async::<()>(&mut connection)
      .await
      .expect("a user with no hgetall");
    let mut denied = client.get_multiplexed_async_connection().await.expect("a connection");
    assert!(read(&mut denied, shard, zone).await.is_err(), "Redis refuses the hash of a layer");
    redis::cmd("ACL")
      .arg(&["SETUSER", "default", "-hkeys"])
      .query_async::<()>(&mut denied)
      .await
      .expect("a user with no hkeys");
    let mut denied = client.get_multiplexed_async_connection().await.expect("a connection");
    assert!(read(&mut denied, shard, zone).await.is_err(), "Redis refuses the layers");
  }
}
