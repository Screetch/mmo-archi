use std::net::{AddrParseError, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use quinn::ServerConfig;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::RootCertStore;
use rustls::crypto::ring;
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{VerifierBuilderError, WebPkiClientVerifier};
use sqlx::postgres::PgConnectOptions;

/// What the keeper reads from its environment, as the doc of the crate says.
pub(crate) struct Config {
  /// The address of the links of the zone writers.
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
  #[error("{name} holds no PEM that the keeper reads: {source}")]
  Pem { name: &'static str, source: pem::Error },
  #[error("the certificates and the key of the keeper make no server of TLS: {0}")]
  Tls(#[from] rustls::Error),
  #[error("KEEPER_AUTHORITY holds no certificate that signs a zone writer: {0}")]
  Authority(#[from] VerifierBuilderError),
}

impl Config {
  /// Reads the config from `var`, which gives the value of a variable of the environment.
  pub(crate) fn read(var: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
    let address = value(&var, "KEEPER_ADDRESS")?.parse()?;
    let database = PgConnectOptions::from_str(&value(&var, "DATABASE_URL")?)?;
    let chain = pem(&var, "KEEPER_CERTIFICATE", certificates)?;
    let key = pem(&var, "KEEPER_KEY", PrivateKeyDer::from_pem_file)?;
    let mut roots = RootCertStore::empty();
    for certificate in pem(&var, "KEEPER_AUTHORITY", certificates)? {
      roots.add(certificate)?;
    }
    Ok(Self { address, database, server: server(roots, chain, key)? })
  }
}

#[expect(clippy::expect_used, reason = "ring holds TLS 1.3 and the cipher suite of QUIC")]
fn server(
  roots: RootCertStore,
  chain: Vec<CertificateDer<'static>>,
  key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Error> {
  let provider = Arc::new(ring::default_provider());
  let writers =
    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider)).build()?;
  let tls = rustls::ServerConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("ring holds TLS 1.3")
    .with_client_cert_verifier(writers)
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
  fn the_keeper_reads_each_variable_and_names_the_one_that_it_cannot_use() {
    let world = World::new();
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n";
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    for (name, text) in [("broken.pem", broken), ("garbage.pem", garbage), ("empty.pem", "")] {
      fs::write(world.path(name), text).expect("a file");
    }
    let vars = world.vars("127.0.0.1:4433", "postgres://keeper@localhost/game");
    let read = |name: &'static str, value: Option<String>| {
      let mut vars = vars.clone();
      vars.retain(|(key, _)| *key != name);
      vars.extend(value.map(|value| (name, value)));
      Config::read(var(&vars)).map(|config| config.address.to_string()).map_err(|e| e.to_string())
    };
    let file = |name: &str| Some(world.path(name));
    let found = [
      read("", None),
      read("KEEPER_ADDRESS", None),
      read("DATABASE_URL", None),
      read("KEEPER_KEY", None),
      read("KEEPER_ADDRESS", Some("keeper".to_owned())),
      read("DATABASE_URL", Some("keeper".to_owned())),
      read("KEEPER_CERTIFICATE", file("broken.pem")),
      read("KEEPER_CERTIFICATE", file("absent.pem")),
      read("KEEPER_KEY", file("keeper.pem")),
      read("KEEPER_AUTHORITY", file("broken.pem")),
      read("KEEPER_AUTHORITY", file("garbage.pem")),
      read("KEEPER_AUTHORITY", file("empty.pem")),
      read("KEEPER_KEY", file("writer.key")),
    ];
    let unset = |name: &str| Err(format!("the variable {name} is not set"));
    let relative = "error with configuration: relative URL without a base";
    let section = "missing section end marker: [67, 69, 82, 84, 73, 70, 73, 67, 65, 84, 69]";
    let absent = fs::File::open(world.path("absent.pem")).err().map(|error| error.to_string());
    let absent = absent.expect("no file absent.pem");
    let anchors = "no root trust anchors were provided";
    let tls = "the certificates and the key of the keeper make no server of TLS: ";
    assert_eq!(
      found,
      [
        Ok("127.0.0.1:4433".to_owned()),
        unset("KEEPER_ADDRESS"),
        unset("DATABASE_URL"),
        unset("KEEPER_KEY"),
        Err(
          "KEEPER_ADDRESS is not an address and a port: invalid socket address syntax".to_owned()
        ),
        Err(format!("DATABASE_URL is not a URL of PostgreSQL: {relative}")),
        Err(format!("KEEPER_CERTIFICATE holds no PEM that the keeper reads: {section}")),
        Err(format!("KEEPER_CERTIFICATE holds no PEM that the keeper reads: I/O error: {absent}")),
        Err("KEEPER_KEY holds no PEM that the keeper reads: no items found".to_owned()),
        Err(format!("KEEPER_AUTHORITY holds no PEM that the keeper reads: {section}")),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err(format!("KEEPER_AUTHORITY holds no certificate that signs a zone writer: {anchors}")),
        Err(format!("{tls}keys may not be consistent: KeyMismatch")),
      ]
    );
  }
}
