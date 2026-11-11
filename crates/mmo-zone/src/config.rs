use std::net::{AddrParseError, SocketAddr};
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;

use mmo_protocol::ZoneId;
use quinn::ServerConfig;
use sqlx::postgres::PgConnectOptions;

/// What the zone writer reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The zone that the writer runs.
  pub(crate) zone: ZoneId,
  /// The address of the links of the gateways.
  pub(crate) address: SocketAddr,
  /// The address that the gateways dial, which the directory of zones holds.
  pub(crate) public: String,
  /// The database of the game.
  pub(crate) database: PgConnectOptions,
  /// The Redis of the game, as a URL.
  pub(crate) redis: String,
  /// The QUIC of the links, which takes only a peer whose certificate the authority signed.
  pub(crate) server: ServerConfig,
}

/// A variable of the environment that the zone writer cannot use.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error("the variable {0} is not set")]
  Unset(&'static str),
  #[error("ZONE_ID is not the id of a zone: {0}")]
  Zone(#[from] ParseIntError),
  #[error("ZONE_ADDRESS is not an address and a port: {0}")]
  Address(#[from] AddrParseError),
  #[error("DATABASE_URL is not a URL of PostgreSQL: {0}")]
  Database(#[from] sqlx::Error),
  #[error(transparent)]
  Link(#[from] mmo_link::Error),
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let zone = ZoneId(value(&var, "ZONE_ID")?.parse()?);
    let address = value(&var, "ZONE_ADDRESS")?.parse()?;
    let public = value(&var, "ZONE_PUBLIC")?;
    let database = PgConnectOptions::from_str(&value(&var, "DATABASE_URL")?)?;
    let redis = value(&var, "REDIS_URL")?;
    let file = |name| value(&var, name).map(PathBuf::from);
    let server =
      mmo_link::server(&file("ZONE_CERTIFICATE")?, &file("ZONE_KEY")?, &file("ZONE_AUTHORITY")?)?;
    Ok(Self { zone, address, public, database, redis, server })
  }
}

fn value(var: &impl Fn(&str) -> Option<String>, name: &'static str) -> Result<String, Error> {
  var(name).ok_or(Error::Unset(name))
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::Config;
  use crate::peer::{World, var};

  #[test]
  fn the_zone_writer_reads_each_variable_and_names_the_one_that_it_cannot_use() {
    let world = World::new();
    let vars = world.vars("127.0.0.1:4433", "postgres://zone@localhost/game", "redis://redis");
    let read = |name: &'static str, value: Option<String>| {
      let mut vars = vars.clone();
      vars.retain(|(key, _)| *key != name);
      vars.extend(value.map(|value| (name, value)));
      let config = Config::read(var(vars));
      config
        .map(|config| (config.zone.0, config.address.to_string(), config.public, config.redis))
        .map_err(|e| e.to_string())
    };
    let found = [
      read("", None),
      read("ZONE_ID", None),
      read("ZONE_ADDRESS", None),
      read("ZONE_PUBLIC", None),
      read("DATABASE_URL", None),
      read("REDIS_URL", None),
      read("ZONE_CERTIFICATE", None),
      read("ZONE_KEY", None),
      read("ZONE_AUTHORITY", None),
      read("ZONE_ID", Some("meadow".to_owned())),
      read("ZONE_ADDRESS", Some("zone".to_owned())),
      read("DATABASE_URL", Some("zone".to_owned())),
      read("ZONE_CERTIFICATE", Some(world.path("absent.pem"))),
    ];
    let unset = |name: &str| Err(format!("the variable {name} is not set"));
    let relative = "error with configuration: relative URL without a base";
    let absent = fs::File::open(world.path("absent.pem")).expect_err("no file absent.pem");
    assert_eq!(
      found,
      [
        Ok((1, "127.0.0.1:4433".to_owned(), "zone-1:4433".to_owned(), "redis://redis".to_owned())),
        unset("ZONE_ID"),
        unset("ZONE_ADDRESS"),
        unset("ZONE_PUBLIC"),
        unset("DATABASE_URL"),
        unset("REDIS_URL"),
        unset("ZONE_CERTIFICATE"),
        unset("ZONE_KEY"),
        unset("ZONE_AUTHORITY"),
        Err("ZONE_ID is not the id of a zone: invalid digit found in string".to_owned()),
        Err("ZONE_ADDRESS is not an address and a port: invalid socket address syntax".to_owned()),
        Err(format!("DATABASE_URL is not a URL of PostgreSQL: {relative}")),
        Err(format!(
          "{} holds no PEM that can be read: I/O error: {absent}",
          world.path("absent.pem")
        )),
      ]
    );
  }
}
