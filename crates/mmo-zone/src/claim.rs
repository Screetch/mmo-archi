//! The claim of a zone: the epoch that its writer holds, and the entries that the writer keeps in
//! Redis, its own in the directory of zones and the presence of each of its players.

use std::sync::LazyLock;

use mmo_protocol::{AccountId, Batch, CharacterId, Epoch, Save, ZoneId};
use mmo_store::keys;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};

/// The life of the entry of a zone, which the writer renews each second.
const ENTRY_LIFE_MS: u64 = 3000;

/// The life of the presence of a player, which the writer renews each 10 s.
const PRESENCE_LIFE_S: i64 = 30;

static ENTRY: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("entry.lua")));

/// Why the claim of a zone fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error(
    "zone {} is held by a writer of epoch {held}, which is newer than the epoch {}",
    zone.0,
    own.0
  )]
  Superseded { zone: ZoneId, held: i64, own: Epoch },
  #[error("Redis refuses the zone writer: {0}")]
  Redis(#[from] redis::RedisError),
}

/// A write of the writer in Redis.
pub(crate) enum Entry {
  /// The entry of the zone, with the count of its players.
  Zone { players: usize },
  /// The presence of these players, which the writer holds now.
  Present(Vec<(AccountId, CharacterId)>),
  /// The end of the presence of these accounts.
  Absent(Vec<AccountId>),
}

/// The zone of a writer, the epoch that the keeper gave it, and its connection to Redis.
#[derive(Clone)]
pub(crate) struct Claim {
  redis: ConnectionManager,
  zone: ZoneId,
  epoch: Epoch,
  address: String,
}

impl Claim {
  /// Writes the entry of `zone` in the directory with `epoch` and the `address` that the gateways
  /// dial, and names the zone on the channel of the directory. It fails when the entry holds a
  /// newer epoch, since a writer of that epoch has the zone.
  pub(crate) async fn open(
    url: &str,
    zone: ZoneId,
    epoch: Epoch,
    address: String,
  ) -> Result<Self, Error> {
    let redis = ConnectionManager::new(redis::Client::open(url)?).await?;
    let claim = Self { redis, zone, epoch, address };
    claim.write(Entry::Zone { players: 0 }).await?;
    claim.redis.clone().publish::<_, _, ()>(keys::DIRECTORY, zone.0).await?;
    Ok(claim)
  }

  /// The batch of `saves` for the keeper, with the epoch of this writer.
  pub(crate) fn batch(&self, saves: Vec<Save>) -> Batch {
    Batch { zone: self.zone, epoch: self.epoch, saves }
  }

