//! A world for the tests: an authority, the keeper and a zone writer with their certificates, and
//! the link of a zone writer to the keeper.

use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

use mmo_protocol::{Frames, FromKeeper, Hello, ToKeeper, decode, encode, frame};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::TempDir;

use crate::link::BUILD;

/// The certificates of a world, and the folder of the PEM files of the keeper: `authority.pem`,
/// `keeper.pem`, `keeper.key` and `writer.key`.
pub(crate) struct World {
  folder: TempDir,
  authority: CertificateDer<'static>,
  writer: CertificateDer<'static>,
  key: PrivatePkcs8KeyDer<'static>,
}

impl World {
  pub(crate) fn new() -> Self {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("no name");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let authority = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a key"))
      .expect("an authority");
    let signed = |names: Vec<String>| {
      let key = KeyPair::generate().expect("a key");
      let params = CertificateParams::new(names).expect("a name");
      (params.signed_by(&key, &authority).expect("a certificate"), key)
    };
    let (keeper, keeper_key) = signed(vec!["localhost".to_owned()]);
    let (writer, writer_key) = signed(Vec::new());
    let folder = TempDir::new().expect("a folder");
    let files = [
      ("authority.pem", authority.pem()),
      ("keeper.pem", keeper.pem()),
      ("keeper.key", keeper_key.serialize_pem()),
      ("writer.key", writer_key.serialize_pem()),
    ];
    for (name, text) in files {
      fs::write(folder.path().join(name), text).expect("a file");
    }
    Self {
      folder,
      authority: authority.der().clone(),
      writer: writer.der().clone(),
      key: PrivatePkcs8KeyDer::from(writer_key.serialize_der()),
    }
  }

  /// The path of a file of the folder of the world.
  pub(crate) fn path(&self, name: &str) -> String {
    self.folder.path().join(name).to_str().expect("a path in UTF-8").to_owned()
  }

  /// The variables of a keeper of this world, at `address` and with the database at `database`.
  pub(crate) fn vars(&self, address: &str, database: &str) -> Vec<(&'static str, String)> {
    vec![
      ("KEEPER_ADDRESS", address.to_owned()),
      ("DATABASE_URL", database.to_owned()),
      ("KEEPER_CERTIFICATE", self.path("keeper.pem")),
      ("KEEPER_KEY", self.path("keeper.key")),
      ("KEEPER_AUTHORITY", self.path("authority.pem")),
    ]
  }

  /// The endpoint of a zone writer, which trusts the authority, and which shows the certificate
  /// of the writer when it is `signed`.
  pub(crate) fn writer(&self, signed: bool) -> Endpoint {
    let mut roots = RootCertStore::empty();
    roots.add(self.authority.clone()).expect("the authority");
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
      rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS 1.3")
    .with_root_certificates(roots);
    let tls = if signed {
      let key = PrivateKeyDer::Pkcs8(self.key.clone_key());
      tls.with_client_auth_cert(vec![self.writer.clone()], key).expect("the writer")
    } else {
      tls.with_no_client_auth()
    };
    let quic = QuicClientConfig::try_from(tls).expect("the QUIC of TLS 1.3");
    let mut endpoint = Endpoint::client(([127, 0, 0, 1], 0).into()).expect("an endpoint");
    endpoint.set_default_client_config(ClientConfig::new(Arc::new(quic)));
    endpoint
  }
}

/// The value of each variable that `vars` holds.
pub(crate) fn var(vars: &[(&'static str, String)]) -> impl Fn(&str) -> Option<String> {
  |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
}

/// A zone writer, with the stream that it opened to the keeper.
pub(crate) struct Writer {
  connection: Connection,
  pub(crate) send: SendStream,
  pub(crate) recv: RecvStream,
  frames: Frames,
}

impl Writer {
  pub(crate) async fn link(endpoint: &Endpoint, keeper: SocketAddr) -> Self {
    let connecting = endpoint.connect(keeper, "localhost").expect("a connection");
    let connection = connecting.await.expect("a link");
    let (send, recv) = connection.open_bi().await.expect("a stream");
    Self { connection, send, recv, frames: Frames::default() }
  }

  pub(crate) async fn write(&mut self, bytes: &[u8]) {
    self.send.write_all(bytes).await.expect("a write");
  }

  /// The payload of the next frame of the keeper.
  pub(crate) async fn read(&mut self) -> Vec<u8> {
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop().expect("a frame") {
        return payload;
      }
      let size = self.recv.read(&mut chunk).await.expect("a read").expect("an open stream");
      self.frames.push(&chunk[..size]);
    }
  }

  /// Writes the hello of this version, and checks the hello of the keeper.
  pub(crate) async fn greet(&mut self) {
    self.write(&frame(&Hello { build: 0 }.encode()).expect("a hello")).await;
    let theirs = Hello::decode(&self.read().await).expect("a hello of this version");
    assert_eq!(theirs, Hello { build: BUILD });
  }

  pub(crate) async fn ask(&mut self, message: &ToKeeper) -> FromKeeper {
    self.write(&frame(&encode(message)).expect("a frame")).await;
    decode(&self.read().await).expect("an answer of this version")
  }

  /// The code and the reason with which the keeper closed the link, or the fault of the link.
  pub(crate) async fn closed(&self) -> Result<(u64, String), ConnectionError> {
    match self.connection.closed().await {
      ConnectionError::ApplicationClosed(close) => {
        Ok((close.error_code.into_inner(), String::from_utf8_lossy(&close.reason).into_owned()))
      }
      fault => Err(fault),
    }
  }
}
