use std::net::{AddrParseError, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, IdleTimeout, ServerConfig, TransportConfig, VarInt};
use redis::{IntoConnectionInfo, ProtocolVersion, RedisError};
use rustls::RootCertStore;
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A link closes when its peer sends nothing for 3 s, so a client sees the stop of its gateway
/// within 3 s, and attaches to another.
const IDLE: VarInt = VarInt::from_u32(3000);

/// A link that has nothing to send pings its peer each second, so it stays open while both run.
const PING: Duration = Duration::from_secs(1);

/// What the gateway reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The address of the clients.
  pub(crate) address: SocketAddr,
  /// The QUIC of the clients.
  pub(crate) clients: ServerConfig,
  /// The QUIC of the links to the zone writers, which shows the certificate of the gateway and
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
  #[error("{name} holds no PEM that the gateway reads: {source}")]
  Pem { name: &'static str, source: pem::Error },
  #[error("the certificates and the keys of the gateway make no TLS: {0}")]
  Tls(#[from] rustls::Error),
  #[error("GATEWAY_AUTHORITY holds no certificate that signs a zone writer")]
  Authority,
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  #[expect(clippy::expect_used, reason = "a client of a parsed ConnectionInfo checks nothing more")]
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let address = value(&var, "GATEWAY_ADDRESS")?.parse()?;
    let redis = value(&var, "REDIS_URL")?.as_str().into_connection_info()?;
    let resp3 = redis.redis_settings().clone().set_protocol(ProtocolVersion::RESP3);
    let chain = pem(&var, "GATEWAY_CERTIFICATE", certificates)?;
    let key = pem(&var, "GATEWAY_KEY", PrivateKeyDer::from_pem_file)?;
    let link = pem(&var, "GATEWAY_LINK_CERTIFICATE", certificates)?;
    let link_key = pem(&var, "GATEWAY_LINK_KEY", PrivateKeyDer::from_pem_file)?;
    let mut roots = RootCertStore::empty();
    for certificate in pem(&var, "GATEWAY_AUTHORITY", certificates)? {
      roots.add(certificate)?;
    }
    if roots.is_empty() {
      return Err(Error::Authority);
    }
    let provider = Arc::new(ring::default_provider());
    Ok(Self {
      address,
      clients: clients(Arc::clone(&provider), chain, key)?,
      writers: writers(provider, roots, link, link_key)?,
      redis: redis::Client::open(redis.set_redis_settings(resp3)).expect("a client of Redis"),
    })
  }
}

#[expect(clippy::expect_used, reason = "ring holds TLS 1.3 and the cipher suite of QUIC")]
fn clients(
  provider: Arc<CryptoProvider>,
  chain: Vec<CertificateDer<'static>>,
  key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Error> {
  let tls = rustls::ServerConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("ring holds TLS 1.3")
    .with_no_client_auth()
    .with_single_cert(chain, key)?;
  let quic = QuicServerConfig::try_from(tls).expect("ring holds the first cipher suite of QUIC");
  let mut config = ServerConfig::with_crypto(Arc::new(quic));
  config.transport_config(transport());
  Ok(config)
}

#[expect(clippy::expect_used, reason = "ring holds TLS 1.3 and the cipher suite of QUIC")]
fn writers(
  provider: Arc<CryptoProvider>,
  roots: RootCertStore,
  chain: Vec<CertificateDer<'static>>,
  key: PrivateKeyDer<'static>,
) -> Result<ClientConfig, Error> {
  let tls = rustls::ClientConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("ring holds TLS 1.3")
    .with_root_certificates(roots)
    .with_client_auth_cert(chain, key)?;
  let quic = QuicClientConfig::try_from(tls).expect("ring holds the first cipher suite of QUIC");
  let mut config = ClientConfig::new(Arc::new(quic));
  config.transport_config(transport());
  Ok(config)
}

fn transport() -> Arc<TransportConfig> {
  let mut transport = TransportConfig::default();
  transport.max_idle_timeout(Some(IdleTimeout::from(IDLE))).keep_alive_interval(Some(PING));
  Arc::new(transport)
}

fn value(var: &impl Fn(&str) -> Option<String>, name: &'static str) -> Result<String, Error> {
  var(name).ok_or(Error::Unset(name))
}

fn pem<T>(
  var: &impl Fn(&str) -> Option<String>,
  name: &'static str,
  read: impl FnOnce(String) -> Result<T, pem::Error>,
) -> Result<T, Error> {
  read(value(var, name)?).map_err(|source| Error::Pem { name, source })
}

fn certificates(path: String) -> Result<Vec<CertificateDer<'static>>, pem::Error> {
  CertificateDer::pem_file_iter(path)?.collect()
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
    let tls = "the certificates and the keys of the gateway make no TLS: ";
    let mismatch = Err(format!("{tls}keys may not be consistent: KeyMismatch"));
    let pem =
      |name: &str, why: &str| Err(format!("{name} holds no PEM that the gateway reads: {why}"));
    assert_eq!(
      found,
      [
        pem("GATEWAY_CERTIFICATE", &format!("I/O error: {absent}")),
        pem("GATEWAY_KEY", "no items found"),
        pem("GATEWAY_LINK_CERTIFICATE", section),
        pem("GATEWAY_LINK_KEY", "no items found"),
        pem("GATEWAY_AUTHORITY", section),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err("GATEWAY_AUTHORITY holds no certificate that signs a zone writer".to_owned()),
        mismatch.clone(),
        mismatch,
      ]
    );
  }
}
