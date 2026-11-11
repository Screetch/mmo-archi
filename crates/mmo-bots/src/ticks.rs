//! The times of the ticks of a zone writer, from the hash that it keeps in Redis.

use std::collections::BTreeMap;

use mmo_protocol::ZoneId;
use mmo_store::keys;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

/// The count of ticks in each bucket of `keys::ticks`.
pub(crate) type Counts = BTreeMap<u32, u64>;

/// The hash of the ticks of `zone` as it stands now.
pub(crate) async fn read(
  redis: &mut MultiplexedConnection,
  zone: ZoneId,
) -> redis::RedisResult<Counts> {
  redis.hgetall(keys::ticks(zone)).await
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

  use mmo_protocol::ZoneId;
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
  async fn the_hash_of_a_zone_reads_as_counts_of_buckets() {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
    assert_eq!(read(&mut connection, ZoneId(1)).await.expect("a read"), BTreeMap::new());
    let () = connection.hincr("ticks:1", 4, 7).await.expect("a count");
    let () = connection.hincr("ticks:1", 12, 2).await.expect("a count");
    assert_eq!(
      read(&mut connection, ZoneId(1)).await.expect("a read"),
      BTreeMap::from([(4, 7), (12, 2)])
    );
  }
}
