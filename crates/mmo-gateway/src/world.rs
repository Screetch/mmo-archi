//! A world for the tests: an authority that signs the gateway and the zone writers, a Redis of the
//! test alone, the zone writers that a test drives, and the clients.

use std::fs;
use std::future::IntoFuture;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bitcode::{DecodeOwned, Encode};
use mmo_protocol::{
  Attach, Frames, Hello, PROTOCOL, ToClient, Token, ZoneId, decode, encode, frame,
};
use mmo_store::keys;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{self, Timeout};

use crate::run;

/// The user of Redis of the gateway, whose commands and keys a test can take away.
const USER: &str = "gateway";

/// The bound of each step of a test that waits for the gateway, a zone writer or a client. A step
/// takes milliseconds, so a step past the bound hangs, and the test fails with the name of the
/// step.
const STEP: Duration = Duration::from_secs(10);

/// `step` with the bound of [`STEP`]: its caller names the step in the `expect` of the elapsed
/// bound.
pub(crate) fn within<F: IntoFuture>(step: F) -> Timeout<F::IntoFuture> {
  time::timeout(STEP, step)
}

/// The certificates of a world, and its folder of PEM files: `authority.pem`, `gateway.pem`,
/// `gateway.key`, `link.pem`, `link.key`, `zone.pem` and `zone.key`. `REDIS_SERVER` names the
/// program of Redis, or else `redis-server` of the `PATH` runs.
pub(crate) struct World {
  folder: TempDir,
  authority: CertificateDer<'static>,
  _process: Child,
  _log: Lines<BufReader<ChildStdout>>,
  port: u16,
  pub(crate) redis: ConnectionManager,
}

impl World {
  pub(crate) async fn new() -> Self {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("no name");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let authority = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a key"))
      .expect("an authority");
    let signed = |names: Vec<String>| {
      let key = KeyPair::generate().expect("a key");
      let params = CertificateParams::new(names).expect("a name");
      (params.signed_by(&key, &authority).expect("a certificate"), key)
    };
    let (gateway, gateway_key) = signed(vec!["localhost".to_owned()]);
    let (link, link_key) = signed(Vec::new());
    let (zone, zone_key) = signed(vec!["localhost".to_owned()]);
    let folder = TempDir::new().expect("a folder");
    let files = [
      ("authority.pem", authority.pem()),
      ("gateway.pem", gateway.pem()),
      ("gateway.key", gateway_key.serialize_pem()),
      ("link.pem", link.pem()),
      ("link.key", link_key.serialize_pem()),
      ("zone.pem", zone.pem()),
      ("zone.key", zone_key.serialize_pem()),
    ];
    for (name, text) in files {
      fs::write(folder.path().join(name), text).expect("a file");
    }
    let (process, log, port) = redis_server().await;
    let client = redis::Client::open(format!("redis://127.0.0.1:{port}")).expect("a URL");
    let redis = ConnectionManager::new(client).await.expect("Redis accepts the test");
    let world = Self {
      folder,
      authority: authority.der().clone(),
      _process: process,
      _log: log,
      port,
      redis,
    };
    world.grant("~*", "+@all").await;
    world
  }

  /// The path of a file of the folder of the world.
  pub(crate) fn path(&self, name: &str) -> String {
    self.folder.path().join(name).to_str().expect("a path in UTF-8").to_owned()
  }

  /// The variables of a gateway of this world at `address`.
  pub(crate) fn vars(&self, address: &str) -> Vec<(&'static str, String)> {
    vec![
      ("GATEWAY_ADDRESS", address.to_owned()),
      ("GATEWAY_CERTIFICATE", self.path("gateway.pem")),
      ("GATEWAY_KEY", self.path("gateway.key")),
      ("GATEWAY_LINK_CERTIFICATE", self.path("link.pem")),
      ("GATEWAY_LINK_KEY", self.path("link.key")),
      ("GATEWAY_AUTHORITY", self.path("authority.pem")),
      ("REDIS_URL", format!("redis://{USER}:any@127.0.0.1:{}", self.port)),
    ]
  }

  /// Gives the user of Redis of the gateway the keys that the rule of ACL `keys` names, and each
  /// command but those that the rule `denied` takes away.
  pub(crate) async fn grant(&self, keys: &str, denied: &str) {
    redis::cmd("ACL")
      .arg(&["SETUSER", USER, "reset", "on", "nopass", keys, "&*", "+@all", denied])
      .query_async::<()>(&mut self.redis.clone())
      .await
      .expect("Redis makes the user");
  }

  /// Runs a gateway of this world until the test ends, and gives its address. A client that
  /// connects before the gateway takes its address sends its first packet again.
  pub(crate) fn gateway(&self) -> SocketAddr {
    let address = free();
    tokio::spawn(run(var(self.vars(&address))));
    address.parse().expect("an address")
  }

