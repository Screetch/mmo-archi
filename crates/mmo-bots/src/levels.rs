//! The levels of population that the list of the realms gives, against the admitted players and
//! the cap of each realm in Redis.

use std::collections::BTreeMap;

use mmo_client::{Trust, realms};
use mmo_protocol::{Population, RealmId};
use mmo_store::keys;
use redis::aio::MultiplexedConnection;

use crate::Failure;

/// What the looks at the list of the realms showed.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Levels {
  /// The count of the looks.
  pub(crate) looks: u32,
  /// The realms of a look whose level was not the level of their admitted players and their cap,
  /// nor missing from the list, counted once at each look.
  pub(crate) apart: u32,
  /// The level of each realm at the last look, by its id.
  pub(crate) last: BTreeMap<u32, Population>,
}

/// The level of each of the `count` first realms, from the admitted players and the cap that
/// Redis holds.
async fn held(
  redis: &mut MultiplexedConnection,
  count: u32,
) -> redis::RedisResult<BTreeMap<u32, Population>> {
  let mut pipe = redis::pipe();
  for id in 1..=count {
    pipe.zcard(keys::admitted(id)).hget(keys::realm(RealmId(id)), keys::CAP);
  }
  let found: Vec<(u32, Option<u32>)> = pipe.query_async(redis).await?;
  let levels = (1..)
    .zip(found)
    .map(|(id, (admitted, cap))| (id, Population::of(admitted, cap.unwrap_or_default())));
  Ok(levels.collect())
}

/// Asks the login service at `login` for the list of the realms, and counts in `levels` each of
/// the `count` first realms that the list lacks or whose level is apart from the level of its
/// admitted players and its cap, as Redis holds them just before and just after the list.
pub(crate) async fn look(
  trust: &Trust,
  login: &str,
  redis: &mut MultiplexedConnection,
  count: u32,
  levels: &mut Levels,
) -> Result<(), Failure> {
  let before = held(redis, count).await;
  let listed = realms(trust, login).await?;
  let after = held(redis, count).await;
  let (before, after) = before.and_then(|before| after.map(|after| (before, after)))?;
  levels.looks += 1;
  levels.last.clear();
  for realm in listed {
    let id = realm.id.0;
    let matches = |held: &BTreeMap<u32, Population>| held.get(&id) == Some(&realm.population);
    if matches(&before) || matches(&after) {
      levels.last.insert(id, realm.population);
    }
  }
  levels.apart += count - u32::try_from(levels.last.len()).unwrap_or(count);
  Ok(())
}

#[cfg(test)]
mod tests {
  use mmo_client::testing::Fake;
  use mmo_protocol::{FromLogin, Population, Realm, RealmId, Rules, ToLogin};
  use mmo_store::keys;
  use mmo_store::testing::Redis;
  use redis::AsyncCommands;

  use super::{Levels, look};
  use crate::fixture::authority;

  fn realm(id: u32, population: Population) -> Realm {
    Realm {
      id: RealmId(id),
      name: format!("Realm{id}"),
      region: "Europe".to_owned(),
      language: "French".to_owned(),
      rules: Rules::Normal,
      population,
      queue: 0,
      login: "localhost:1".to_owned(),
    }
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_look_counts_each_realm_whose_level_is_apart_from_its_admitted_players_or_missing() {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
    for (id, admitted) in [(1, 2), (2, 1), (3, 0)] {
      let () = connection.hset(keys::realm(RealmId(id)), keys::CAP, 2).await.expect("a cap");
      for account in 0..admitted {
        let () = connection.zadd(keys::admitted(id), account, 1).await.expect("a slot");
      }
    }
    let listed =
      vec![realm(1, Population::Full), realm(2, Population::Low), realm(3, Population::Low)];
    let login = Fake::start(move |mut peer| async move {
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Realms);
      peer.write(&FromLogin::Realms(listed)).await;
      peer
    });
    let (_folder, path) = authority(&[&login]);
    let trust = mmo_client::Trust::from_pem(&path).expect("a trust");
    let mut levels = Levels::default();
    look(&trust, &login.address, &mut connection, 4, &mut levels).await.expect("a look");
    login.end().await;
    let last = [(1, Population::Full), (3, Population::Low)].into_iter().collect();
    assert_eq!(levels, Levels { looks: 1, apart: 2, last });
    let closed = look(&trust, "127.0.0.1:0", &mut connection, 1, &mut levels).await;
    let closed = closed.map_err(|failure| failure.to_string());
    assert_eq!(
      closed,
      Err("the client cannot connect: invalid remote address: 127.0.0.1:0".to_owned())
    );
    redis::cmd("ACL")
      .arg(&["SETUSER", "blind", "on", "nopass", "~*", "&*", "+@all", "-zcard"])
      .query_async::<()>(&mut connection)
      .await
      .expect("a user");
    let url = format!("redis://blind:any@127.0.0.1:{}", redis.port());
    let blind = redis::Client::open(url).expect("a URL");
    let mut blind = blind.get_multiplexed_async_connection().await.expect("a connection");
    let answering = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Realms(vec![])).await;
      peer
    });
    let (_folder, path) = authority(&[&answering]);
    let trust = mmo_client::Trust::from_pem(&path).expect("a trust");
    let denied = look(&trust, &answering.address, &mut blind, 1, &mut levels).await;
    answering.end().await;
    let denied = denied.map_err(|failure| failure.to_string()).err().unwrap_or_default();
    assert!(denied.starts_with("Redis refuses the bots: "), "{denied}");
    assert_eq!(levels.looks, 1);
  }
}
