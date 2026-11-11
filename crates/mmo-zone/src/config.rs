use std::net::{AddrParseError, SocketAddr};
use std::num::ParseIntError;
use std::str::FromStr;
use std::sync::Arc;

use mmo_protocol::ZoneId;
use quinn::ServerConfig;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::RootCertStore;
use rustls::crypto::ring;
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{VerifierBuilderError, WebPkiClientVerifier};
use sqlx::postgres::PgConnectOptions;

/// What the zone writer reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The zone that the writer runs.
  pub(crate) zone: ZoneId,
  /// The address of the links of the gateways.
  pub(crate) address: SocketAddr,
  /// The database of the game.
  pub(crate) database: PgConnectOptions,
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
  #[error("{name} holds no PEM that the zone writer reads: {source}")]
  Pem { name: &'static str, source: pem::Error },
  #[error("the certificates and the key of the zone writer make no server of TLS: {0}")]
  Tls(#[from] rustls::Error),
  #[error("ZONE_AUTHORITY holds no certificate that signs a gateway: {0}")]
  Authority(#[from] VerifierBuilderError),
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let zone = ZoneId(value(&var, "ZONE_ID")?.parse()?);
    let address = value(&var, "ZONE_ADDRESS")?.parse()?;
    let database = PgConnectOptions::from_str(&value(&var, "DATABASE_URL")?)?;
    let chain = pem(&var, "ZONE_CERTIFICATE", certificates)?;
    let key = pem(&var, "ZONE_KEY", PrivateKeyDer::from_pem_file)?;
    let mut roots = RootCertStore::empty();
    for certificate in pem(&var, "ZONE_AUTHORITY", certificates)? {
      roots.add(certificate)?;
    }
    Ok(Self { zone, address, database, server: server(roots, chain, key)? })
  }
}

#[expect(clippy::expect_used, reason = "ring holds TLS 1.3 and the cipher suite of QUIC")]
fn server(
  roots: RootCertStore,
  chain: Vec<CertificateDer<'static>>,
  key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Error> {
  let provider = Arc::new(ring::default_provider());
  let gateways =
    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider)).build()?;
  let tls = rustls::ServerConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("ring holds TLS 1.3")
    .with_client_cert_verifier(gateways)
    .with_single_cert(chain, key)?;
  let quic = QuicServerConfig::try_from(tls).expect("ring holds the first cipher suite of QUIC");
  Ok(ServerConfig::with_crypto(Arc::new(quic)))
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
  use crate::peer::{World, var};

  #[test]
  fn the_zone_writer_reads_each_variable_and_names_the_one_that_it_cannot_use() {
    let world = World::new();
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n";
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    for (name, text) in [("broken.pem", broken), ("garbage.pem", garbage), ("empty.pem", "")] {
      fs::write(world.path(name), text).expect("a file");
    }
    let vars = world.vars("127.0.0.1:4433", "postgres://zone@localhost/game");
    let read = |name: &'static str, value: Option<String>| {
      let mut vars = vars.clone();
      vars.retain(|(key, _)| *key != name);
      vars.extend(value.map(|value| (name, value)));
      let config = Config::read(var(vars));
      config.map(|config| (config.zone.0, config.address.to_string())).map_err(|e| e.to_string())
    };
    let file = |name: &str| Some(world.path(name));
    let found = [
      read("", None),
      read("ZONE_ID", None),
      read("ZONE_ADDRESS", None),
      read("DATABASE_URL", None),
      read("ZONE_KEY", None),
      read("ZONE_ID", Some("meadow".to_owned())),
      read("ZONE_ADDRESS", Some("zone".to_owned())),
      read("DATABASE_URL", Some("zone".to_owned())),
      read("ZONE_CERTIFICATE", file("broken.pem")),
      read("ZONE_CERTIFICATE", file("absent.pem")),
      read("ZONE_KEY", file("zone.pem")),
      read("ZONE_AUTHORITY", file("broken.pem")),
      read("ZONE_AUTHORITY", file("garbage.pem")),
      read("ZONE_AUTHORITY", file("empty.pem")),
      read("ZONE_KEY", file("gateway.key")),
    ];
    let unset = |name: &str| Err(format!("the variable {name} is not set"));
    let relative = "error with configuration: relative URL without a base";
    let section = "missing section end marker: [67, 69, 82, 84, 73, 70, 73, 67, 65, 84, 69]";
    let absent = fs::File::open(world.path("absent.pem")).expect_err("no file absent.pem");
    let anchors = "no root trust anchors were provided";
    let tls = "the certificates and the key of the zone writer make no server of TLS: ";
    let pem =
      |name: &str, why: &str| Err(format!("{name} holds no PEM that the zone writer reads: {why}"));
    assert_eq!(
      found,
      [
        Ok((1, "127.0.0.1:4433".to_owned())),
        unset("ZONE_ID"),
        unset("ZONE_ADDRESS"),
        unset("DATABASE_URL"),
        unset("ZONE_KEY"),
        Err("ZONE_ID is not the id of a zone: invalid digit found in string".to_owned()),
        Err("ZONE_ADDRESS is not an address and a port: invalid socket address syntax".to_owned()),
        Err(format!("DATABASE_URL is not a URL of PostgreSQL: {relative}")),
        pem("ZONE_CERTIFICATE", section),
        pem("ZONE_CERTIFICATE", &format!("I/O error: {absent}")),
        pem("ZONE_KEY", "no items found"),
        pem("ZONE_AUTHORITY", section),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err(format!("ZONE_AUTHORITY holds no certificate that signs a gateway: {anchors}")),
        Err(format!("{tls}keys may not be consistent: KeyMismatch")),
      ]
    );
  }
}
