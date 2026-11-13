//! The accounts and the characters that the bots play. The login service makes 5 new accounts an
//! hour for one address, so a load test makes its accounts in PostgreSQL, as the login service
//! does, and then logs in through the login service as each player does.

use futures_util::future::try_join_all;
use mmo_client::Account;
use mmo_protocol::{Angle, CharacterId, Password, Position, ShardId, State};
use mmo_store::{NewCharacter, Store};

use crate::Failure;
use crate::config::Config;
use crate::rng::Rng;

/// The accounts that the bots make at once: each one hashes a password, which takes 19 MiB.
pub(crate) const SEEDERS: u32 = 8;

/// The password of each account of the bots.
const PASSWORD: &str = "a password of the bots";

/// The hit points of a character of the bots, as the login service gives them.
const HIT_POINTS: i32 = 100;

/// A player of the load test: its account, and the character that the account holds.
pub(crate) struct Bot {
  pub(crate) index: u32,
  /// The shard that the bot plays in, and the address of the login service of that shard.
  pub(crate) shard: u32,
  pub(crate) login: String,
  pub(crate) account: Account,
  pub(crate) character: CharacterId,
}

/// The `width` letters, from a to z, of the number `n`, the least digit first.
pub(crate) fn letters(mut n: u64, width: usize) -> String {
  (0..width)
    .map(|_| {
      let letter = char::from(b'a' + u8::try_from(n % 26).unwrap_or_default());
      n /= 26;
      letter
    })
    .collect()
}

/// Where the bot `index` starts: the first `town` bots stand in one square of 128 m, and the others
/// anywhere in the zone, in centimetres on the floor.
fn start(config: &Config, index: u32) -> State {
  let mut rng = Rng::new(config.seed ^ u64::from(index));
  let (origin, span) = if index < config.town { (10_000, 12_800) } else { (1_000, 100_400) };
  let mut coordinate = || origin + i32::try_from(rng.below(span)).unwrap_or_default();
  let position = Position { x: coordinate(), y: 0, z: coordinate() };
  State { position, yaw: Angle(0), hit_points: HIT_POINTS }
}

/// Makes the account and the character of the bot `index` in the store.
async fn make(store: &Store, config: &Config, index: u32) -> Result<Bot, Failure> {
  let run = &config.run;
  let (name, character) =
    (format!("bot{run}{index}"), format!("B{run}{}", letters(index.into(), 4)));
  let id = store.create_account(&name, Password(PASSWORD.to_owned())).await?;
  let state = start(config, index);
  let (shard, login) = config.shard(index);
  let new =
    NewCharacter { account: id, shard: ShardId(shard), name: &character, zone: config.zone, state };
  let saved = store.create_character(&new).await?;
  let password = Password(PASSWORD.to_owned());
  let account = Account { name, password, character, create: false };
  Ok(Bot { index, shard, login: login.to_owned(), account, character: saved })
}

/// Makes the account and the character of each bot, `SEEDERS` at once.
pub(crate) async fn seed(store: &Store, config: &Config) -> Result<Vec<Bot>, Failure> {
  let mut bots = Vec::new();
  let indexes: Vec<u32> = (0..config.bots).collect();
  for chunk in indexes.chunks(SEEDERS as usize) {
    bots.extend(try_join_all(chunk.iter().map(|index| make(store, config, *index))).await?);
  }
  Ok(bots)
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use mmo_protocol::{Password, Position, ShardId, ZoneId};
  use mmo_store::Store;
  use sqlx::PgPool;

  use super::{letters, seed};
  use crate::config::Config;

  fn config(bots: u32, town: u32) -> Config {
    Config {
      authority: "ca.pem".into(),
      login: "a:1,b:2".to_owned(),
      gateway: String::new(),
      database: String::new(),
      redis: String::new(),
      zone: ZoneId(3),
      bots,
      waiting: 0,
      town,
      ramp: 1,
      join: Duration::ZERO,
      play: Duration::ZERO,
      recover: Duration::ZERO,
      run: "abc".to_owned(),
      seed: 4,
    }
  }

  #[sqlx::test(migrations = false)]
  async fn each_bot_gets_an_account_and_a_character_and_a_second_run_with_the_same_names_fails(
    pool: PgPool,
  ) {
    let store = Store::migrate(pool).await.expect("a migrated database");
    store.add_zone(ZoneId(3), "meadow").await.expect("a zone");
    let bots = seed(&store, &config(10, 4)).await.expect("ten bots");
    let names: Vec<_> = bots.iter().map(|bot| (bot.index, bot.account.name.as_str())).collect();
    assert_eq!(names.first(), Some(&(0, "botabc0")));
    assert_eq!(names.last(), Some(&(9, "botabc9")));
    assert_eq!(bots[1].account.character, "Babcbaaa");
    let shards: Vec<_> = bots.iter().take(3).map(|bot| (bot.shard, bot.login.as_str())).collect();
    assert_eq!(shards, [(1, "a:1"), (2, "b:2"), (1, "a:1")]);
    for bot in bots.iter().take(3) {
      let password = Password("a password of the bots".to_owned());
      let account = store.sign_in(&bot.account.name, password).await.expect("a sign in");
      let account = account.expect("an account");
      let listed = store.characters(account, ShardId(bot.shard)).await.expect("the characters");
      assert_eq!(listed.iter().map(|one| one.id).collect::<Vec<_>>(), [bot.character]);
    }
    let ids: Vec<_> = bots.iter().map(|bot| bot.character).collect();
    let saved = store.load(&ids).await.expect("the characters");
    let at = |index: usize| {
      let found = saved.iter().find(|one| one.character == bots[index].character);
      let Position { x, y, z } = found.expect("a saved character").state.position;
      (x, y, z)
    };
    let town = |index: usize| {
      let (x, y, z) = at(index);
      (10_000..22_800).contains(&x) && (10_000..22_800).contains(&z) && y == 0
    };
    let spread = |index: usize| {
      let (x, _, z) = at(index);
      (1_000..101_400).contains(&x) && (1_000..101_400).contains(&z)
    };
    assert!((0..4).all(town) && (4..10).all(spread));
    assert!(saved.iter().all(|one| one.zone == ZoneId(3) && one.state.hit_points == 100));
    let again = seed(&store, &config(1, 0)).await.err().map(|error| error.to_string());
    assert_eq!(again.as_deref(), Some("the store refuses the bots: the name botabc0 is taken"));
    let elsewhere = Config { zone: ZoneId(99), run: "def".to_owned(), ..config(1, 0) };
    let lost = seed(&store, &elsewhere).await.err().map(|error| error.to_string());
    assert!(lost.is_some_and(|text| text.starts_with("the store refuses the bots")));
  }

  #[test]
  fn letters_write_a_number_in_base_26_with_the_least_digit_first() {
    assert_eq!(
      [letters(0, 4), letters(1, 4), letters(26, 4), letters(27, 3), letters(25, 1)],
      ["aaaa", "baaa", "abaa", "bba", "z"]
    );
  }
}
