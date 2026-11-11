use std::fmt::Display;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use mmo_protocol::{FRAME, Frames, Hello, Position, ZoneId, decode, encode, frame};
use mmo_store::Store;
use quinn::{Connection, Endpoint, Incoming, RecvStream, SendStream, ServerConfig};
use redis::aio::ConnectionManager;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sqlx::PgPool;
use tokio::time::interval;

use crate::{Client, Login, Start, World};

/// The signal on which a copy of the login service stops, as the end of `tokio::signal::ctrl_c`.
pub type Stop = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

/// The pace of the admission of the world, and of the check of the slot of each queued client.
const TICK: Duration = Duration::from_secs(1);

/// The build of the login service in its [`Hello`], for the logs of the operator.
const BUILD: u32 = 0;

/// A fault that stops the login service, or that ends the link of one client.
#[derive(Debug, thiserror::Error)]
pub enum Fault {
  /// The environment lacks a variable of the configuration.
  #[error("the variable {0} is not set")]
  Unset(&'static str),
  /// A variable of the configuration does not hold the value that it names.
  #[error("the variable {name} holds no valid value: {why}")]
  Invalid {
    /// The name of the variable.
    name: &'static str,
    /// Why the value is not valid.
    why: String,
  },
  /// The certificate or the key of TLS cannot be read as PEM.
  #[error("the certificate or the key cannot be read: {0}")]
  Pem(#[from] rustls::pki_types::pem::Error),
  /// TLS refuses the certificate or the key.
  #[error("TLS refuses the certificate or the key: {0}")]
  Tls(#[from] rustls::Error),
  /// The endpoint of QUIC cannot open, or the signal to stop cannot be read.
  #[error(transparent)]
  Io(#[from] io::Error),
  /// A store refuses to open, or fails.
  #[error(transparent)]
  Login(#[from] crate::Error),
  /// The connection of a client ends.
  #[error(transparent)]
  Connection(#[from] quinn::ConnectionError),
  /// The stream of a client cannot be read.
  #[error(transparent)]
  Read(#[from] quinn::ReadError),
  /// The stream of a client cannot be written.
  #[error(transparent)]
  Write(#[from] quinn::WriteError),
  /// A client sends bytes that are no frame or no message of this version of the protocol.
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
}

impl From<mmo_store::Error> for Fault {
  fn from(fault: mmo_store::Error) -> Self {
    Self::Login(fault.into())
  }
}

impl From<redis::RedisError> for Fault {
  fn from(fault: redis::RedisError) -> Self {
    Self::Login(fault.into())
  }
}

/// The configuration of one copy of the login service.
#[derive(Debug, PartialEq)]
struct Config {
  address: SocketAddr,
  certificate: PathBuf,
  key: PathBuf,
  database: String,
  redis: String,
  world: World,
  start: Start,
}

impl Config {
  /// Reads each variable of the configuration with `read`.
  fn read(read: &dyn Fn(&str) -> Option<String>) -> Result<Self, Fault> {
    let text = |name: &'static str| read(name).ok_or(Fault::Unset(name));
    Ok(Self {
      address: parsed("MMO_LOGIN_ADDRESS", &text("MMO_LOGIN_ADDRESS")?)?,
      certificate: text("MMO_LOGIN_CERTIFICATE")?.into(),
      key: text("MMO_LOGIN_KEY")?.into(),
      database: text("DATABASE_URL")?,
      redis: text("REDIS_URL")?,
      world: World {
        id: parsed("MMO_WORLD", &text("MMO_WORLD")?)?,
        cap: parsed("MMO_WORLD_CAP", &text("MMO_WORLD_CAP")?)?,
      },
      start: Start {
        zone: ZoneId(parsed("MMO_START_ZONE", &text("MMO_START_ZONE")?)?),
        position: position("MMO_START_POSITION", &text("MMO_START_POSITION")?)?,
      },
    })
  }

  /// Opens the stores and the endpoint of QUIC.
  async fn open(self) -> Result<(Endpoint, Login), Fault> {
    let chain = CertificateDer::pem_file_iter(&self.certificate)?.collect::<Result<_, _>>()?;
    let key = PrivateKeyDer::from_pem_file(&self.key)?;
    let tls = ServerConfig::with_single_cert(chain, key)?;
    let pool = PgPool::connect(&self.database).await.map_err(mmo_store::Error::from)?;
    let store = Store::open(pool).await?;
    let redis = ConnectionManager::new(redis::Client::open(self.redis)?).await?;
    let endpoint = Endpoint::server(tls, self.address)?;
    Ok((endpoint, Login::new(store, redis, self.world, self.start)))
  }
}

/// The position that `text` gives as `x,y,z` in centimetres.
fn position(name: &'static str, text: &str) -> Result<Position, Fault> {
  let numbers: Vec<i32> = text.split(',').map(|one| parsed(name, one)).collect::<Result<_, _>>()?;
  match numbers[..] {
    [x, y, z] => Ok(Position { x, y, z }),
    _ => Err(Fault::Invalid { name, why: "a position is x,y,z in whole centimetres".to_owned() }),
  }
}

fn parsed<T: FromStr<Err: Display>>(name: &'static str, text: &str) -> Result<T, Fault> {
  text.parse().map_err(|why: T::Err| Fault::Invalid { name, why: why.to_string() })
}

/// Runs one copy of the login service, as the environment that `read` gives configures it, until
/// `stop` ends.
///
/// - `MMO_LOGIN_ADDRESS`: the address of UDP of the endpoint of QUIC, as `0.0.0.0:4433`.
/// - `MMO_LOGIN_CERTIFICATE` and `MMO_LOGIN_KEY`: the files of PEM of the chain of the
///   certificate of TLS and of its key.
/// - `DATABASE_URL` and `REDIS_URL`: the stores of the game. The keeper migrates the database.
/// - `MMO_WORLD` and `MMO_WORLD_CAP`: the id of the world and the most accounts that it admits.
/// - `MMO_START_ZONE` and `MMO_START_POSITION`: the zone, and the point of the zone as `x,y,z` in
///   centimetres, where each new character starts.
///
/// Each client opens one stream, sends its [`Hello`] in a frame, then each [`ToLogin`] in a frame.
/// The copy answers with its hello, then with a `FromLogin` for each message, and with the
/// characters of a queued account as soon as the admission gives it a slot. Each second, the copy
/// runs the admission of the world.
pub async fn run(read: &dyn Fn(&str) -> Option<String>, stop: Stop) -> Result<(), Fault> {
  let (endpoint, login) = Config::read(read)?.open().await?;
  serve(endpoint.clone(), login, stop).await?;
  endpoint.close(0_u32.into(), b"the login service stops");
  endpoint.wait_idle().await;
  Ok(())
}

async fn serve(endpoint: Endpoint, login: Login, mut stop: Stop) -> Result<(), Fault> {
  let admission = tokio::spawn(admit(login.clone()));
  let stopped = loop {
    tokio::select! {
      Some(incoming) = endpoint.accept() => {
        tokio::spawn(link(incoming, login.clone()));
      }
      stopped = &mut stop => break stopped,
    }
  };
  admission.abort();
  Ok(stopped?)
}

async fn admit(login: Login) {
  let mut each = interval(TICK);
  loop {
    each.tick().await;
    if let Err(fault) = login.admit().await {
      eprintln!("the admission of the world fails, and runs again in 1 s: {fault}");
    }
  }
}

async fn link(incoming: Incoming, login: Login) {
  if let Err(fault) = talk(incoming, &login).await {
    eprintln!("the link of a client ends: {fault}");
  }
}

/// Serves one client until it closes its stream.
async fn talk(incoming: Incoming, login: &Login) -> Result<(), Fault> {
  let connection = incoming.await?;
  let (mut send, mut recv) = connection.accept_bi().await?;
  let mut frames = Frames::default();
  let Some(theirs) = next(&mut recv, &mut frames).await? else {
    ended(&connection, send).await;
    return Ok(());
  };
  send.write_all(&hello()).await?;
  if let Err(fault) = Hello::decode(&theirs) {
    connection.closed().await;
    return Err(fault.into());
  }
  let from = connection.remote_address().ip();
  let mut client = Client::default();
  let mut each = interval(TICK);
  loop {
    let answer = tokio::select! {
      payload = next(&mut recv, &mut frames) => {
        let Some(payload) = payload? else {
          ended(&connection, send).await;
          return Ok(());
        };
        Some(login.answer(&mut client, from, decode(&payload)?).await?)
      }
      _ = each.tick() => login.ready(&mut client).await?,
    };
    if let Some(answer) = answer {
      send.write_all(&frame(&encode(&answer))?).await?;
    }
  }
}

/// Ends the stream of the login service after its last answer, and waits until the client closes
/// the link, so the answer reaches it.
async fn ended(connection: &Connection, send: SendStream) {
  drop(send);
  connection.closed().await;
}

/// The hello of the login service in its frame. A client of another version reads it, and closes
/// the link.
#[expect(clippy::expect_used, reason = "a hello of 6 bytes fits a frame")]
fn hello() -> Vec<u8> {
  frame(&Hello { build: BUILD }.encode()).expect("a hello of 6 bytes fits a frame")
}

/// The payload of the next frame of `recv`, or `None` when the client closed the stream.
async fn next(recv: &mut RecvStream, frames: &mut Frames) -> Result<Option<Vec<u8>>, Fault> {
  loop {
    if let Some(payload) = frames.pop()? {
      return Ok(Some(payload));
    }
    match recv.read_chunk(FRAME, true).await? {
      Some(chunk) => frames.push(&chunk.bytes),
      None => return Ok(None),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;
  use std::io;
  use std::net::SocketAddr;
  use std::path::PathBuf;
  use std::sync::Arc;

  use mmo_protocol::{
    Character, FRAME, Frames, FromLogin, Hello, PROTOCOL, ToLogin, ZoneId, decode, encode, frame,
  };
  use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
  use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
  use sqlx::PgPool;

  use super::{Config, Fault, run, serve};
  use crate::game::{Game, MEADOW, START, login};
  use crate::{Login, World};

  /// A certificate of TLS for `localhost`, and its files of PEM in a folder of the test alone.
  struct Tls {
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    folder: PathBuf,
  }

  impl Tls {
    fn new() -> Self {
      let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("a key");
      let mut tag = [0_u8; 8];
      getrandom::fill(&mut tag).expect("random bytes");
      let folder = std::env::temp_dir().join(format!("mmo-login-{}", u64::from_le_bytes(tag)));
      std::fs::create_dir_all(&folder).expect("a folder of the test");
      std::fs::write(folder.join("certificate.pem"), made.cert.pem()).expect("a file");
      std::fs::write(folder.join("key.pem"), made.signing_key.serialize_pem()).expect("a file");
      let key = PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()).into();
      Self { certificate: made.cert.der().clone(), key, folder }
    }

    fn file(&self, name: &str) -> String {
      self.folder.join(name).to_string_lossy().into_owned()
    }

    /// The endpoint of a copy that serves with `login` until the test ends.
    fn serve(&self, login: Login) -> SocketAddr {
      let tls =
        ServerConfig::with_single_cert(vec![self.certificate.clone()], self.key.clone_key());
      let endpoint =
        Endpoint::server(tls.expect("a server"), "127.0.0.1:0".parse().expect("an address"));
      let endpoint = endpoint.expect("an endpoint");
      let address = endpoint.local_addr().expect("a bound address");
      tokio::spawn(serve(endpoint, login, Box::pin(std::future::pending())));
      address
    }
  }

  impl Drop for Tls {
    fn drop(&mut self) {
      // A folder that stays in the temporary folder of the system harms no later test.
      drop(std::fs::remove_dir_all(&self.folder));
    }
  }

  /// A client of the login service, which trusts `trusted`.
  struct Peer {
    _endpoint: Endpoint,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    frames: Frames,
  }

  impl Peer {
    async fn connect(
      address: SocketAddr,
      trusted: &[CertificateDer<'static>],
    ) -> Result<Self, String> {
      let mut roots = rustls::RootCertStore::empty();
      roots.add_parsable_certificates(trusted.iter().cloned());
      let config = ClientConfig::with_root_certificates(Arc::new(roots)).expect("a client");
      let mut endpoint = Endpoint::client("127.0.0.1:0".parse().expect("an address")).expect("UDP");
      endpoint.set_default_client_config(config);
      let connecting = endpoint.connect(address, "localhost").expect("a connection starts");
      let connection = connecting.await.map_err(|fault| fault.to_string())?;
      let (send, recv) = connection.open_bi().await.expect("a stream");
      Ok(Self { _endpoint: endpoint, connection, send, recv, frames: Frames::default() })
    }

    async fn new(address: SocketAddr, tls: &Tls) -> Self {
      Self::connect(address, std::slice::from_ref(&tls.certificate))
        .await
        .expect("the server is trusted")
    }

    async fn send(&mut self, bytes: &[u8]) {
      self.send.write_all(bytes).await.expect("the stream takes the bytes");
    }

    /// The payload of the next frame of the server, `None` at the end of its stream, or the fault
    /// of the stream.
    async fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
      loop {
        if let Some(payload) = self.frames.pop().expect("a frame") {
          return Ok(Some(payload));
        }
        match self.recv.read_chunk(FRAME, true).await.map_err(|fault| fault.to_string())? {
          Some(chunk) => self.frames.push(&chunk.bytes),
          None => return Ok(None),
        }
      }
    }

    async fn hello(&mut self, hello: [u8; 6]) -> Hello {
      self.send(&frame(&hello).expect("a frame")).await;
      let theirs = self.read().await.expect("a frame").expect("a hello");
      Hello::decode(&theirs).expect("a hello of this version")
    }

    async fn ask(&mut self, message: &ToLogin) -> FromLogin {
      self.send(&frame(&encode(message)).expect("a frame")).await;
      self.answer().await
    }

    async fn answer(&mut self) -> FromLogin {
      decode(&self.read().await.expect("a frame").expect("an answer")).expect("a message")
    }
  }

  const MINE: [u8; 6] = {
    let [low, high] = PROTOCOL.to_le_bytes();
    [low, high, 7, 0, 0, 0]
  };

  #[sqlx::test(migrations = false)]
  async fn a_client_waits_for_its_slot_and_gets_a_session_over_quic(
    pool: PgPool,
  ) -> Result<(), Fault> {
    let game = Game::new(pool).await;
    let ada = game.account("ada").await;
    let brom = game.store.create_character(ada, "Brom", ZoneId(7), START).await?;
    let tls = Tls::new();
    let address = tls.serve(game.copy(1));
    let mut peer = Peer::new(address, &tls).await;
    assert_eq!(peer.hello(MINE).await, Hello { build: 0 });
    let FromLogin::Queued { position, length, .. } = peer.ask(&login("ada")).await else {
      panic!("a place in the queue");
    };
    assert_eq!((position, length), (0, 1));
    let characters = vec![Character { id: brom, name: "Brom".to_owned() }];
    assert_eq!(peer.answer().await, FromLogin::Characters(characters));
    let session = peer.ask(&ToLogin::Select { character: brom }).await;
    assert!(matches!(session, FromLogin::Session { .. }), "a session, and not {session:?}");
    peer.send.finish().expect("the stream ends");
    assert_eq!(peer.read().await, Ok(None));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_client_that_breaks_the_protocol_loses_its_link(pool: PgPool) -> Result<(), Fault> {
    let game = Game::new(pool).await;
    game.account("ada").await;
    let tls = Tls::new();
    let address = tls.serve(game.copy(1));
    let mut outcomes = Vec::new();
    outcomes.push(Peer::connect(address, &[Tls::new().certificate.clone()]).await.err());
    let silent = Peer::new(address, &tls).await;
    silent.connection.close(0_u32.into(), b"");
    let mut quiet = Peer::new(address, &tls).await;
    quiet.send.finish().expect("the stream ends");
    outcomes.push(format!("{:?}", quiet.read().await).into());
    let mut other = Peer::new(address, &tls).await;
    let [low, high] = (PROTOCOL + 1).to_le_bytes();
    other.send(&frame(&[low, high, 7, 0, 0, 0]).expect("a frame")).await;
    let theirs = other.read().await.expect("a frame").expect("a hello");
    outcomes.push(format!("{:?}", Hello::decode(&theirs)).into());
    other.connection.close(0_u32.into(), b"another version");
    let mut large = Peer::new(address, &tls).await;
    large.send(&[0x81, 0x80, 0x04]).await;
    outcomes.push(large.read().await.err());
    let mut garbled = Peer::new(address, &tls).await;
    garbled.hello(MINE).await;
    garbled.send(&frame(&[0xff, 0xff, 0xff]).expect("a frame")).await;
    outcomes.push(garbled.read().await.err());
    let mut reset = Peer::new(address, &tls).await;
    reset.hello(MINE).await;
    reset.send.reset(0_u32.into()).expect("the stream resets");
    outcomes.push(reset.read().await.err());
    let mut early = Peer::new(address, &tls).await;
    early.recv.stop(0_u32.into()).expect("the stream stops");
    early.send(&frame(&MINE).expect("a frame")).await;
    outcomes.push(Some(early.connection.closed().await.to_string()));
    let mut deaf = Peer::new(address, &tls).await;
    deaf.hello(MINE).await;
    deaf.recv.stop(0_u32.into()).expect("the stream stops");
    deaf.send(&frame(&encode(&login("ada"))).expect("a frame")).await;
    outcomes.push(Some(deaf.connection.closed().await.to_string()));
    let lost = Some("connection lost".to_owned());
    let refused =
      "the cryptographic handshake failed: error 51: invalid peer certificate: BadSignature";
    assert_eq!(
      outcomes,
      [
        Some(refused.to_owned()),
        Some("Ok(None)".to_owned()),
        Some("Ok(Hello { build: 0 })".to_owned()),
        lost.clone(),
        lost.clone(),
        lost,
        Some("closed by peer: 0".to_owned()),
        Some("closed by peer: 0".to_owned()),
      ]
    );
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_fault_of_a_store_ends_the_link_of_its_client(pool: PgPool) -> Result<(), Fault> {
    let game = Game::new(pool.clone()).await;
    let ada = game.account("ada").await;
    sqlx::query(
      "INSERT INTO characters (account_id, name, zone_id, x, y, z, yaw, hit_points) \
       SELECT $1, lpad(n::text, 40, '0'), 7, 0, 0, 0, 0, 100 FROM generate_series(1, 2000) n",
    )
    .bind(ada.0)
    .execute(&pool)
    .await
    .map_err(mmo_store::Error::from)?;
    let tls = Tls::new();
    let mut denied = Peer::new(tls.serve(game.denied("-@scripting").await), &tls).await;
    denied.hello(MINE).await;
    denied.send(&frame(&encode(&login("ada"))).expect("a frame")).await;
    let mut queued = Peer::new(tls.serve(game.denied("+@all").await), &tls).await;
    queued.hello(MINE).await;
    let place = queued.ask(&login("ada")).await;
    game.deny("denied+@all", "-exists").await;
    let mut listed = Peer::new(tls.serve(game.copy(1)), &tls).await;
    listed.hello(MINE).await;
    listed.send(&frame(&encode(&login("ada"))).expect("a frame")).await;
    let mut ends = Vec::new();
    for peer in [&mut denied, &mut queued, &mut listed] {
      let mut end = peer.read().await;
      while let Ok(Some(_)) = end {
        end = peer.read().await;
      }
      ends.push(end);
    }
    assert!(matches!(place, FromLogin::Queued { position: 0, length: 1, .. }), "{place:?}");
    assert_eq!(
      ends,
      [
        Err("connection lost".to_owned()),
        Err("connection lost".to_owned()),
        Err("connection lost".to_owned())
      ]
    );
    Ok(())
  }

  fn variables(tls: &Tls, changed: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
    let database =
      std::env::var("DATABASE_URL").expect("DATABASE_URL names the PostgreSQL of the tests");
    let mut held = HashMap::from([
      ("MMO_LOGIN_ADDRESS", "127.0.0.1:0".to_owned()),
      ("MMO_LOGIN_CERTIFICATE", tls.file("certificate.pem")),
      ("MMO_LOGIN_KEY", tls.file("key.pem")),
      ("DATABASE_URL", database),
      ("REDIS_URL", "redis://127.0.0.1:1".to_owned()),
      ("MMO_WORLD", "1".to_owned()),
      ("MMO_WORLD_CAP", "5000".to_owned()),
      ("MMO_START_ZONE", "7".to_owned()),
      ("MMO_START_POSITION", "1200,-300,50".to_owned()),
    ]);
    for (name, value) in changed {
      held.insert(name, (*value).to_owned());
    }
    held
  }

  #[test]
  fn the_configuration_names_each_variable_that_it_lacks_or_refuses() {
    let tls = Tls::new();
    let full = variables(&tls, &[]);
    let read = Config::read(&|name| full.get(name).cloned());
    assert_eq!(
      read.ok(),
      Some(Config {
        address: "127.0.0.1:0".parse().expect("an address"),
        certificate: tls.folder.join("certificate.pem"),
        key: tls.folder.join("key.pem"),
        database: full["DATABASE_URL"].clone(),
        redis: "redis://127.0.0.1:1".to_owned(),
        world: World { id: 1, cap: 5000 },
        start: MEADOW,
      })
    );
    let mut refusals = Vec::new();
    for name in full.keys() {
      let read = Config::read(&|wanted| (wanted != *name).then(|| full[wanted].clone()));
      refusals.push(read.err().map(|fault| fault.to_string()));
    }
    let wrongs = [
      ("MMO_LOGIN_ADDRESS", "-1"),
      ("MMO_WORLD", "-1"),
      ("MMO_WORLD_CAP", "-1"),
      ("MMO_START_ZONE", "z"),
      ("MMO_START_POSITION", "1,2"),
      ("MMO_START_POSITION", "1,2,z"),
    ];
    for (name, value) in wrongs {
      let wrong = variables(&tls, &[(name, value)]);
      refusals.push(
        Config::read(&|wanted| wrong.get(wanted).cloned()).err().map(|fault| fault.to_string()),
      );
    }
    refusals.sort();
    assert_eq!(
      refusals,
      [
        "the variable DATABASE_URL is not set",
        "the variable MMO_LOGIN_ADDRESS holds no valid value: invalid socket address syntax",
        "the variable MMO_LOGIN_ADDRESS is not set",
        "the variable MMO_LOGIN_CERTIFICATE is not set",
        "the variable MMO_LOGIN_KEY is not set",
        "the variable MMO_START_POSITION holds no valid value: a position is x,y,z in whole centimetres",
        "the variable MMO_START_POSITION holds no valid value: invalid digit found in string",
        "the variable MMO_START_POSITION is not set",
        "the variable MMO_START_ZONE holds no valid value: invalid digit found in string",
        "the variable MMO_START_ZONE is not set",
        "the variable MMO_WORLD holds no valid value: invalid digit found in string",
        "the variable MMO_WORLD is not set",
        "the variable MMO_WORLD_CAP holds no valid value: invalid digit found in string",
        "the variable MMO_WORLD_CAP is not set",
        "the variable REDIS_URL is not set",
      ]
      .map(|text| Some(text.to_owned()))
    );
  }

  #[sqlx::test(migrations = false)]
  async fn a_copy_runs_until_its_signal_and_names_what_keeps_it_from_its_start(
    pool: PgPool,
  ) -> Result<(), Fault> {
    let game = Game::new(pool).await;
    let tls = Tls::new();
    std::fs::write(tls.folder.join("empty.pem"), "").expect("a file");
    std::fs::write(
      tls.folder.join("broken.pem"),
      "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n",
    )
    .expect("a file");
    let taken = tls.serve(game.copy(1)).to_string();
    let redis = game.redis_url();
    let migrated = game.database.clone();
    let (scheme, place) = game.database.split_once("://").expect("a URL");
    let nowhere = format!("{scheme}://nobody@{}", place.rsplit_once('@').map_or(place, |at| at.1));
    let cases: [&[(&'static str, &str)]; 10] = [
      &[("MMO_LOGIN_CERTIFICATE", "nowhere.pem")],
      &[("MMO_LOGIN_CERTIFICATE", &tls.file("broken.pem"))],
      &[("MMO_LOGIN_KEY", "nowhere.pem")],
      &[("MMO_LOGIN_CERTIFICATE", &tls.file("empty.pem"))],
      &[("DATABASE_URL", &nowhere)],
      &[],
      &[("DATABASE_URL", &migrated), ("REDIS_URL", "nope")],
      &[("DATABASE_URL", &migrated)],
      &[("DATABASE_URL", &migrated), ("REDIS_URL", &redis), ("MMO_LOGIN_ADDRESS", &taken)],
      &[("DATABASE_URL", &migrated), ("REDIS_URL", &redis)],
    ];
    let mut outcomes = Vec::new();
    for case in cases {
      let held = variables(&tls, case);
      let ran = run(
        &|name| held.get(name).cloned(),
        Box::pin(std::future::ready(Err(io::Error::other("no signal")))),
      )
      .await;
      outcomes.push(ran.err().map(|fault| fault.to_string()));
    }
    let held = variables(&tls, &[("DATABASE_URL", &migrated), ("REDIS_URL", &redis)]);
    let stopped = run(&|name| held.get(name).cloned(), Box::pin(std::future::ready(Ok(())))).await;
    assert_eq!(stopped.ok(), Some(()));
    let unset = run(&|_| None, Box::pin(std::future::ready(Ok(())))).await;
    let unset = unset.err().map(|fault| fault.to_string());
    assert_eq!(unset.as_deref(), Some("the variable MMO_LOGIN_ADDRESS is not set"));
    let missing = "the certificate or the key cannot be read: I/O error: ";
    let expected = [
      Some(missing.to_owned()),
      Some(
        "the certificate or the key cannot be read: base64 decode error: InvalidCharacter(33)"
          .to_owned(),
      ),
      Some(missing.to_owned()),
      Some("TLS refuses the certificate or the key: peer sent no certificates".to_owned()),
      Some("error returned from database: ".to_owned()),
      Some("the database lacks the migration 1, which the keeper runs".to_owned()),
      Some("Redis URL did not parse - InvalidClientConfig".to_owned()),
      Some(String::new()),
      Some(String::new()),
      Some("no signal".to_owned()),
    ];
    let cut: Vec<_> = outcomes
      .iter()
      .zip(&expected)
      .map(|(text, prefix)| {
        text
          .as_ref()
          .map(|text| text.chars().take(prefix.as_ref().map_or(0, String::len)).collect::<String>())
      })
      .collect();
    assert_eq!(cut, expected);

    Ok(())
  }
}
