//! The zone writer of mmo-archi: the one authority of the state of one zone.
//!
//! It runs the zone on a fixed tick of 50 ms. It takes the links of the gateways on QUIC: each
//! [`ToZone::Join`](mmo_protocol::ToZone) loads the last save of a character from PostgreSQL with
//! `mmo-store`, and the zone takes the player when the save is in this zone and the zone holds
//! fewer than 2500 players. At each tick it takes at most one input of each player, moves each
//! body with the step of `mmo-collide`, and sends to each player a
//! [`Snapshot`](mmo_protocol::Snapshot) of the 64 nearest entities in the 9 cells of 32 m around
//! it, as a delta against the last snapshot that its client acknowledged.
//!
//! The writer keeps its state as sections 2, 5 and 9 of `docs/architecture.md` say. When it
//! starts it claims its zone with [`Store::claim`](mmo_store::Store::claim), which gives it the
//! next epoch, and it writes its entry in the directory of zones in Redis. It stops when a newer
//! epoch holds the zone, in the directory or in a refusal of the store. Each second it renews its
//! entry. Each 10 s, and when a player logs out, it saves the players whose state changed, each
//! row with its version, and it keeps the presence of each player in Redis. It reads these
//! variables of its environment:
//!
//! - `ZONE_ID`: the id of the zone in the table `zones`, as `1`.
//! - `ZONE_ADDRESS`: the address and the UDP port of the links, as `0.0.0.0:4434`.
//! - `ZONE_PUBLIC`: the address that the gateways dial, as `zone-1.internal:4434`.
//! - `DATABASE_URL`: the database of the game, as `postgres://zone@db/game`.
//! - `REDIS_URL`: the Redis of the game, as `redis://redis:6379`.
//! - `ZONE_CERTIFICATE` and `ZONE_KEY`: the PEM files of the certificate chain of the zone writer
//!   and of its private key.
//! - `ZONE_AUTHORITY`: the PEM file of the certificates that sign the certificate of each
//!   gateway. The zone writer refuses each peer whose certificate they do not sign, so only a
//!   gateway of the operator relays the intents of a player.
//!
//! It runs until a fault stops it. Then it writes the fault on its standard error and exits with
//! the code 1, and the process manager starts it again.

mod claim;
mod config;
mod interest;
mod link;
#[cfg(test)]
mod peer;
mod view;
mod zone;

use std::process::ExitCode;
use std::{env, io};

use mmo_store::Store;
use quinn::Endpoint;
use sqlx::postgres::PgPoolOptions;

use crate::claim::Claim;
use crate::config::Config;
use crate::zone::Zone;

/// The most connections of the zone writer to PostgreSQL. It reads a character at each join.
const CONNECTIONS: u32 = 4;

