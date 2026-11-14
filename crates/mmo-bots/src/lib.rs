//! The load test of mmo-archi: headless clients that log in through the login service, wait in
//! its queue, link to a gateway, and send the inputs of a walk 20 times each second, as the
//! players do. It measures the time of each tick of the layer writer, the bytes of each client, the
//! wait in the queue and the faults, and prints a report against the budget of
//! `docs/architecture.md`.
//!
//! The login service makes only 5 new accounts an hour for one address, so [`run`] first makes the
//! accounts and the characters in PostgreSQL, with `mmo-store` as the login service does, and then
//! each bot logs in through the login service. The tick times come from the hash that the writer
//! of each layer of each zone keeps in Redis, `mmo_store::keys::ticks`: [`run`] reads them before
//! and after the measure and takes the difference. The bots go round the realms: the bot
//! with the index `i` plays in the realm `i % n + 1`, through the login service of that realm. Each
//! second of the measure, [`run`] reads the directory of each realm, `mmo_store::keys::layers`, and
//! the report tells the layers that opened in each zone of each realm and the players on each,
//! and fails when a layer passes its cap or a realm opens fewer layers than its players need. When
//! two realms play, it also counts the bots of another realm that a snapshot gave to a bot, and
//! fails when one did. The bytes are the sizes of the datagrams, with no header of IP, UDP or QUIC,
//! which add about 60 bytes to each datagram.
//!
//! [`run`] reads these variables of its environment:
//!
//! - `MMO_BOTS_AUTHORITY`: the PEM file of the certificates that sign the login service and the
//!   gateway.
//! - `MMO_BOTS_LOGIN` and `MMO_BOTS_GATEWAY`: their addresses, as `host:port`. Each host is the
//!   name that its certificate must hold. `MMO_BOTS_LOGIN` holds the address of the login service
//!   of each realm, with commas between them, so the bots play in as many realms as it names.
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
//!   each of them played, so the crossers stand on the first layer of their realm. Give at most
//!   as many crossers to a realm as a layer holds. The report fails when a crosser did not enter
//!   the next zone, and when one entered a layer with another number than its own. The default
//!   is 0.
//! - `MMO_BOTS_LEAVERS`: how many of the first bots log out once each bot plays, before the
//!   measure, as `150`. The others play to the end, and send no order, since the game has none.
//!   The report then fails when a bot left does not play at the end, and when the last look at the
//!   directory shows an empty layer, or a layer of a quarter of its cap or fewer whose players fit
//!   in the room of the other layers of the zone. So a measure of 150 s proves the drain and the
//!   close of the layers that the leavers emptied. The default is 0.
//! - `MMO_BOTS_REALMS`: the count of the bots of each realm, in the order of the realms, with
//!   commas between them, as `150,85,50`. The first bots fill the first realm, the next ones the
//!   second, and so on. With no value, the bots go round the realms.
//! - `MMO_BOTS_RUSH`: how many of the last bots of the first realm log in once each other bot
//!   plays, at the pace of the ramp, as `50`. The bot of the first realm before them asks first
//!   for a new character, which fills the realm, so the realm must refuse it, and then plays its
//!   own. The rush waits in the queue of the full realm, and the leavers then free their slots.
//!   Each second of the measure, the run also asks the last login service for the list of the
//!   realms, and the report fails when the level of a realm is not the level of its admitted
//!   players and its cap, when the full realm made the new character, or when the layers of a
//!   realm do not hold its bots that play. The default is 0.
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
mod levels;
mod report;
mod rng;
mod seed;
mod ticks;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mmo_client::Trust;
use mmo_protocol::RealmId;
use mmo_store::Store;
use redis::aio::MultiplexedConnection;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

use bot::{Outcome, Shared};
use config::Config;
use layers::Seen;
use levels::Levels;
use report::Report;
use seed::{Bot, SEEDERS};

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

