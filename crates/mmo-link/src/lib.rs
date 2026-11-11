//! The TLS of the links between the servers of mmo-archi, which run on QUIC (section 4 of
//! `docs/architecture.md`).
//!
//! A server of the game takes a link only from a peer whose certificate an authority of the
//! operator signed, so a process that the operator did not start takes no part in the game. A
//! server reads its certificate chain, its private key and the certificates of the authority from
//! PEM files, and makes its [`ServerConfig`] with [`server`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use quinn::ServerConfig;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::RootCertStore;
use rustls::crypto::ring;
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{VerifierBuilderError, WebPkiClientVerifier};

/// Why the files of a server make no [`ServerConfig`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// A file is absent, or holds no PEM item of the kind that the server needs.
  #[error("{} holds no PEM that can be read: {source}", path.display())]
  Pem {
    /// The file.
    path: PathBuf,
    /// What the reader of PEM refused.
    source: pem::Error,
  },
  /// The chain and the key do not make a server of TLS, or the authority holds a bad certificate.
  #[error("the certificates and the key make no server of TLS: {0}")]
  Tls(#[from] rustls::Error),
  /// The file of the authority holds no certificate that can sign a peer.
  #[error("the authority holds no certificate that signs a peer: {0}")]
  Authority(#[from] VerifierBuilderError),
}

/// The QUIC of a server, which speaks TLS 1.3 with the chain in `certificate` and the key in `key`,
/// and takes only a peer whose certificate a certificate of `authority` signs.
#[expect(clippy::expect_used, reason = "ring holds TLS 1.3 and the cipher suite of QUIC")]
pub fn server(certificate: &Path, key: &Path, authority: &Path) -> Result<ServerConfig, Error> {
  let chain = pem(certificate, certificates)?;
  let key = pem(key, PrivateKeyDer::from_pem_file)?;
  let mut roots = RootCertStore::empty();
  for certificate in pem(authority, certificates)? {
    roots.add(certificate)?;
  }
  let provider = Arc::new(ring::default_provider());
  let peers =
    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider)).build()?;
  let tls = rustls::ServerConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("ring holds TLS 1.3")
    .with_client_cert_verifier(peers)
    .with_single_cert(chain, key)?;
  let quic = QuicServerConfig::try_from(tls).expect("ring holds the first cipher suite of QUIC");
  Ok(ServerConfig::with_crypto(Arc::new(quic)))
}

fn pem<T>(path: &Path, read: impl FnOnce(PathBuf) -> Result<T, pem::Error>) -> Result<T, Error> {
  read(path.to_owned()).map_err(|source| Error::Pem { path: path.to_owned(), source })
}

fn certificates(path: PathBuf) -> Result<Vec<CertificateDer<'static>>, pem::Error> {
  CertificateDer::pem_file_iter(path)?.collect()
}

#[cfg(test)]
mod tests {
  use std::fs;

  use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
  use tempfile::TempDir;

  use super::server;

  /// The files of an operator: an authority, and a server that it signed, with its key and the
  /// key of another one.
  fn files() -> TempDir {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("no name");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let authority = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a key"))
      .expect("an authority");
    let key = KeyPair::generate().expect("a key");
    let params = CertificateParams::new(vec!["localhost".to_owned()]).expect("a name");
    let certificate = params.signed_by(&key, &authority).expect("a certificate");
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n";
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    let folder = TempDir::new().expect("a folder");
    let files = [
      ("authority.pem", authority.pem()),
      ("server.pem", certificate.pem()),
      ("server.key", key.serialize_pem()),
      ("other.key", KeyPair::generate().expect("a key").serialize_pem()),
      ("broken.pem", broken.to_owned()),
      ("garbage.pem", garbage.to_owned()),
      ("empty.pem", String::new()),
    ];
    for (name, text) in files {
      fs::write(folder.path().join(name), text).expect("a file");
    }
    folder
  }

  #[test]
  fn a_server_takes_the_files_of_the_operator_and_names_the_file_that_it_cannot_use() {
    let folder = files();
    let at = |name: &str| folder.path().join(name);
    let made = |certificate: &str, key: &str, authority: &str| -> Result<(), String> {
      server(&at(certificate), &at(key), &at(authority))
        .map(drop)
        .map_err(|error| error.to_string())
    };
    let (certificate, key, authority) = ("server.pem", "server.key", "authority.pem");
    let found = [
      made(certificate, key, authority),
      made("broken.pem", key, authority),
      made("absent.pem", key, authority),
      made(certificate, certificate, authority),
      made(certificate, key, "broken.pem"),
      made(certificate, key, "garbage.pem"),
      made(certificate, key, "empty.pem"),
      made(certificate, "other.key", authority),
    ];
    let section = "missing section end marker: [67, 69, 82, 84, 73, 70, 73, 67, 65, 84, 69]";
    let absent = fs::File::open(at("absent.pem")).expect_err("no file absent.pem");
    let pem = |name: &str, why: &str| {
      Err(format!("{} holds no PEM that can be read: {why}", at(name).display()))
    };
    let tls = "the certificates and the key make no server of TLS: ";
    assert_eq!(
      found,
      [
        Ok(()),
        pem("broken.pem", section),
        pem("absent.pem", &format!("I/O error: {absent}")),
        pem(certificate, "no items found"),
        pem("broken.pem", section),
        Err(format!("{tls}invalid peer certificate: BadEncoding")),
        Err(
          "the authority holds no certificate that signs a peer: no root trust anchors were \
           provided"
            .to_owned()
        ),
        Err(format!("{tls}keys may not be consistent: KeyMismatch")),
      ]
    );
  }
}
