use std::net::{AddrParseError, SocketAddr};
use std::path::PathBuf;

use quinn::{ClientConfig, ServerConfig};
use redis::{IntoConnectionInfo, ProtocolVersion, RedisError};

/// What the gateway reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The address of the clients.
  pub(crate) address: SocketAddr,
  /// The QUIC of the clients.
  pub(crate) clients: ServerConfig,
  /// The QUIC of the links to the layer writers, which shows the certificate of the gateway and
  /// takes only a writer whose certificate the authority signed.
  pub(crate) writers: ClientConfig,
  /// The Redis of the world, in RESP3, so the one connection of the gateway also hears the
  /// channel of the directory.
  pub(crate) redis: redis::Client,
}

/// A variable of the environment that the gateway cannot use.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error("the variable {0} is not set")]
  Unset(&'static str),
  #[error("GATEWAY_ADDRESS is not an address and a port: {0}")]
  Address(#[from] AddrParseError),
  #[error("REDIS_URL is not a URL of Redis: {0}")]
  Redis(#[from] RedisError),
  #[error(transparent)]
  Link(#[from] mmo_link::Error),
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  #[expect(clippy::expect_used, reason = "a client of a parsed ConnectionInfo checks nothing more")]
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let address = value(&var, "GATEWAY_ADDRESS")?.parse()?;
    let redis = value(&var, "REDIS_URL")?.as_str().into_connection_info()?;
    let resp3 = redis.redis_settings().clone().set_protocol(ProtocolVersion::RESP3);
    let file = |name| value(&var, name).map(PathBuf::from);
    let files = [
      file("GATEWAY_CERTIFICATE")?,
      file("GATEWAY_KEY")?,
      file("GATEWAY_LINK_CERTIFICATE")?,
      file("GATEWAY_LINK_KEY")?,
      file("GATEWAY_AUTHORITY")?,
    ];
    let [certificate, key, link, link_key, authority] = &files;
    Ok(Self {
      address,
      clients: mmo_link::public(certificate, key)?,
      writers: mmo_link::client(link, link_key, authority)?,
      redis: redis::Client::open(redis.set_redis_settings(resp3)).expect("a client of Redis"),
    })
  }
}

fn value(var: &impl Fn(&str) -> Option<String>, name: &'static str) -> Result<String, Error> {
  var(name).ok_or(Error::Unset(name))
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::Config;
  use crate::world::{World, var};

  /// The address that the gateway reads from the variables of `world`, with `name` set to `value`,
  /// or unset when `value` is `None`, or why the gateway cannot use them.
  fn read(world: &World, name: &'static str, value: Option<String>) -> Result<String, String> {
    let mut vars = world.vars("127.0.0.1:4435");
    vars.retain(|(key, _)| *key != name);
    vars.extend(value.map(|value| (name, value)));
    Config::read(var(vars)).map(|config| config.address.to_string()).map_err(|e| e.to_string())
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_gateway_names_each_variable_that_it_lacks_or_cannot_parse() {
    let world = World::new().await;
    let found = [
      read(&world, "", None),
      read(&world, "GATEWAY_ADDRESS", None),
      read(&world, "REDIS_URL", None),
      read(&world, "GATEWAY_KEY", None),
      read(&world, "GATEWAY_CERTIFICATE", None),
      read(&world, "GATEWAY_LINK_CERTIFICATE", None),
      read(&world, "GATEWAY_LINK_KEY", None),
      read(&world, "GATEWAY_AUTHORITY", None),
      read(&world, "GATEWAY_ADDRESS", Some("gateway".to_owned())),
      read(&world, "REDIS_URL", Some("nope".to_owned())),
    ];
    let unset = |name: &str| Err(format!("the variable {name} is not set"));
    assert_eq!(
      found,
      [
        Ok("127.0.0.1:4435".to_owned()),
        unset("GATEWAY_ADDRESS"),
        unset("REDIS_URL"),
        unset("GATEWAY_KEY"),
        unset("GATEWAY_CERTIFICATE"),
        unset("GATEWAY_LINK_CERTIFICATE"),
        unset("GATEWAY_LINK_KEY"),
        unset("GATEWAY_AUTHORITY"),
        Err(
          "GATEWAY_ADDRESS is not an address and a port: invalid socket address syntax".to_owned()
        ),
        Err(
          "REDIS_URL is not a URL of Redis: Redis URL did not parse - InvalidClientConfig"
            .to_owned()
        ),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_gateway_names_each_file_of_pem_that_makes_no_tls() {
    let world = World::new().await;
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n";
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    for (name, text) in [("broken.pem", broken), ("garbage.pem", garbage), ("empty.pem", "")] {
      fs::write(world.path(name), text).expect("a file");
    }
    let file = |name: &'static str, path: &str| read(&world, name, Some(world.path(path)));
    let found = [
      file("GATEWAY_CERTIFICATE", "absent.pem"),
      file("GATEWAY_KEY", "gateway.pem"),
      file("GATEWAY_LINK_CERTIFICATE", "broken.pem"),
      file("GATEWAY_LINK_KEY", "link.pem"),
      file("GATEWAY_AUTHORITY", "broken.pem"),
      file("GATEWAY_AUTHORITY", "garbage.pem"),
      file("GATEWAY_AUTHORITY", "empty.pem"),
      file("GATEWAY_KEY", "link.key"),
      file("GATEWAY_LINK_KEY", "gateway.key"),
    ];
    let section = "missing section end marker: [67, 69, 82, 84, 73, 70, 73, 67, 65, 84, 69]";
    let absent = fs::File::open(world.path("absent.pem")).expect_err("no file absent.pem");
    let anchors = "no root trust anchors were provided";
    let tls = "the certificates and the key make no server of TLS: ";
    let mismatch = Err(format!("{tls}keys may not be consistent: KeyMismatch"));
    let pem = |file: &str, why: &str| {
      Err(format!("{} holds no PEM that can be read: {why}", world.path(file)))
    };
    assert_eq!(
      found,
      [
        pem("absent.pem", &format!("I/O error: {absent}")),
        pem("gateway.pem", "no items found"),
        pem("broken.pem", section),
        pem("link.pem", "no items found"),
        pem("broken.pem", section),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err(format!("the authority holds no certificate that signs a peer: {anchors}")),
        mismatch.clone(),
        mismatch,
      ]
    );
  }
}
