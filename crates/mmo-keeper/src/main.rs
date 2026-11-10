//! The keeper of mmo-archi: the one process that writes the characters, the items and the epoch
//! of each zone in PostgreSQL.
//!
//! When it starts, it runs the migrations of `mmo-store` under the lock of sqlx. Then it takes the
//! links of the zone writers on QUIC, and answers each [`ToKeeper`](mmo_protocol::ToKeeper) with
//! the queries of `mmo-store`, through 16 connections to PostgreSQL at most. It reads these
//! variables of its environment:
//!
//! - `KEEPER_ADDRESS`: the address and the UDP port of the links, as `0.0.0.0:4433`.
//! - `DATABASE_URL`: the database of the game, as `postgres://keeper@db/game`.
//! - `KEEPER_CERTIFICATE` and `KEEPER_KEY`: the PEM files of the certificate chain of the keeper
//!   and of its private key.
//! - `KEEPER_AUTHORITY`: the PEM file of the certificates that sign the certificate of each zone
//!   writer. The keeper refuses each peer whose certificate they do not sign, so only a zone
//!   writer of the operator writes the game data.
//!
//! It runs until a fault stops it. Then it writes the fault on its standard error and exits with
//! the code 1, and the process manager starts it again.

mod config;
mod link;
#[cfg(test)]
mod peer;

use std::process::ExitCode;
use std::{env, io};

use mmo_store::Store;
use quinn::Endpoint;
use sqlx::postgres::PgPoolOptions;

use crate::config::Config;

/// The most connections of the keeper to PostgreSQL.
const CONNECTIONS: u32 = 16;

/// Why the keeper stops.
#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Config(#[from] config::Error),
  #[error("the database refuses the keeper: {0}")]
  Store(#[from] mmo_store::Error),
  #[error("the keeper cannot take links on its address: {0}")]
  Address(#[from] io::Error),
  #[error("the endpoint of the links closed")]
  Closed,
}

#[expect(clippy::disallowed_methods, reason = "main starts the runtime of the keeper once")]
#[tokio::main]
async fn main() -> ExitCode {
  eprintln!("the keeper stops: {}", keep(|name| env::var(name).ok()).await);
  ExitCode::FAILURE
}

/// Runs the keeper until a fault stops it, and gives that fault.
async fn keep(var: impl Fn(&str) -> Option<String>) -> Error {
  match open(var).await {
    Ok((endpoint, store)) => link::serve(endpoint, store).await,
    Err(error) => error,
  }
}

async fn open(var: impl Fn(&str) -> Option<String>) -> Result<(Endpoint, Store), Error> {
  let config = Config::read(var)?;
  let pool = PgPoolOptions::new().max_connections(CONNECTIONS).connect_lazy_with(config.database);
  let store = Store::migrate(pool).await?;
  Ok((Endpoint::server(config.server, config.address)?, store))
}

#[cfg(test)]
mod tests {
  use std::io;
  use std::net::UdpSocket;

  use mmo_protocol::{
    Angle, Batch, Epoch, FromKeeper, KeeperRefusal, Password, Position, Save, State, ToKeeper,
    ZoneId,
  };
  use mmo_store::{Saved, Store};
  use sqlx::ConnectOptions;
  use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

  use super::{Error, keep};
  use crate::peer::{World, Writer, var};

  const START: State =
    State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };
  const MOVED: State =
    State { position: Position { x: 102_400, y: 250, z: -3 }, yaw: Angle(16384), hit_points: 7 };

  #[sqlx::test(migrations = false)]
  async fn a_writer_claims_its_zone_and_the_keeper_writes_each_newer_save(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) -> Result<(), mmo_store::Error> {
    let world = World::new();
    let address =
      UdpSocket::bind("127.0.0.1:0").and_then(|free| free.local_addr()).expect("a port");
    let vars = world.vars(&address.to_string(), connect.to_url_lossy().as_str());
    let keeper = tokio::spawn(async move { keep(var(&vars)).await });
    let writers = world.writer(true);
    let mut writer = Writer::link(&writers, address).await;
    writer.greet().await;
    let store = Store::open(PgPool::connect_with(connect).await?).await?;
    store.add_zone(ZoneId(1), "meadow").await?;
    let account = store.create_account("alice", Password("one".to_owned())).await?;
    let brom = store.create_character(account, "Brom", ZoneId(1), START).await?;
    let save = |epoch| {
      let saves = vec![Save { character: brom, state: MOVED, version: 1 }];
      ToKeeper::Save(Batch { zone: ZoneId(1), epoch: Epoch(epoch), saves })
    };
    let claim = |zone| ToKeeper::Claim { zone: ZoneId(zone) };
    let mut answers = Vec::new();
    for message in [claim(1), save(1), save(1), claim(1), save(1), claim(9)] {
      answers.push(writer.ask(&message).await);
    }
    assert_eq!(
      answers,
      [
        FromKeeper::Claimed { epoch: Epoch(1) },
        FromKeeper::Saved { written: 1 },
        FromKeeper::Saved { written: 0 },
        FromKeeper::Claimed { epoch: Epoch(2) },
        FromKeeper::Refused(KeeperRefusal::Stale { held: Epoch(2) }),
        FromKeeper::Refused(KeeperRefusal::NoZone),
      ]
    );
    writer.send.finish().expect("an open stream");
    assert_eq!(writer.closed().await, Ok((0, String::new())));
    assert_eq!(
      store.load(&[brom]).await?,
      [Saved { character: brom, zone: ZoneId(1), state: MOVED, version: 1 }]
    );
    keeper.abort();
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn the_keeper_stops_on_a_database_that_refuses_it_and_on_an_address_in_use(
    _: PgPoolOptions,
    connect: PgConnectOptions,
  ) {
    let world = World::new();
    let taken = UdpSocket::bind("127.0.0.1:0").expect("a socket");
    let address = taken.local_addr().expect("an address").to_string();
    let absent = connect.clone().database("absent").to_url_lossy();
    let refused = keep(var(&world.vars(&address, absent.as_str()))).await.to_string();
    assert_eq!(
      refused,
      "the database refuses the keeper: while executing migrations: error returned from \
       database: database \"absent\" does not exist"
    );
    let in_use = keep(var(&world.vars(&address, connect.to_url_lossy().as_str()))).await;
    assert!(
      matches!(in_use, Error::Address(ref error) if error.kind() == io::ErrorKind::AddrInUse),
      "{in_use}"
    );
  }
}