/// Waits until `count` reaches `bots`, or until `limit` passes, and says its progress: the bots
/// that play or failed, or the bots of the rush that got a place in the queue.
async fn settle(count: &AtomicUsize, bots: usize, what: &str, limit: Duration) {
  let end = Instant::now() + limit;
  let mut waits = 0_u32;
  loop {
    let ready = count.load(Ordering::Relaxed);
    if waits.is_multiple_of(50) {
      eprintln!("{ready} of {bots} bots {what}");
    }
    if ready >= bots || Instant::now() >= end {
      return;
    }
    waits += 1;
    sleep(Duration::from_millis(100)).await;
  }
}

/// Looks at the directory of the layers each second until the measure ends, and once at its end,
/// and in a run with a rush at the levels of the list of the realms, which any login service
/// gives: it asks the last one, whose realm the rush does not fill.
async fn look_during(
  redis: &mut MultiplexedConnection,
  config: &Config,
  trust: &Trust,
  seen: &mut Seen,
  levels: &mut Levels,
) -> Result<(), Failure> {
  let end = Instant::now() + config.play;
  loop {
    let looked = layers::look(redis, config.realms(), seen).await;
    let listed = if config.rush > 0 {
      let login = config.login.rsplit(',').next().unwrap_or_default();
      levels::look(trust, login, redis, config.realms(), levels).await
    } else {
      Ok(())
    };
    looked.map_err(Failure::from).and(listed)?;
    let left = end.saturating_duration_since(Instant::now());
    if left.is_zero() {
      return Ok(());
    }
    sleep(left.min(Duration::from_secs(1))).await;
  }
}

