//! The load test of mmo-archi: headless clients that log in through the login service, wait in
//! its queue, link to a gateway, and send the inputs of a walk 20 times each second, as the
//! players do. It measures the time of each tick of the layer writer, the bytes of each client, the
//! wait in the queue and the faults, and prints a report against the budget of
//! `docs/architecture.md`.
//!
//! The login service makes only 5 new accounts an hour for one address, so [`run`] first makes the
//! accounts and the characters in PostgreSQL, with `mmo-store` as the login service does, and then
//! each bot logs in through the login service. The tick times come from the hash that the writer
//! of each layer of the zone keeps in Redis, `mmo_store::keys::ticks`: [`run`] reads them before
//! and after the measure and takes the difference. The bots go round the shards: the bot
//! with the index `i` plays in the shard `i % n + 1`, through the login service of that shard. Each
//! second of the measure, [`run`] reads the directory of each shard, `mmo_store::keys::layers`, and
//! the report tells the layers that opened in each shard and the players on each, and fails when a
//! layer passes its cap or a shard opens fewer layers than its players need. When two shards play,
//! it also counts the bots of another shard that a snapshot gave to a bot, and fails when one did.
//! The bytes are the sizes of the datagrams, with no header of IP, UDP or QUIC, which add about 60
//! bytes to each datagram.
//!
//! [`run`] reads these variables of its environment:
//!
//! - `MMO_BOTS_AUTHORITY`: the PEM file of the certificates that sign the login service and the
//!   gateway.
//! - `MMO_BOTS_LOGIN` and `MMO_BOTS_GATEWAY`: their addresses, as `host:port`. Each host is the
//!   name that its certificate must hold. `MMO_BOTS_LOGIN` holds the address of the login service
//!   of each shard, with commas between them, so the bots play in as many shards as it names.
//! - `DATABASE_URL` and `REDIS_URL`: the database and the Redis of the game.
//! - `MMO_BOTS_ZONE` and `MMO_BOTS_COUNT`: the zone where the characters stand, which the table
//!   `zones` holds, and the count of bots, which is at most 500, since no test runs more.
//! - `MMO_BOTS_WAITING`: how many of the bots the queue must hold when the measure ends, as `100`.
//!   The measure waits for the other bots to play, and the report fails when another count of bots
//!   waits, when a bot with a session entered the queue after one that waits, when a waiting bot
//!   holds no place of its order of entry, or when it gets no place for more than 7 s. The default
//!   is 0.
//! - `MMO_BOTS_CROSSERS`: how many of the first bots start within 30 m of the east border of the
//!   zone and walk east, which makes each of them cross it, as `40`. The other bots log in after
//!   each of them played, so the crossers stand on the first layer of their shard. Give at most
//!   as many crossers to a shard as a layer holds. The report fails when a crosser did not enter
//!   the next zone, and when one entered a layer with another number than its own. The default
//!   is 0.
//! - `MMO_BOTS_TOWN`: how many of the bots start in one square of 128 m, as `250`. The others
//!   start anywhere in the zone. The default is 0.
//! - `MMO_BOTS_RAMP`: the logins that start each second. The default is 50.
//! - `MMO_BOTS_JOIN_SECONDS`: the time that the measure waits for each bot to play, as `60`. A
//!   bot that waits in the queue for longer does not count in the measure of the ticks.
//! - `MMO_BOTS_RECOVER_SECONDS`: when it is not 0, a bot that loses its link after it played
//!   attaches to the gateway again with its token, as a client does, and the report fails when
//!   a bot is not back in play within that time, counted from its last snapshot, when a bot is
//!   still away at the end, or when a bot comes back on another layer. The default is 0.
//! - `MMO_BOTS_PLAY_SECONDS`: the time of the measure, as `30`.
//! - `MMO_BOTS_RUN`: 1 to 6 letters from a to z in the name of each account, as `abc`, so a run
//!   makes new accounts. The default comes from the clock.
//! - `MMO_BOTS_SEED`: the seed of the walks and of the places. The default is 1.

mod bot;
mod config;
#[cfg(test)]
mod fixture;
mod layers;
mod report;
mod rng;
mod seed;
mod ticks;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mmo_client::Trust;
use mmo_store::Store;
use redis::aio::MultiplexedConnection;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

use bot::{Outcome, Shared};
use config::Config;
use layers::Seen;
use report::Report;
use seed::SEEDERS;

pub use config::Error as ConfigError;

