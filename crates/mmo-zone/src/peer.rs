//! A world for the tests: an authority, the zone writer and a gateway with their certificates, and
//! the link of a gateway to the zone writer.

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

use mmo_protocol::{
  Frames, Hello, Intents, Link, Relayed, Snapshot, ToZone, decode, encode, frame,
};
use mmo_store::testing::Redis;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use redis::AsyncCommands;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::TempDir;

use crate::link::BUILD;

/// The certificates of a world, and the folder of the PEM files of the zone writer:
/// `authority.pem`, `zone.pem`, `zone.key` and `gateway.key`.
pub(crate) struct World {
  folder: TempDir,
  authority: CertificateDer<'static>,
  gateway: CertificateDer<'static>,
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
    let (zone, zone_key) = signed(vec!["localhost".to_owned()]);
    let (gateway, gateway_key) = signed(Vec::new());
    let folder = TempDir::new().expect("a folder");
    let files = [
      ("authority.pem", authority.pem()),
      ("zone.pem", zone.pem()),
      ("zone.key", zone_key.serialize_pem()),
      ("gateway.key", gateway_key.serialize_pem()),
    ];
    for (name, text) in files {
      fs::write(folder.path().join(name), text).expect("a file");
    }
    Self {
      folder,
      authority: authority.der().clone(),
      gateway: gateway.der().clone(),
      key: PrivatePkcs8KeyDer::from(gateway_key.serialize_der()),
    }
  }

  /// The path of a file of the folder of the world.
  pub(crate) fn path(&self, name: &str) -> String {
    self.folder.path().join(name).to_str().expect("a path in UTF-8").to_owned()
  }

  /// The variables of the writer of zone 1 in this world, at `address` and with the database at
  /// `database` and the Redis at `redis`.
  pub(crate) fn vars(
    &self,
    address: &str,
    database: &str,
    redis: &str,
  ) -> Vec<(&'static str, String)> {
    vec![
      ("ZONE_ID", "1".to_owned()),
      ("ZONE_ADDRESS", address.to_owned()),
      ("ZONE_PUBLIC", "zone-1:4433".to_owned()),
      ("DATABASE_URL", database.to_owned()),
      ("REDIS_URL", redis.to_owned()),
      ("ZONE_CERTIFICATE", self.path("zone.pem")),
      ("ZONE_KEY", self.path("zone.key")),
      ("ZONE_AUTHORITY", self.path("authority.pem")),
    ]
  }

  /// The endpoint of a gateway, which trusts the authority, and which shows the certificate of
  /// the gateway when it is `signed`.
  pub(crate) fn gateway(&self, signed: bool) -> Endpoint {
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
      tls.with_client_auth_cert(vec![self.gateway.clone()], key).expect("the gateway")
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
pub(crate) fn var(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
  move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
}

/// The fields of a hash of `redis`.
pub(crate) async fn hash(redis: &Redis, key: &str) -> BTreeMap<String, String> {
  let client = redis::Client::open(redis.url()).expect("a URL");
  let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
  connection.hgetall(key).await.expect("a hash")
}

/// The milliseconds that the key of `redis` still lives.
pub(crate) async fn life(redis: &Redis, key: &str) -> i64 {
  let client = redis::Client::open(redis.url()).expect("a URL");
  let mut connection = client.get_multiplexed_async_connection().await.expect("a connection");
  redis::cmd("PTTL").arg(key).query_async(&mut connection).await.expect("a life")
}

/// A gateway, with the stream that it opened to the zone writer.
pub(crate) struct Peer {
  connection: Connection,
  pub(crate) send: SendStream,
  pub(crate) recv: RecvStream,
  frames: Frames,
}

impl Peer {
  pub(crate) async fn link(endpoint: &Endpoint, zone: SocketAddr) -> Self {
    let connecting = endpoint.connect(zone, "localhost").expect("a connection");
    let connection = connecting.await.expect("a link");
    let (send, recv) = connection.open_bi().await.expect("a stream");
    Self { connection, send, recv, frames: Frames::default() }
  }

  pub(crate) async fn write(&mut self, bytes: &[u8]) {
    self.send.write_all(bytes).await.expect("a write");
  }

  /// The payload of the next frame of the zone writer.
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

  /// Writes the hello of this version, and checks the hello of the zone writer.
  pub(crate) async fn greet(&mut self) {
    self.write(&frame(&Hello { build: 0 }.encode()).expect("a hello")).await;
    let theirs = Hello::decode(&self.read().await).expect("a hello of this version");
    assert_eq!(theirs, Hello { build: BUILD });
  }

  pub(crate) async fn tell(&mut self, message: &ToZone) {
    self.write(&frame(&encode(message)).expect("a frame")).await;
  }

  pub(crate) fn datagram(&self, bytes: &[u8]) {
    self.connection.send_datagram(bytes.to_vec().into()).expect("a datagram");
  }

  pub(crate) fn intents(&self, intents: &Relayed<Intents>) {
    self.datagram(&encode(intents));
  }

  /// The next snapshot that the zone writer sends to `link`, after the snapshots of the other
  /// links.
  pub(crate) async fn snapshot(&self, link: Link) -> Snapshot {
    loop {
      let bytes = self.connection.read_datagram().await.expect("a datagram");
      let relayed: Relayed<Snapshot> = decode(&bytes).expect("a snapshot of this version");
      if relayed.link == link {
        return relayed.message;
      }
    }
  }

  pub(crate) fn close(&self) {
    self.connection.close(0u32.into(), b"");
  }

  /// The code and the reason with which the zone writer closed the link, or the fault of the
  /// link.
  pub(crate) async fn closed(&self) -> Result<(u64, String), ConnectionError> {
    match self.connection.closed().await {
      ConnectionError::ApplicationClosed(close) => {
        Ok((close.error_code.into_inner(), String::from_utf8_lossy(&close.reason).into_owned()))
      }
      fault => Err(fault),
    }
  }
}
