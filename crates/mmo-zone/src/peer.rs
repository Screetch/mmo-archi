//! A world for the tests: an authority, the zone writer and a gateway with their certificates, the
//! link of a gateway to the zone writer, and a keeper.

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

use mmo_protocol::{
  Frames, FromKeeper, Hello, Intents, Link, Relayed, Snapshot, ToKeeper, ToZone, decode, encode,
  frame, relayed, unrelayed,
};
use mmo_store::Store;
use mmo_store::testing::Redis;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{
  ClientConfig, Connection, ConnectionError, Endpoint, Incoming, RecvStream, SendStream,
};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use redis::AsyncCommands;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::TempDir;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::{JoinHandle, JoinSet};

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
      ("ZONE_KEEPER", "localhost:4433".to_owned()),
      ("ZONE_CERTIFICATE", self.path("zone.pem")),
      ("ZONE_KEY", self.path("zone.key")),
      ("ZONE_AUTHORITY", self.path("authority.pem")),
    ]
  }

  /// The endpoint of a keeper on a free port, which shows the certificate of the zone writer, for
  /// the name `localhost`, and takes only a peer that the authority signed.
  pub(crate) fn keeper(&self) -> Endpoint {
    let file = |name: &str| self.folder.path().join(name);
    let tls = mmo_link::server(&file("zone.pem"), &file("zone.key"), &file("authority.pem"));
    Endpoint::server(tls.expect("a keeper"), ([127, 0, 0, 1], 0).into()).expect("an endpoint")
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

/// A keeper that answers each message of a zone writer from a store, as mmo-keeper does, until
/// the test stops it.
pub(crate) struct Keeper {
  endpoint: Endpoint,
  serve: JoinHandle<()>,
  /// Each link of a writer that ends sends a unit on it.
  ends: UnboundedSender<()>,
  ended: UnboundedReceiver<()>,
}

impl Keeper {
  pub(crate) fn start(world: &World, store: Store) -> Self {
    let endpoint = world.keeper();
    let (ends, ended) = unbounded_channel();
    let serve = tokio::spawn(keep(endpoint.clone(), store, ends.clone()));
    Self { endpoint, serve, ends, ended }
  }

  /// The address that a zone writer dials, as `ZONE_KEEPER` gives it.
  pub(crate) fn address(&self) -> String {
    format!("localhost:{}", self.endpoint.local_addr().expect("an address").port())
  }

  /// Ends each link of the keeper, and refuses each new link until [`Keeper::resume`].
  pub(crate) fn stop(&mut self) {
    self.serve.abort();
    let endpoint = self.endpoint.clone();
    self.serve = tokio::spawn(async move {
      loop {
        endpoint.accept().await.expect("the endpoint of a keeper stays open").refuse();
      }
    });
  }

  /// Takes the links of the zone writers again, and answers them from `store`.
  pub(crate) fn resume(&mut self, store: Store) {
    self.serve.abort();
    self.serve = tokio::spawn(keep(self.endpoint.clone(), store, self.ends.clone()));
  }

  /// Waits for the end of a link, which a writer that stops drops.
  pub(crate) async fn ended(&mut self) {
    self.ended.recv().await.expect("the keeper holds a sender");
  }
}

impl Drop for Keeper {
  fn drop(&mut self) {
    self.serve.abort();
  }
}

/// Takes the links of the zone writers, each in a task that ends with this one.
async fn keep(endpoint: Endpoint, store: Store, ends: UnboundedSender<()>) {
  let mut links = JoinSet::new();
  loop {
    let incoming = endpoint.accept().await.expect("the endpoint of a keeper stays open");
    links.spawn(answer(incoming, store.clone(), ends.clone()));
  }
}

/// Answers each message of a zone writer in order, until the writer drops the link.
async fn answer(incoming: Incoming, store: Store, ends: UnboundedSender<()>) {
  let connection = incoming.await.expect("a link");
  let (mut send, recv) = connection.accept_bi().await.expect("a writer opens its stream at once");
  let mut writer = Stream { recv, frames: Frames::default() };
  // A write to a writer that dropped its link fails, and the next read ends the link.
  drop(send.write_all(&frame(&Hello { build: 0 }.encode()).expect("a hello")).await);
  writer.next().await;
  while let Some(payload) = writer.next().await {
    let answer = match decode(&payload).expect("a message of this version") {
      ToKeeper::Claim { zone } => {
        store.claim(zone).await.map(|epoch| FromKeeper::Claimed { epoch })
      }
      ToKeeper::Save(batch) => {
        store.save(&batch).await.map(|written| FromKeeper::Saved { written })
      }
    };
    let answer = answer.unwrap_or_else(|error| FromKeeper::Refused(error.refusal()));
    drop(send.write_all(&frame(&encode(&answer)).expect("an answer")).await);
  }
  // The send fails only when the test dropped the keeper, and then no one waits for the end.
  ends.send(()).ok();
}

/// The stream of a zone writer to a keeper.
struct Stream {
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// The payload of the next frame of the writer, or None when the link ends.
  async fn next(&mut self) -> Option<Vec<u8>> {
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop().expect("a frame of the writer") {
        return Some(payload);
      }
      let size = self.recv.read(&mut chunk).await.ok().flatten()?;
      self.frames.push(&chunk[..size]);
    }
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
    self.datagram(&relayed(intents.link, &encode(&intents.message)));
  }

  /// The next snapshot that the zone writer sends to `link`, after the snapshots of the other
  /// links.
  pub(crate) async fn snapshot(&self, link: Link) -> Snapshot {
    loop {
      let bytes = self.connection.read_datagram().await.expect("a datagram");
      let (to, snapshot) = unrelayed(&bytes).expect("a link");
      if to == link {
        return decode(snapshot).expect("a snapshot of this version");
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
