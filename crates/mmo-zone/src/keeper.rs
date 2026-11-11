//! The link of the zone writer to the keeper, the one writer of the game data: the writer claims
//! the epoch of its zone on it, and sends its save batches on it.

use std::io;

use mmo_protocol::{
  Batch, Epoch, Frames, FromKeeper, Hello, KeeperRefusal, ToKeeper, ZoneId, decode, encode, frame,
};
use quinn::{
  ClientConfig, ConnectError, Connection, ConnectionError, Endpoint, ReadError, RecvStream,
  SendStream, WriteError,
};
use tokio::net::lookup_host;

use crate::link::BUILD;

/// Why the keeper takes no message of the zone writer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error("the zone writer cannot find the keeper at {address}: {source}")]
  Resolve { address: String, source: io::Error },
  #[error("the zone writer finds no address of the keeper at {0} that its endpoint can dial")]
  Unresolved(String),
  #[error("the zone writer cannot dial the keeper: {0}")]
  Dial(#[from] ConnectError),
  #[error("the link to the keeper fails: {0}")]
  Link(#[from] ConnectionError),
  #[error("the stream of the keeper fails: {0}")]
  Read(#[from] ReadError),
  #[error("the zone writer cannot write to the keeper: {0}")]
  Write(#[from] WriteError),
  #[error("the keeper ended its stream")]
  Ended,
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
  #[error("the keeper gives {0:?}, which does not answer the message of the zone writer")]
  Answer(FromKeeper),
  #[error("the keeper holds no zone of the id that the zone writer claims")]
  NoZone,
  #[error("the keeper holds the epoch {} of the zone, which is newer than the epoch of the batch", held.0)]
  Stale { held: Epoch },
  #[error("the keeper cannot write in PostgreSQL now")]
  Unavailable,
}

impl From<KeeperRefusal> for Error {
  fn from(refusal: KeeperRefusal) -> Self {
    match refusal {
      KeeperRefusal::NoZone => Self::NoZone,
      KeeperRefusal::Stale { held } => Self::Stale { held },
      KeeperRefusal::Unavailable => Self::Unavailable,
    }
  }
}

/// The link of the zone writer to the keeper at `address`, as `host:port`. It dials the keeper at
/// the first message, and again at the next message after a fault ends the link, so a keeper that
/// stops and comes back takes the next batch.
pub(crate) struct Keeper {
  endpoint: Endpoint,
  client: ClientConfig,
  address: String,
  /// True when the endpoint binds an address of IPv4, which dials only an address of IPv4.
  ipv4: bool,
  stream: Option<Stream>,
}

impl Keeper {
  /// The link to the keeper at `address`, which `endpoint` dials with `client`. The name of the
  /// host of `address` is the name that the certificate of the keeper must hold.
  pub(crate) fn new(endpoint: Endpoint, client: ClientConfig, address: String, ipv4: bool) -> Self {
    Self { endpoint, client, address, ipv4, stream: None }
  }

  /// Asks the keeper for the next epoch of `zone`, which it writes in PostgreSQL.
  pub(crate) async fn claim(&mut self, zone: ZoneId) -> Result<Epoch, Error> {
    match self.ask(&ToKeeper::Claim { zone }).await? {
      FromKeeper::Claimed { epoch } => Ok(epoch),
      answer => Err(Error::Answer(answer)),
    }
  }

  /// Sends `batch` to the keeper, and gives the count of the characters that it wrote.
  pub(crate) async fn save(&mut self, batch: Batch) -> Result<u64, Error> {
    match self.ask(&ToKeeper::Save(batch)).await? {
      FromKeeper::Saved { written } => Ok(written),
      answer => Err(Error::Answer(answer)),
    }
  }

  /// Sends `message` and gives the answer of the keeper. A fault of the link drops it.
  async fn ask(&mut self, message: &ToKeeper) -> Result<FromKeeper, Error> {
    let mut stream = match self.stream.take() {
      Some(stream) => stream,
      None => self.dial().await?,
    };
    let answer = decode(&stream.exchange(&encode(message)).await?)?;
    self.stream = Some(stream);
    match answer {
      FromKeeper::Refused(refusal) => Err(refusal.into()),
      answer => Ok(answer),
    }
  }

  /// Opens a link and its stream to the keeper. Each side writes its hello first.
  async fn dial(&self) -> Result<Stream, Error> {
    let address = &self.address;
    let host = address.rsplit_once(':').map_or(address.as_str(), |(host, _)| host);
    let found = lookup_host(address)
      .await
      .map_err(|source| Error::Resolve { address: address.clone(), source })?
      .find(|found| found.is_ipv4() == self.ipv4)
      .ok_or_else(|| Error::Unresolved(address.clone()))?;
    let link =
      self.endpoint.connect_with(self.client.clone(), found, host.trim_matches(['[', ']']));
    // A failed handshake and a failed stream end the dial alike.
    let opened = match link?.await {
      Ok(connection) => connection.open_bi().await.map(|(send, recv)| (connection, send, recv)),
      Err(fault) => Err(fault),
    };
    let (connection, send, recv) = opened?;
    let mut stream = Stream { _connection: connection, send, recv, frames: Frames::default() };
    Hello::decode(&stream.exchange(&Hello { build: BUILD }.encode()).await?)?;
    Ok(stream)
  }
}

struct Stream {
  /// The link of the stream, which closes when the stream drops.
  _connection: Connection,
  send: SendStream,
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// Writes `payload` in a frame, and gives the payload of the next frame of the keeper.
  #[expect(clippy::expect_used, reason = "a batch holds SAVES saves at most")]
  async fn exchange(&mut self, payload: &[u8]) -> Result<Vec<u8>, Error> {
    let framed = frame(payload).expect("a message of the writer holds less than a frame");
    self.send.write_all(&framed).await?;
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(payload);
      }
      let size = self.recv.read(&mut chunk).await?.ok_or(Error::Ended)?;
      self.frames.push(&chunk[..size]);
    }
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Batch, Epoch, Frames, FromKeeper, Hello, KeeperRefusal, PROTOCOL, ZoneId, encode, frame,
  };
  use quinn::{Connection, Endpoint, RecvStream, SendStream};

  use super::{Error, Keeper};
  use crate::config::Config;
  use crate::peer::{World, var};

  const MEADOW: ZoneId = ZoneId(1);

  /// The side of the keeper on a link, which the test drives.
  struct Side {
    _connection: Connection,
    send: SendStream,
    recv: RecvStream,
    frames: Frames,
  }

  impl Side {
    /// Takes the next link of a writer, and its stream, which opens with the hello of the writer.
    async fn accept(keeper: &Endpoint) -> Self {
      let connection = keeper.accept().await.expect("a writer").await.expect("a link");
      let (send, recv) = connection.accept_bi().await.expect("a stream");
      Self { _connection: connection, send, recv, frames: Frames::default() }
    }

    async fn write(&mut self, payload: &[u8]) {
      self.send.write_all(&frame(payload).expect("a frame")).await.expect("a write");
    }

    /// The payload of the next frame of the writer.
    async fn read(&mut self) -> Vec<u8> {
      let mut chunk = [0; 4096];
      loop {
        if let Some(payload) = self.frames.pop().expect("a frame") {
          return payload;
        }
        let size = self.recv.read(&mut chunk).await.expect("a read").expect("an open stream");
        self.frames.push(&chunk[..size]);
      }
    }

    /// Writes the hello of this version and reads the hello of the writer.
    async fn greet(&mut self) {
      self.write(&Hello { build: 0 }.encode()).await;
      self.read().await;
    }

    /// Reads the next message of the writer, and gives it `answer`.
    async fn answer(&mut self, answer: FromKeeper) {
      self.answer_bytes(&encode(&answer)).await;
    }
  }

  /// A writer that dials `address` with the certificate of the zone writer of `world`.
  fn dialing(world: &World, address: &str) -> Keeper {
    let mut vars = world.vars("127.0.0.1:0", "postgres://zone@localhost/game", "nope");
    vars.insert(0, ("ZONE_KEEPER", address.to_owned()));
    let config = Config::read(var(vars)).expect("a config");
    let endpoint = Endpoint::client(config.address).expect("an endpoint");
    Keeper::new(endpoint, config.client, config.keeper, true)
  }

  fn text<T>(found: Result<T, Error>) -> Result<T, String> {
    found.map_err(|error| error.to_string())
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_writer_takes_each_answer_of_the_keeper_on_one_link() {
    let world = World::new();
    let keeper = world.keeper();
    let address = format!("localhost:{}", keeper.local_addr().expect("an address").port());
    let mut writer = dialing(&world, &address);
    let batch = Batch { zone: MEADOW, epoch: Epoch(1), saves: Vec::new() };
    let answers = [
      FromKeeper::Refused(KeeperRefusal::Unavailable),
      FromKeeper::Refused(KeeperRefusal::NoZone),
      FromKeeper::Refused(KeeperRefusal::Stale { held: Epoch(2) }),
      FromKeeper::Claimed { epoch: Epoch(2) },
      FromKeeper::Saved { written: 3 },
      FromKeeper::Saved { written: 3 },
      FromKeeper::Claimed { epoch: Epoch(4) },
    ];
    let keep = async {
      let mut side = Side::accept(&keeper).await;
      side.greet().await;
      for answer in answers {
        side.answer(answer).await;
      }
      side
    };
    let ask = async {
      let mut claims = vec![text(writer.claim(MEADOW).await), text(writer.claim(MEADOW).await)];
      let mut saves = vec![text(writer.save(batch.clone()).await)];
      saves.push(text(writer.save(batch.clone()).await));
      claims.push(text(writer.claim(MEADOW).await));
      saves.push(text(writer.save(batch.clone()).await));
      claims.push(text(writer.claim(MEADOW).await));
      (claims, saves)
    };
    let ((claims, saves), _side) = tokio::join!(ask, keep);
    let stale =
      "the keeper holds the epoch 2 of the zone, which is newer than the epoch of the batch";
    assert_eq!(
      claims,
      [
        Err("the keeper cannot write in PostgreSQL now".to_owned()),
        Err("the keeper holds no zone of the id that the zone writer claims".to_owned()),
        Err(
          "the keeper gives Saved { written: 3 }, which does not answer the message of the zone \
           writer"
            .to_owned()
        ),
        Ok(Epoch(4)),
      ]
    );
    assert_eq!(
      saves,
      [
        Err(stale.to_owned()),
        Err(
          "the keeper gives Claimed { epoch: Epoch(2) }, which does not answer the message of the \
           zone writer"
            .to_owned()
        ),
        Ok(3),
      ]
    );
  }

  /// What the keeper does on a link, which the writer opened with its hello.
  #[derive(Clone, Copy)]
  enum Script {
    Foreign,
    Large,
    Ended,
    Garbled,
    Reset,
    Deaf,
  }

  impl Side {
    async fn play(mut self, script: Script) -> Self {
      let [low, high] = (PROTOCOL + 1).to_le_bytes();
      match script {
        Script::Foreign => self.write(&[low, high, 0, 0, 0, 0]).await,
        Script::Large => self.send.write_all(&[0x81, 0x80, 0x04]).await.expect("a write"),
        Script::Ended => self.send.finish().expect("an open stream"),
        Script::Garbled => {
          self.greet().await;
          self.answer_bytes(&[]).await;
        }
        Script::Reset => {
          self.greet().await;
          self.read().await;
          self.send.reset(7u32.into()).expect("an open stream");
        }
        Script::Deaf => {
          self.recv.stop(7u32.into()).expect("an open stream");
          self.write(&Hello { build: 0 }.encode()).await;
        }
      }
      self
    }

    /// Reads the next message of the writer, and gives it `payload` as its answer.
    async fn answer_bytes(&mut self, payload: &[u8]) {
      self.read().await;
      self.write(payload).await;
    }
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn each_fault_of_a_stream_ends_its_link_and_the_next_message_dials_again() {
    let world = World::new();
    let keeper = world.keeper();
    let address = format!("localhost:{}", keeper.local_addr().expect("an address").port());
    let mut writer = dialing(&world, &address);
    let mut faults = Vec::new();
    let scripts =
      [Script::Foreign, Script::Large, Script::Ended, Script::Garbled, Script::Reset, Script::Deaf];
    for script in scripts {
      let side = async { Side::accept(&keeper).await.play(script).await };
      faults.push(text(tokio::join!(writer.claim(MEADOW), side).0));
    }
    let foreign = PROTOCOL + 1;
    assert_eq!(
      faults,
      [
        Err(format!(
          "the peer speaks version {foreign} of the protocol, and this side speaks version {PROTOCOL}"
        )),
        Err("a frame holds 65536 bytes at most".to_owned()),
        Err("the keeper ended its stream".to_owned()),
        Err("the bytes are not a message of this version of the protocol".to_owned()),
        Err("the stream of the keeper fails: stream reset by peer: error 7".to_owned()),
        Err(
          "the zone writer cannot write to the keeper: sending stopped by peer: error 7".to_owned()
        ),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_writer_names_each_keeper_that_it_cannot_dial() {
    let world = World::new();
    let keeper = world.keeper();
    let address = format!("localhost:{}", keeper.local_addr().expect("an address").port());
    let refuse = async { keeper.accept().await.expect("a writer").refuse() };
    let mut writer = dialing(&world, &address);
    let mut faults = vec![text(tokio::join!(writer.claim(MEADOW), refuse).0)];
    for address in ["127.0.0.1:0", "nope", "[::1]:4433"] {
      faults.push(text(dialing(&world, address).claim(MEADOW).await));
    }
    assert_eq!(
      faults,
      [
        Err(
          "the link to the keeper fails: aborted by peer: the server refused to accept a new \
           connection"
            .to_owned()
        ),
        Err(
          "the zone writer cannot dial the keeper: invalid remote address: 127.0.0.1:0".to_owned()
        ),
        Err("the zone writer cannot find the keeper at nope: invalid socket address".to_owned()),
        Err(
          "the zone writer finds no address of the keeper at [::1]:4433 that its endpoint can \
           dial"
            .to_owned()
        ),
      ]
    );
  }
}