/// Why the zone writer stops.
#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Config(#[from] config::Error),
  #[error("the database refuses the zone writer: {0}")]
  Store(#[from] mmo_store::Error),
  #[error(transparent)]
  Claim(#[from] claim::Error),
  #[error("the zone writer cannot take links on its address: {0}")]
  Address(#[from] io::Error),
  #[error("the endpoint of the links closed")]
  Closed,
}

#[expect(clippy::disallowed_methods, reason = "main starts the runtime of the zone writer once")]
#[tokio::main]
async fn main() -> ExitCode {
  eprintln!("the zone writer stops: {}", write(|name| env::var(name).ok()).await);
  ExitCode::FAILURE
}

/// Runs the zone writer until a fault stops it, and gives that fault.
async fn write(var: impl Fn(&str) -> Option<String>) -> Error {
  match open(var).await {
    Ok((endpoint, zone, store, claim)) => link::serve(endpoint, zone, store, claim).await,
    Err(error) => error,
  }
}

/// Opens each part of the zone writer. It binds its address before it claims its zone, so a writer
/// that cannot take links takes no epoch.
async fn open(
  var: impl Fn(&str) -> Option<String>,
) -> Result<(Endpoint, Zone, Store, Claim), Error> {
  let config = Config::read(var)?;
  let pool = PgPoolOptions::new().max_connections(CONNECTIONS).connect_lazy_with(config.database);
  let store = Store::open(pool).await?;
  let endpoint = Endpoint::server(config.server, config.address)?;
  let epoch = store.claim(config.zone).await?;
  let claim = Claim::open(&config.redis, config.zone, epoch, config.public).await?;
  Ok((endpoint, Zone::new(config.zone), store, claim))
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::io;
  use std::net::UdpSocket;
  use std::time::Duration;

  use mmo_protocol::{
    Angle, CharacterId, Entity, FromClient, Input, Intents, Link, Password, Position, Relayed,
    Snapshot, Tick, ToZone, Velocity, ZoneId,
  };
  use mmo_store::testing::Redis;
  use mmo_store::{State, Store};
  use sqlx::ConnectOptions;
  use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
  use tokio::time;

  use super::{Error, write};
  use crate::peer::{Peer, World, hash, var};

  /// The bound of a walk of 26 ticks, which takes 1.3 s.
  const WALK: Duration = Duration::from_secs(20);

  type View = BTreeMap<i64, Entity>;

  /// What a client holds after `snapshot`, from what it held after each snapshot before it, when
  /// no entity left its view.
  fn apply(views: &mut BTreeMap<u32, View>, snapshot: &Snapshot) -> View {
    assert_eq!(snapshot.left, [], "no entity leaves the view of a player of this test");
    let mut view = snapshot.base.map(|base| views[&base.0].clone()).unwrap_or_default();
    for entity in &snapshot.entered {
      view.insert(entity.id.0, *entity);
    }
    for change in &snapshot.changed {
      let held = view.get_mut(&change.id.0).expect("a changed entity is held");
      held.position.x += change.movement.x;
      held.position.y += change.movement.y;
      held.position.z += change.movement.z;
      held.velocity = change.velocity;
      held.view = change.view;
      held.hit_points = change.hit_points;
    }
    views.insert(snapshot.tick.0, view.clone());
    view
  }

  fn free() -> String {
    let free = UdpSocket::bind("127.0.0.1:0").and_then(|free| free.local_addr());
    free.expect("a port").to_string()
  }

  fn at(id: CharacterId, x: i32, speed: i16) -> Entity {
    Entity {
      id,
      position: Position { x, y: 0, z: 50_000 },
      velocity: Velocity { x: speed, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    }
  }

  /// Brom walks east into the block whose face is at x = 498 m, and Cael stands near him.
  #[sqlx::test(migrations = false)]
  async fn the_players_of_a_gateway_walk_in_the_zone_and_see_each_other(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) -> Result<(), mmo_store::Error> {
    let pool = PgPool::connect_with(connect.clone()).await?;
    let store = Store::migrate(pool.clone()).await?;
    store.add_zone(ZoneId(1), "meadow").await?;
    store.add_zone(ZoneId(2), "forest").await?;
    let account = store.create_account("alice", Password("one".to_owned())).await?;
    let state =
      |x| State { position: Position { x, y: 0, z: 50_000 }, yaw: Angle(0), hit_points: 100 };
    let brom = store.create_character(account, "Brom", ZoneId(1), state(49_000)).await?;
    let cael = store.create_character(account, "Cael", ZoneId(1), state(48_000)).await?;
    let dara = store.create_character(account, "Dara", ZoneId(2), state(48_000)).await?;
    let world = World::new();
    let address = free();
    let redis = Redis::start().await;
    let vars = world.vars(&address, connect.to_url_lossy().as_str(), &redis.url());
    let zone = tokio::spawn(write(var(vars)));
    let mut peer = Peer::link(&world.gateway(true), address.parse().expect("an address")).await;
    peer.greet().await;
    let joins = [(1, brom), (2, cael), (3, dara), (4, CharacterId(i64::MAX))];
    for (link, character) in joins {
      peer.tell(&ToZone::Join { link: Link(link), account, character }).await;
    }

    let stopped = at(brom, 49_757, 0);
    let mut views = BTreeMap::new();
    let walk = time::timeout(WALK, async {
      loop {
        let snapshot = peer.snapshot(Link(2)).await;
        let view = apply(&mut views, &snapshot);
        let inputs = |heading| {
          [1, 2, 3].map(|ahead| Input {
            tick: Tick(snapshot.tick.0 + ahead),
            heading,
            view: Angle(0),
          })
        };
        let intents = |ack, heading| Intents { zone: ZoneId(1), ack, inputs: inputs(heading) };
        peer.intents(&Relayed { link: Link(1), message: intents(None, Some(Angle(0))) });
        peer.intents(&Relayed { link: Link(2), message: intents(Some(snapshot.tick), None) });
        if view.get(&brom.0) == Some(&stopped) {
          return view;
        }
      }
    });
    let view = walk.await.expect("Brom reaches the block");
    assert_eq!(view, BTreeMap::from([(brom.0, stopped), (cael.0, at(cael, 48_000, 0))]));

    let entry = hash(&redis, "zone:1").await;
    assert_eq!((&entry["epoch"], &entry["address"]), (&"1".to_owned(), &"zone-1:4433".to_owned()));
    assert_eq!(hash(&redis, &format!("presence:{}", account.0)).await["zone"], "1");

    let say = FromClient::Say { channel: "world".to_owned(), text: "hi".to_owned() };
    peer.tell(&ToZone::Client(Relayed { link: Link(2), message: say })).await;
    let mut lock = pool.begin().await?;
    sqlx::query("SELECT epoch FROM zones WHERE id = 1 FOR UPDATE").execute(&mut *lock).await?;
    peer.tell(&ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout })).await;
    peer.tell(&ToZone::Leave { link: Link(2) }).await;
    peer.tell(&ToZone::Join { link: Link(5), account, character: cael }).await;
    let back = time::timeout(WALK, peer.snapshot(Link(5))).await.expect("Cael comes back");
    assert_eq!(back.entered, [at(cael, 48_000, 0)]);
    let mut kept = store.load(&[brom]).await?[0];
    assert_eq!(kept.version, 0, "the save of Brom waits for the lock on its zone");
    lock.commit().await?;
    let saved = time::timeout(WALK, async {
      while kept.version == 0 {
        time::sleep(Duration::from_millis(50)).await;
        kept = store.load(&[brom]).await.expect("a save")[0];
      }
      kept
    });
    let kept = saved.await.expect("the logout of Brom is saved");
    assert_eq!((kept.version, kept.state.position), (1, stopped.position));
    zone.abort();
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn the_zone_writer_stops_on_a_database_that_the_keeper_did_not_migrate_and_on_an_address_in_use(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) -> Result<(), mmo_store::Error> {
    let world = World::new();
    let taken = UdpSocket::bind("127.0.0.1:0").expect("a socket");
    let address = taken.local_addr().expect("an address").to_string();
    let database = connect.to_url_lossy();
    let behind = write(var(world.vars(&address, database.as_str(), "nope"))).await.to_string();
    assert_eq!(
      behind,
      "the database refuses the zone writer: the database lacks the migration 1, which the \
       keeper runs"
    );
    let store = Store::migrate(PgPool::connect_with(connect).await?).await?;
    let in_use = write(var(world.vars(&address, database.as_str(), "nope"))).await;
    assert!(
      matches!(in_use, Error::Address(ref error) if error.kind() == io::ErrorKind::AddrInUse),
      "{in_use}"
    );
    let mut faults = Vec::new();
    for _ in 0..2 {
      let vars = world.vars(&free(), database.as_str(), "nope");
      faults.push(write(var(vars)).await.to_string());
      store.add_zone(ZoneId(1), "meadow").await.ok();
    }
    assert_eq!(
      faults,
      [
        "the database refuses the zone writer: no zone has the id 1",
        "Redis refuses the zone writer: Redis URL did not parse - InvalidClientConfig",
      ]
    );
    let unset = write(var(Vec::new())).await.to_string();
    assert_eq!(unset, "the variable ZONE_ID is not set");
    Ok(())
  }
}