  /// Writes the session of a character in a zone, as the login service does, and gives its
  /// token.
  pub(crate) async fn session(&self, account: i64, character: i64, zone: i32) -> Token {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    let token = Token(bytes);
    let fields = [("account", account), ("character", character), ("zone", zone.into())];
    let mut redis = self.redis.clone();
    let () = redis.hset_multiple(keys::session(&token), &fields).await.expect("a session");
    token
  }

  /// Writes the entry of `zone` in the directory, as its writer does.
  pub(crate) async fn entry(&self, zone: i32, address: &str, epoch: i64) {
    let fields = [("address", address.to_owned()), ("epoch", epoch.to_string())];
    let mut redis = self.redis.clone();
    let () = redis.hset_multiple(keys::zone(ZoneId(zone)), &fields).await.expect("an entry");
  }

  /// Says on the channel of the directory that `zone` changed its writer, as a writer does.
  pub(crate) async fn publish(&self, zone: i32) {
    let () = self.redis.clone().publish(keys::DIRECTORY, zone).await.expect("a message");
  }

  /// A zone writer whose certificate the authority signed, and which takes only a gateway whose
  /// certificate the authority signed. Its entry in the directory names it.
  pub(crate) async fn zone(&self, id: i32) -> Zone {
    let file = |name: &str| self.folder.path().join(name);
    let config = mmo_link::server(&file("zone.pem"), &file("zone.key"), &file("authority.pem"));
    let endpoint = Endpoint::server(config.expect("a server"), ([127, 0, 0, 1], 0).into());
    let endpoint = endpoint.expect("an endpoint");
    let zone = Zone { endpoint };
    self.entry(id, &zone.address(), 1).await;
    zone
  }

  /// A client of a gateway at `address`, which trusts the authority of this world, or why the
  /// handshake failed.
  pub(crate) async fn client(&self, address: SocketAddr) -> Result<Peer, String> {
    let mut roots = RootCertStore::empty();
    roots.add(self.authority.clone()).expect("the authority");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
      .with_protocol_versions(&[&rustls::version::TLS13])
      .expect("TLS 1.3")
      .with_root_certificates(roots)
      .with_no_client_auth();
    let quic = QuicClientConfig::try_from(tls).expect("the QUIC of TLS 1.3");
    let mut endpoint = Endpoint::client(([127, 0, 0, 1], 0).into()).expect("an endpoint");
    endpoint.set_default_client_config(ClientConfig::new(Arc::new(quic)));
    let connecting = endpoint.connect(address, "localhost").expect("a connection");
    let handshake = within(connecting).await.expect("the handshake of a client ends in time");
    let connection = handshake.map_err(|fault| fault.to_string())?;
    let opened = within(connection.open_bi()).await.expect("the stream of a client opens in time");
    let (send, recv) = opened.expect("a stream");
    Ok(Peer { _endpoint: Some(endpoint), connection, send, recv, frames: Frames::default() })
  }

  /// A client that greets the gateway at `address` and attaches with `token`.
  pub(crate) async fn attach(&self, address: SocketAddr, token: Token) -> Peer {
    let mut client = self.client(address).await.expect("the gateway is trusted");
    client.greet().await;
    client.tell(&Attach { session: token }).await;
    client
  }

  /// A client that attached with `token`, and the first message that the gateway sends it.
  pub(crate) async fn announced(&self, address: SocketAddr, token: Token) -> (Peer, ToClient) {
    let mut client = self.attach(address, token).await;
    let first = client.message::<ToClient>().await;
    (client, first)
  }

  /// A client that greets the gateway at `address` and attaches with `token`, and that has read
  /// the zone of its session.
  pub(crate) async fn attached(&self, address: SocketAddr, token: Token) -> Peer {
    self.announced(address, token).await.0
  }
}

/// A Redis of one test alone, which ends with the test, with its log and its port.
type Redis = (Child, Lines<BufReader<ChildStdout>>, u16);

/// Starts a [`Redis`]. A port that the system gave can go to another process before Redis takes
/// it, so a Redis that stops before it is ready starts again on another port.
async fn redis_server() -> Redis {
  let mut started = None;
  while started.is_none() {
    started = start_redis().await;
  }
  started.expect("a Redis that is ready")
}