  pub(crate) async fn write(&self, entry: Entry) -> Result<(), Error> {
    let mut redis = self.redis.clone();
    match entry {
      Entry::Zone { players } => {
        let held: i64 = ENTRY
          .key(keys::zone(self.zone))
          .arg(ENTRY_LIFE_MS)
          .arg(keys::EPOCH)
          .arg(self.epoch.0)
          .arg(keys::ADDRESS)
          .arg(&self.address)
          .arg(keys::PLAYERS)
          .arg(players)
          .invoke_async(&mut redis)
          .await?;
        match held {
          0 => Ok(()),
          held => Err(Error::Superseded { zone: self.zone, held, own: self.epoch }),
        }
      }
      Entry::Present(players) if players.is_empty() => Ok(()),
      Entry::Absent(accounts) if accounts.is_empty() => Ok(()),
      Entry::Present(players) => {
        let mut batch = redis::pipe();
        for (account, character) in players {
          let key = keys::presence(account);
          let fields = [
            (keys::CHARACTER, character.0),
            (keys::ZONE, i64::from(self.zone.0)),
            (keys::EPOCH, self.epoch.0),
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
  use futures_util::StreamExt;
  use mmo_protocol::Batch;
  use mmo_protocol::{AccountId, CharacterId, Epoch, ZoneId};
  use mmo_store::keys;
  use mmo_store::testing::Redis;
  use sqlx::PgPool;

  use super::{Claim, Entry, Error};
  use crate::peer::{hash, life};

  const MEADOW: ZoneId = ZoneId(1);

  async fn claim(redis: &Redis, epoch: i64) -> Result<Claim, Error> {
    Claim::open(&redis.url(), MEADOW, Epoch(epoch), "zone-1:4433".to_owned()).await
  }

  fn fields<const N: usize>(
    fields: [(&str, &str); N],
  ) -> std::collections::BTreeMap<String, String> {
    fields.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_writes_its_entry_for_3_s_and_names_its_zone_on_the_directory(_: PgPool) {
    let redis = Redis::start().await;
    let client = redis::Client::open(redis.url()).expect("a URL");
    let mut channel = client.get_async_pubsub().await.expect("a connection");
    channel.subscribe(keys::DIRECTORY).await.expect("a channel");
    let claim = claim(&redis, 1).await.expect("a claim");
    let named: i32 = channel
      .on_message()
      .next()
      .await
      .expect("a message")
      .get_payload()
      .expect("the id of a zone");
    assert_eq!(named, 1);
    let entry =
      |players| fields([("address", "zone-1:4433"), ("epoch", "1"), ("players", players)]);
    assert_eq!(hash(&redis, "zone:1").await, entry("0"));
    claim.write(Entry::Zone { players: 3 }).await.expect("the entry again");
    assert_eq!(hash(&redis, "zone:1").await, entry("3"));
    assert!((1..=3000).contains(&life(&redis, "zone:1").await));
    let batch = Batch { zone: MEADOW, epoch: Epoch(1), saves: Vec::new() };
    assert_eq!(claim.batch(Vec::new()), batch);
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_stops_when_the_entry_holds_a_newer_epoch_or_redis_is_not_a_url(_: PgPool) {
    let redis = Redis::start().await;
    let newer = claim(&redis, 9).await.expect("a claim");
    let older = claim(&redis, 4).await.err().map(|error| error.to_string());
    assert_eq!(
      older.as_deref(),
      Some("zone 1 is held by a writer of epoch 9, which is newer than the epoch 4")
    );
    assert_eq!(hash(&redis, "zone:1").await["epoch"], "9");
    newer.write(Entry::Zone { players: 1 }).await.expect("the newest epoch writes");
    let bad = Claim::open("nope", MEADOW, Epoch(1), String::new()).await.err();
    assert!(matches!(bad, Some(Error::Redis(_))), "{bad:?}");
  }

  #[sqlx::test(migrations = false)]
  async fn the_presence_of_a_player_lives_30_s_and_ends_with_its_account(_: PgPool) {
    let redis = Redis::start().await;
    let claim = claim(&redis, 2).await.expect("a claim");
    let players = vec![(AccountId(5), CharacterId(7)), (AccountId(6), CharacterId(8))];
    claim.write(Entry::Present(players)).await.expect("two players");
    claim.write(Entry::Present(Vec::new())).await.expect("no player");
    claim.write(Entry::Absent(Vec::new())).await.expect("no account");
    let present = |character| fields([("character", character), ("zone", "1"), ("epoch", "2")]);
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
  async fn a_writer_names_each_command_that_redis_refuses(_: PgPool) {
    let redis = Redis::start().await;
    let open = async |url: &str| Claim::open(url, MEADOW, Epoch(1), String::new()).await;
    let stranger = format!("redis://nobody:any@127.0.0.1:{}", redis.port());
    let mut faults = vec![open(&stranger).await.err()];
    for (name, rules) in [("noscript", "-@scripting"), ("nopublish", "-publish")] {
      faults.push(open(&user(&redis, name, &[rules]).await).await.err());
    }
    assert!(faults.iter().all(|fault| matches!(fault, Some(Error::Redis(_)))), "{faults:?}");

    let present = vec![(AccountId(5), CharacterId(7))];
    let url = user(&redis, "noexpire", &["-expire"]).await;
    let claim = open(&url).await.expect("a claim");
    let refused = claim.write(Entry::Present(present)).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
    let url = user(&redis, "nodel", &["-del"]).await;
    let claim = open(&url).await.expect("a claim");
    let refused = claim.write(Entry::Absent(vec![AccountId(5)])).await.err();
    assert!(matches!(refused, Some(Error::Redis(_))), "{refused:?}");
  }
}
