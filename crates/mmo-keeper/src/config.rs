use std::net::{AddrParseError, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use quinn::ServerConfig;
use sqlx::postgres::PgConnectOptions;

/// What the keeper reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The address of the links of the layer writers.
  pub(crate) address: SocketAddr,
  /// The database of the game.
  pub(crate) database: PgConnectOptions,
  /// The QUIC of the links, which takes only a peer whose certificate the authority signed.
  pub(crate) server: ServerConfig,
}

/// A variable of the environment that the keeper cannot use.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error("the variable {0} is not set")]
  Unset(&'static str),
  #[error("KEEPER_ADDRESS is not an address and a port: {0}")]
  Address(#[from] AddrParseError),
  #[error("DATABASE_URL is not a URL of PostgreSQL: {0}")]
  Database(#[from] sqlx::Error),
  #[error(transparent)]
  Link(#[from] mmo_link::Error),
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let address = value(&var, "KEEPER_ADDRESS")?.parse()?;
    let database = PgConnectOptions::from_str(&value(&var, "DATABASE_URL")?)?;
    let file = |name| value(&var, name).map(PathBuf::from);
    let files = [file("KEEPER_CERTIFICATE")?, file("KEEPER_KEY")?, file("KEEPER_AUTHORITY")?];
    let [certificate, key, authority] = &files;
    Ok(Self { address, database, server: mmo_link::server(certificate, key, authority)? })
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

  /// The address that the keeper reads from the variables of `world`, with `name` set to `value`,
  /// or unset when `value` is `None`, or why the keeper cannot use them.
  fn read(world: &World, name: &'static str, value: Option<String>) -> Result<String, String> {
    let mut vars = world.vars("127.0.0.1:4433", "postgres://keeper@localhost/game");
    vars.retain(|(key, _)| *key != name);
    vars.extend(value.map(|value| (name, value)));
    Config::read(var(&vars)).map(|config| config.address.to_string()).map_err(|e| e.to_string())
  }

  #[test]
  fn the_keeper_names_each_variable_that_it_lacks_or_cannot_parse() {
    let world = World::new();
    let found = [
      read(&world, "", None),
      read(&world, "KEEPER_ADDRESS", None),
      read(&world, "DATABASE_URL", None),
      read(&world, "KEEPER_KEY", None),
      read(&world, "KEEPER_CERTIFICATE", None),
      read(&world, "KEEPER_AUTHORITY", None),
      read(&world, "KEEPER_ADDRESS", Some("keeper".to_owned())),
      read(&world, "DATABASE_URL", Some("keeper".to_owned())),
    ];
    let unset = |name: &str| Err(format!("the variable {name} is not set"));
    let relative = "error with configuration: relative URL without a base";
    assert_eq!(
      found,
      [
        Ok("127.0.0.1:4433".to_owned()),
        unset("KEEPER_ADDRESS"),
        unset("DATABASE_URL"),
        unset("KEEPER_KEY"),
        unset("KEEPER_CERTIFICATE"),
        unset("KEEPER_AUTHORITY"),
        Err(
          "KEEPER_ADDRESS is not an address and a port: invalid socket address syntax".to_owned()
        ),
        Err(format!("DATABASE_URL is not a URL of PostgreSQL: {relative}")),
      ]
    );
  }

  #[test]
  fn the_keeper_names_each_file_of_pem_that_makes_no_server_of_tls() {
    let world = World::new();
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n";
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    for (name, text) in [("broken.pem", broken), ("garbage.pem", garbage), ("empty.pem", "")] {
      fs::write(world.path(name), text).expect("a file");
    }
    let file = |name: &'static str, path: &str| read(&world, name, Some(world.path(path)));
    let found = [
      file("KEEPER_CERTIFICATE", "broken.pem"),
      file("KEEPER_CERTIFICATE", "absent.pem"),
      file("KEEPER_KEY", "keeper.pem"),
      file("KEEPER_AUTHORITY", "broken.pem"),
      file("KEEPER_AUTHORITY", "garbage.pem"),
      file("KEEPER_AUTHORITY", "empty.pem"),
      file("KEEPER_KEY", "writer.key"),
    ];
    let section = "missing section end marker: [67, 69, 82, 84, 73, 70, 73, 67, 65, 84, 69]";
    let absent = fs::File::open(world.path("absent.pem")).expect_err("no file absent.pem");
    let anchors = "no root trust anchors were provided";
    let tls = "the certificates and the key make no server of TLS: ";
    let pem = |name: &str, why: &str| {
      Err(format!("{} holds no PEM that can be read: {why}", world.path(name)))
    };
    assert_eq!(
      found,
      [
        pem("broken.pem", section),
        pem("absent.pem", &format!("I/O error: {absent}")),
        pem("keeper.pem", "no items found"),
        pem("broken.pem", section),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err(format!("the authority holds no certificate that signs a peer: {anchors}")),
        Err(format!("{tls}keys may not be consistent: KeyMismatch")),
      ]
    );
  }
}