/// Why the load test does not end.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
  /// The environment lacks a variable, or holds a bad one.
  #[error(transparent)]
  Config(#[from] ConfigError),
  /// The authority of the servers cannot be used.
  #[error(transparent)]
  Fault(#[from] mmo_client::Fault),
  /// PostgreSQL cannot be reached.
  #[error("PostgreSQL refuses the bots: {0}")]
  Database(#[from] sqlx::Error),
  /// The store refuses an account or a character.
  #[error("the store refuses the bots: {0}")]
  Store(#[from] mmo_store::Error),
  /// Redis cannot be reached.
  #[error("Redis refuses the bots: {0}")]
  Redis(#[from] redis::RedisError),
}

/// Runs the load test as the environment that `read` gives configures it, and prints its report.
pub async fn run(read: &dyn Fn(&str) -> Option<String>) -> Result<(), Failure> {
  let config = Config::read(read)?;
  println!("{}", measure(&config).await?);
  Ok(())
}

/// Waits until each bot plays or failed, or until `limit` passes, and says its progress.
async fn settle(shared: &Shared, bots: usize, limit: Duration) {
  let end = Instant::now() + limit;
  let mut waits = 0_u32;
  loop {
    let ready = shared.ready.load(Ordering::Relaxed);
    if waits.is_multiple_of(50) {
      eprintln!("{ready} of {bots} bots play or failed");
    }
    if ready >= bots || Instant::now() >= end {
      return;
    }
    waits += 1;
    sleep(Duration::from_millis(100)).await;
  }
}

/// Looks at the directory of the layers each second until the measure ends, and once at its end.
async fn look_during(
  redis: &mut MultiplexedConnection,
  config: &Config,
  seen: &mut Seen,
) -> redis::RedisResult<()> {
  let end = Instant::now() + config.play;
  loop {
    layers::look(redis, config.shards(), config.zone, seen).await?;
    let left = end.saturating_duration_since(Instant::now());
    if left.is_zero() {
      return Ok(());
    }
    sleep(left.min(Duration::from_secs(1))).await;
  }
}

async fn measure(config: &Config) -> Result<Report, Failure> {
  let trust = Trust::from_pem(&config.authority)?;
  let pool = PgPoolOptions::new()
    .max_connections(SEEDERS)
    .acquire_timeout(Duration::from_secs(5))
    .connect(&config.database)
    .await?;
  let store = Store::open(pool).await?;
  eprintln!("making {} accounts", config.bots);
  let bots = seed::seed(&store, config).await?;
  let client = redis::Client::open(config.redis.as_str())?;
  let mut redis = client.get_multiplexed_async_connection().await?;
  let gateway = config.gateway.clone();
  let (zone, seed, ready) = (config.zone, config.seed, AtomicUsize::new(0));
  let reconnect = !config.recover.is_zero();
  let crossers = usize::try_from(config.crossers).unwrap_or(usize::MAX);
  let shards = bots.iter().map(|bot| (bot.character.0, bot.shard)).collect();
  let shared = Arc::new(Shared { trust, gateway, zone, seed, ready, reconnect, crossers, shards });
  let (stop, stopped) = watch::channel(false);
  let mut tasks = JoinSet::new();
  for bot in bots {
    let (shared, stopped) = (Arc::clone(&shared), stopped.clone());
    let delay = Duration::from_secs_f64(f64::from(bot.index) / f64::from(config.ramp.max(1)));
    tasks.spawn(async move {
      sleep(delay).await;
      bot::run(&shared, &bot, stopped).await
    });
  }
  settle(&shared, tasks.len().saturating_sub(config.waiting as usize), config.join).await;
  let before = ticks::read(&mut redis, config.shards(), config.zone).await;
  let mut opened = Seen::new();
  let looked = look_during(&mut redis, config, &mut opened).await;
  let after = ticks::read(&mut redis, config.shards(), config.zone).await;
  stop.send_replace(true);
  let mut outcomes: Vec<Outcome> = Vec::new();
  while let Some(done) = tasks.join_next().await {
    // A bot that panicked gives no outcome, and the report counts it as failed.
    outcomes.extend(done);
  }
  let counts = before.and_then(|before| after.map(|after| ticks::since(&before, &after)))?;
  looked?;
  Ok(Report::new(
    usize::try_from(config.bots).unwrap_or(usize::MAX),
    usize::try_from(config.waiting).unwrap_or(usize::MAX),
    config.recover,
    &outcomes,
    &counts,
    opened,
    config.play,
  ))
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use mmo_client::testing::{Fake, hold};
  use mmo_protocol::{Attach, Password, ZoneId};
  use mmo_store::Store;
  use mmo_store::testing::Redis;
  use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
  use sqlx::{ConnectOptions, PgPool};

  use super::{Config, measure, run};
  use crate::fixture::{authority, entity, login_service, send, snapshot};

  /// The variables of a run, and the `more` that replace some of them.
  fn vars(
    authority: &Path,
    database: &str,
    redis: &str,
    bots: u32,
    more: &[(&'static str, &str)],
  ) -> Vec<(&'static str, String)> {
    let base = [
      ("MMO_BOTS_AUTHORITY", authority.to_str().expect("a path in UTF-8")),
      ("MMO_BOTS_LOGIN", "localhost:1"),
      ("MMO_BOTS_GATEWAY", "localhost:1"),
      ("DATABASE_URL", database),
      ("REDIS_URL", redis),
      ("MMO_BOTS_ZONE", "1"),
      ("MMO_BOTS_PLAY_SECONDS", "0"),
      ("MMO_BOTS_JOIN_SECONDS", "10"),
      ("MMO_BOTS_RUN", "xyz"),
    ];
    let count = bots.to_string();
    [&base[..], &[("MMO_BOTS_COUNT", count.as_str())], more]
      .concat()
      .into_iter()
      .map(|(name, value)| (name, value.to_owned()))
      .collect()
  }

  async fn run_with(vars: &[(&'static str, String)]) -> Result<(), String> {
    let read = |name: &str| vars.iter().rev().find(|(key, _)| *key == name).map(|(_, v)| v.clone());
    run(&read).await.map_err(|error| error.to_string())
  }

  #[sqlx::test(migrations = false)]
  async fn a_bot_logs_in_through_the_login_service_plays_and_the_report_says_so(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) {
    let url = connect.to_url_lossy();
    let store = Store::migrate(PgPool::connect_with(connect).await.expect("a pool"))
      .await
      .expect("a migrated database");
    store.add_zone(ZoneId(1), "meadow").await.expect("a zone");
    let redis = Redis::start().await;
    let login = login_service("Bxyzaaaa", 1);
    let gateway = Fake::start(move |mut peer| async move {
      assert_eq!(peer.read::<Attach>().await.session.0, [5; 32]);
      send(&peer, &snapshot(10, None, vec![entity(1)]));
      peer
    });
    let (_folder, path) = authority(&[&login, &gateway]);
    let addresses = [
      ("MMO_BOTS_LOGIN", login.address.as_str()),
      ("MMO_BOTS_GATEWAY", gateway.address.as_str()),
      ("MMO_BOTS_PLAY_SECONDS", "1"),
    ];
    let one = vars(&path, url.as_str(), &redis.url(), 1, &addresses);
    let read = |name: &str| one.iter().rev().find(|(key, _)| *key == name).map(|(_, v)| v.clone());
    let report = measure(&Config::read(&read).expect("a config")).await.expect("a report");
    let text = report.to_string();
    assert!(text.starts_with("load test: 1 bots, 1 played, 0 waiting, 0 failed\n"), "{text}");
    gateway.end().await;
    login.end().await;
    let none = vars(&path, url.as_str(), &redis.url(), 0, &[]);
    assert_eq!(run_with(&none).await, Ok(()));
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
  async fn the_load_test_names_each_store_and_file_that_refuses_it(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) {
    let url = connect.to_url_lossy();
    let redis = Redis::start().await;
    let idle = Fake::start(hold);
    let (_folder, path) = authority(&[&idle]);
    let (database, redis_url) = (url.as_str(), redis.url());
    let refused =
      |more: &[(&'static str, &str)], bots| vars(&path, database, &redis_url, bots, more);
    assert_eq!(run_with(&[]).await, Err("the variable MMO_BOTS_AUTHORITY is not set".to_owned()));
    let mut faults = vec![run_with(&refused(&[("MMO_BOTS_AUTHORITY", "nowhere.pem")], 0)).await];
    faults
      .push(run_with(&refused(&[("DATABASE_URL", "postgres://nobody@127.0.0.1:1/x")], 0)).await);
    faults.push(run_with(&refused(&[], 0)).await);
    let store = Store::migrate(PgPool::connect_with(connect).await.expect("a pool"))
      .await
      .expect("a migrated database");
    store.add_zone(ZoneId(1), "meadow").await.expect("a zone");
    store.create_account("botxyz0", Password("a password".to_owned())).await.expect("an account");
    faults.push(run_with(&refused(&[], 1)).await);
    faults.push(run_with(&refused(&[("REDIS_URL", "nope")], 0)).await);
    faults.push(run_with(&refused(&[("REDIS_URL", "redis://127.0.0.1:1")], 0)).await);
    let blind = user(&redis, "blind", &["-hgetall"]).await;
    faults.push(run_with(&refused(&[("REDIS_URL", blind.as_str())], 0)).await);
    let capless = user(&redis, "capless", &["-hget"]).await;
    faults.push(run_with(&refused(&[("REDIS_URL", capless.as_str())], 0)).await);
    let starts = [
      "the certificate cannot be read",
      "PostgreSQL refuses the bots",
      "the store refuses the bots: the database lacks the migration",
      "the store refuses the bots: the name botxyz0 is taken",
      "Redis refuses the bots",
      "Redis refuses the bots",
      "Redis refuses the bots",
      "Redis refuses the bots",
    ];
    for (fault, start) in faults.iter().zip(starts) {
      let text = fault.as_ref().expect_err("a refusal");
      assert!(text.starts_with(start), "{text} does not start with {start}");
    }
  }
}