/// What the `bots` of a run share, with the signal `leave` for the leavers.
fn shared(config: &Config, trust: Trust, bots: &[Bot], leave: watch::Receiver<bool>) -> Shared {
  Shared {
    trust,
    gateway: config.gateway.clone(),
    zone: config.zone,
    seed: config.seed,
    ready: AtomicUsize::new(0),
    reconnect: !config.recover.is_zero(),
    crossers: usize::try_from(config.crossers).unwrap_or(usize::MAX),
    realms: bots.iter().map(|bot| (bot.character.0, bot.realm)).collect(),
    leavers: config.leavers,
    leave,
    ramp: config.ramp,
    rushing: bots.len().saturating_sub(config.rush as usize),
    rushed: AtomicUsize::new(0),
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
  let (leaving, leave) = watch::channel(false);
  let prober = bots.iter().find(|bot| bot.probe.is_some()).map(|bot| bot.id);
  let rush = config.rush as usize;
  let shared = Arc::new(shared(config, trust, &bots, leave));
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
  let playing = tasks.len().saturating_sub((config.waiting as usize).max(rush));
  settle(&shared.ready, playing, "play or failed", config.join).await;
  settle(&shared.rushed, rush, "of the rush wait in the queue", config.join).await;
  leaving.send_replace(true);
  let before = ticks::read(&mut redis, config.realms()).await;
  let (mut opened, mut levels) = (Seen::new(), Levels::default());
  let looked = look_during(&mut redis, config, &shared.trust, &mut opened, &mut levels).await;
  let after = ticks::read(&mut redis, config.realms()).await;
  stop.send_replace(true);
  let mut outcomes: Vec<Outcome> = Vec::new();
  while let Some(done) = tasks.join_next().await {
    // A bot that panicked gives no outcome, and the report counts it as failed.
    outcomes.extend(done);
  }
  let counts = before.and_then(|before| after.map(|after| ticks::since(&before, &after)))?;
  looked?;
  let report = Report::new(
    usize::try_from(config.bots).unwrap_or(usize::MAX),
    usize::try_from(config.waiting).unwrap_or(usize::MAX),
    config.recover,
    &outcomes,
    &counts,
    opened,
    config.play,
  );
  match prober {
    Some(id) => store
      .characters(id, RealmId(1))
      .await
      .map(|listed| report.with_census(&outcomes, levels, listed.len()))
      .map_err(Failure::from),
    None => Ok(report),
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use mmo_client::testing::{Fake, hold, script};
  use mmo_protocol::{
    Attach, Character, CharacterId, FromLogin, Password, Population, Realm, RealmId, Refusal,
    Rules, ToLogin, Token, ZoneId,
  };
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

  #[sqlx::test(migrations = false)]
  async fn a_run_with_a_rush_looks_at_the_levels_and_counts_the_characters_of_the_probe(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) {
    let url = connect.to_url_lossy();
    let store = Store::migrate(PgPool::connect_with(connect).await.expect("a pool"))
      .await
      .expect("a migrated database");
    store.add_zone(ZoneId(1), "meadow").await.expect("a zone");
    let redis = Redis::start().await;
    let brom = || Character { id: CharacterId(1), name: "Bxyzaaaa".to_owned() };
    let place = || FromLogin::Queued { position: 0, length: 1, wait: 1, resume: Token([1; 32]) };
    let first = Fake::start_each(vec![
      script(move |mut peer| async move {
        peer.read::<ToLogin>().await;
        peer.write(&place()).await;
        peer.write(&FromLogin::Characters(vec![brom()])).await;
        let asked = ToLogin::NewCharacter { name: "Nxyzaaaa".to_owned() };
        assert_eq!(peer.read::<ToLogin>().await, asked);
        peer.write(&FromLogin::Refused(Refusal::Full)).await;
        peer
      }),
      script(move |mut peer| async move {
        peer.read::<ToLogin>().await;
        peer.write(&FromLogin::Characters(vec![brom()])).await;
        assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(1) });
        peer.write(&FromLogin::Session { token: Token([5; 32]) }).await;
        peer
      }),
      script(move |mut peer| async move {
        peer.read::<ToLogin>().await;
        peer.write(&place()).await;
        peer
      }),
    ]);
    let full = |id| Realm {
      id: RealmId(id),
      name: format!("Realm{id}"),
      region: "Europe".to_owned(),
      language: "French".to_owned(),
      rules: Rules::Normal,
      population: Population::Full,
      queue: 0,
      login: "localhost:1".to_owned(),
    };
    let second = Fake::start(move |mut peer| async move {
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Realms);
      peer.write(&FromLogin::Realms(vec![full(1), full(2)])).await;
      peer
    });
    let gateway = Fake::start(move |mut peer| async move {
      peer.read::<Attach>().await;
      // The seeders make the two characters at once, so either one can hold the id 1.
      send(&peer, &snapshot(10, None, vec![entity(1), entity(2)]));
      peer
    });
    let (_folder, path) = authority(&[&first, &second, &gateway]);
    let logins = format!("{},{}", first.address, second.address);
    let more = [
      ("MMO_BOTS_LOGIN", logins.as_str()),
      ("MMO_BOTS_GATEWAY", gateway.address.as_str()),
      ("MMO_BOTS_REALMS", "2,0"),
      ("MMO_BOTS_RUSH", "1"),
      ("MMO_BOTS_WAITING", "1"),
    ];
    let two = vars(&path, url.as_str(), &redis.url(), 2, &more);
    let read = |name: &str| two.iter().rev().find(|(key, _)| *key == name).map(|(_, v)| v.clone());
    let report = measure(&Config::read(&read).expect("a config")).await.expect("a report");
    let text = report.to_string();
    assert!(text.starts_with("load test: 2 bots, 1 played, 1 waiting, 0 failed\n"), "{text}");
    let census = "realms: 1 looks at the list, 0 levels apart from the admitted players, the last levels 1 Full, 2 Full, the bot before the rush got the refusal Full and holds 1 characters in the first realm, the players of each realm  and its bots that play 1 1\n";
    assert!(text.contains(census), "{text}");
    gateway.end().await;
    second.end().await;
    first.end().await;
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