/// A [`Redis`] once it is ready, or `None` when it stopped before.
async fn start_redis() -> Option<Redis> {
  let port = TcpListener::bind("127.0.0.1:0")
    .and_then(|free| free.local_addr())
    .expect("the system gives a free port")
    .port();
  let program = std::env::var("REDIS_SERVER").unwrap_or(String::from("redis-server"));
  let mut process = Command::new(program)
    .args(["--port", &port.to_string(), "--bind", "127.0.0.1", "--save", ""])
    .args(["--appendonly", "no"])
    .stdout(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .expect("REDIS_SERVER, or redis-server of the PATH, starts");
  let mut log = BufReader::new(process.stdout.take().expect("a piped log")).lines();
  loop {
    let line = log.next_line().await.expect("the log of Redis reads");
    if line.as_deref().is_none_or(|line| line.contains("Ready to accept connections")) {
      return line.is_some().then_some((process, log, port));
    }
  }
}

/// A zone writer that a test drives.
pub(crate) struct Zone {
  endpoint: Endpoint,
}

impl Zone {
  /// The address of the zone writer, as its entry in the directory holds it.
  pub(crate) fn address(&self) -> String {
    format!("localhost:{}", self.endpoint.local_addr().expect("an address").port())
  }

  /// Takes the next link of a gateway, and answers its hello with a hello of `version`.
  pub(crate) async fn accept_with(&self, version: u16) -> Peer {
    let incoming = within(self.endpoint.accept()).await.expect("a gateway links in time");
    let connection = within(incoming.expect("a gateway")).await.expect("the handshake ends");
    let connection = connection.expect("a link");
    let opened = within(connection.accept_bi()).await.expect("the stream of a gateway opens");
    let (send, recv) = opened.expect("a stream");
    let mut zone = Peer { _endpoint: None, connection, send, recv, frames: Frames::default() };
    let [low, high] = version.to_le_bytes();
    zone.write(&frame(&[low, high, 0, 0, 0, 0]).expect("a hello")).await;
    assert_eq!(Hello::decode(&zone.read().await).ok(), Some(Hello { build: 0 }));
    zone
  }

  /// Takes the next link of a gateway, whose handshake fails, and gives why.
  pub(crate) async fn refused(&self) -> String {
    let incoming = within(self.endpoint.accept()).await.expect("a gateway links in time");
    let handshake = within(incoming.expect("a gateway")).await.expect("the handshake ends");
    handshake.err().map(|fault| fault.to_string()).expect("a refused handshake")
  }

  /// Takes the next link of a gateway.
  pub(crate) async fn accept(&self) -> Peer {
    self.accept_with(PROTOCOL).await
  }

  pub(crate) fn close(&self) {
    self.endpoint.close(0u32.into(), b"");
  }
}

/// One end of a link of a gateway: a client, or a zone writer.
pub(crate) struct Peer {
  _endpoint: Option<Endpoint>,
  connection: Connection,
  pub(crate) send: SendStream,
  pub(crate) recv: RecvStream,
  frames: Frames,
}

impl Peer {
  pub(crate) async fn write(&mut self, bytes: &[u8]) {
    let written = within(self.send.write_all(bytes)).await.expect("a write ends in time");
    written.expect("a write");
  }

  pub(crate) async fn tell<M: Encode>(&mut self, message: &M) {
    self.write(&frame(&encode(message)).expect("a frame")).await;
  }

  /// The payload of the next frame of the gateway.
  pub(crate) async fn read(&mut self) -> Vec<u8> {
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop().expect("a frame") {
        return payload;
      }
      let read = within(self.recv.read(&mut chunk)).await.expect("a read ends in time");
      let size = read.expect("a read").expect("an open stream");
      self.frames.push(&chunk[..size]);
    }
  }

  pub(crate) async fn message<M: DecodeOwned>(&mut self) -> M {
    decode(&self.read().await).expect("a message of this version")
  }

  /// Writes the hello of this version, and checks the hello of the gateway.
  pub(crate) async fn greet(&mut self) {
    self.write(&frame(&Hello { build: 7 }.encode()).expect("a hello")).await;
    assert_eq!(Hello::decode(&self.read().await).ok(), Some(Hello { build: 0 }));
  }

  pub(crate) fn datagram(&self, bytes: &[u8]) {
    self.connection.send_datagram(bytes.to_vec().into()).expect("a datagram");
  }

  pub(crate) fn send_datagram<M: Encode>(&self, message: &M) {
    self.datagram(&encode(message));
  }

  pub(crate) async fn datagram_of<M: DecodeOwned>(&self) -> M {
    let read = within(self.connection.read_datagram()).await.expect("a datagram comes in time");
    decode(&read.expect("a datagram")).expect("a datagram of this version")
  }

  pub(crate) fn close(&self) {
    self.connection.close(0u32.into(), b"");
  }

  /// The code and the reason with which the gateway closed the link, or the fault of the link.
  pub(crate) async fn closed(&self) -> Result<(u64, String), String> {
    match within(self.connection.closed()).await.expect("the link closes in time") {
      ConnectionError::ApplicationClosed(close) => {
        Ok((close.error_code.into_inner(), String::from_utf8_lossy(&close.reason).into_owned()))
      }
      fault => Err(fault.to_string()),
    }
  }
}

/// The value of each variable that `vars` holds.
pub(crate) fn var(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
  move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
}

/// An address of UDP that no socket holds.
pub(crate) fn free() -> String {
  let free = UdpSocket::bind("127.0.0.1:0").and_then(|free| free.local_addr());
  free.expect("a port").to_string()
}
