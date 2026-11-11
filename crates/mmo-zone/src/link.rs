//! The links of the gateways, which relay the messages of the players to the zone writer, and its
//! snapshots to the players.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

use mmo_protocol::{
  CharacterId, Frames, FromClient, Hello, Intents, Link, Relayed, ToZone, decode, encode, frame,
};
use mmo_store::{Saved, Store};
use quinn::{
  Connection, ConnectionError, Endpoint, Incoming, ReadError, RecvStream, VarInt, WriteError,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time;

use crate::Error;
use crate::zone::{Gateway, Refusal, Zone};

/// The time of one tick: the zone runs 20 ticks each second, as the step of `mmo-collide` does.
const TICK: Duration = Duration::from_millis(50);

/// The build in the hello of the zone writer: the version of the workspace, as 1002003 for 1.2.3.
pub(crate) const BUILD: u32 = {
  let parts = [
    env!("CARGO_PKG_VERSION_MAJOR"),
    env!("CARGO_PKG_VERSION_MINOR"),
    env!("CARGO_PKG_VERSION_PATCH"),
  ];
  let mut build = 0;
  let mut at = 0;
  while at < parts.len() {
    let Ok(part) = u32::from_str_radix(parts[at], 10) else {
      panic!("cargo gives each part of the version as a number");
    };
    build = build * 1000 + part;
    at += 1;
  }
  build
};

/// The code with which the zone writer closes the link of a gateway after a fault. The reason of
/// the close holds the text of the fault.
const FAULT: VarInt = VarInt::from_u32(1);

/// Why the zone writer ends the link of a gateway.
#[derive(Debug, thiserror::Error)]
enum Fault {
  #[error("the link fails: {0}")]
  Link(#[from] ConnectionError),
  #[error("the stream of the gateway fails: {0}")]
  Read(#[from] ReadError),
  #[error("the zone writer cannot write to the gateway: {0}")]
  Write(#[from] WriteError),
  #[error("the gateway ended its stream")]
  Ended,
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
}

/// Why the zone writer takes no player for a [`ToZone::Join`].
#[derive(Debug, thiserror::Error)]
enum Refused {
  #[error("the database cannot load the character: {0}")]
  Store(#[from] mmo_store::Error),
  #[error("no character has this id")]
  NoCharacter,
  #[error("the link of its gateway closed during the load")]
  Gone,
  #[error(transparent)]
  Zone(#[from] Refusal),
}

/// What the tasks of the links tell the zone writer.
enum Event {
  Opened(Gateway, Connection),
  Message(Gateway, ToZone),
  Intents(Gateway, Relayed<Intents>),
  Loaded { gateway: Gateway, link: Link, found: Result<Vec<Saved>, mmo_store::Error> },
  Closed(Gateway),
}

/// Runs the tick of `zone`, and takes the links of the gateways, each in a task of its own, until
/// the endpoint closes.
pub(crate) async fn serve(endpoint: Endpoint, zone: Zone, store: Store) -> Error {
  let (events, mut heard) = unbounded_channel();
  let mut writer = Writer { zone, store, events, gateways: BTreeMap::new() };
  let mut count = 0;
  let mut clock = time::interval(TICK);
  loop {
    tokio::select! {
      incoming = endpoint.accept() => {
        let Some(incoming) = incoming else {
          return Error::Closed;
        };
        count += 1;
        tokio::spawn(gateway(incoming, Gateway(count), writer.events.clone()));
      }
      _ = clock.tick() => writer.tick(),
      Some(event) = heard.recv() => writer.hear(event),
    }
  }
}

/// The zone, and the links that its messages come on.
struct Writer {
  zone: Zone,
  store: Store,
  events: UnboundedSender<Event>,
  gateways: BTreeMap<Gateway, Connection>,
}

impl Writer {
  fn hear(&mut self, event: Event) {
    match event {
      Event::Opened(gateway, connection) => {
        self.gateways.insert(gateway, connection);
      }
      Event::Message(gateway, message) => self.message(gateway, &message),
      Event::Intents(gateway, Relayed { link, message }) => {
        self.zone.intents(gateway, link, &message);
      }
      Event::Loaded { gateway, link, found } => {
        if let Err(why) = self.loaded(gateway, link, found) {
          eprintln!("the zone writer takes no player on the link {}: {why}", link.0);
        }
      }
      Event::Closed(gateway) => self.close(gateway),
    }
  }

  fn message(&mut self, gateway: Gateway, message: &ToZone) {
    match *message {
      ToZone::Join { link, character, .. } => self.load(gateway, link, character),
      ToZone::Client(Relayed { link, message: FromClient::Logout }) => {
        self.zone.logout(gateway, link);
      }
      // The zone writer sends no chat yet, so a line goes nowhere.
      ToZone::Client(Relayed { message: FromClient::Say { .. }, .. }) => {}
      ToZone::Leave { link } => self.zone.leave(gateway, link),
    }
  }

  /// Loads the last save of `character` in a task, so the tick goes on.
  fn load(&self, gateway: Gateway, link: Link, character: CharacterId) {
    let store = self.store.clone();
    let events = self.events.clone();
    tokio::spawn(async move {
      let found = store.load(&[character]).await;
      // The send fails only when the zone writer stopped, and then no player waits for the load.
      drop(events.send(Event::Loaded { gateway, link, found }));
    });
  }

  fn loaded(
    &mut self,
    gateway: Gateway,
    link: Link,
    found: Result<Vec<Saved>, mmo_store::Error>,
  ) -> Result<(), Refused> {
    let saved = found?.into_iter().next().ok_or(Refused::NoCharacter)?;
    if !self.gateways.contains_key(&gateway) {
      return Err(Refused::Gone);
    }
    Ok(self.zone.join(gateway, link, &saved)?)
  }

  fn close(&mut self, gateway: Gateway) {
    self.gateways.remove(&gateway);
    self.zone.close(gateway);
  }

  /// Runs one tick, and sends each snapshot to the gateway of its player. A gateway that a
  /// snapshot cannot reach closes, so its players leave.
  fn tick(&mut self) {
    let snapshots = self.zone.tick();
    let mut lost = Vec::new();
    for (gateway, connection) in &self.gateways {
      for (_, snapshot) in snapshots.iter().filter(|(via, _)| via == gateway) {
        if let Err(error) = connection.send_datagram(encode(snapshot).into()) {
          eprintln!("a snapshot does not reach the gateway {}: {error}", gateway.0);
          lost.push(*gateway);
          break;
        }
      }
    }
    for gateway in lost {
      self.close(gateway);
    }
  }
}

/// Relays the messages of the link of a gateway to the zone writer until a fault ends it.
async fn gateway(incoming: Incoming, gateway: Gateway, events: UnboundedSender<Event>) {
  let fault = match incoming.await {
    Ok(connection) => {
      // Each send of this task fails only when the zone writer stopped, and then no one needs it.
      drop(events.send(Event::Opened(gateway, connection.clone())));
      let Err(fault) = relay(&connection, gateway, &events).await;
      connection.close(FAULT, fault.to_string().as_bytes());
      fault
    }
    Err(error) => Fault::Link(error),
  };
  eprintln!("the link of the gateway {} ends: {fault}", gateway.0);
  drop(events.send(Event::Closed(gateway)));
}

/// Takes the stream that the gateway opens. Each side writes its hello first, then the gateway
/// sends its messages on the stream and the intents of its players in datagrams.
async fn relay(
  connection: &Connection,
  gateway: Gateway,
  events: &UnboundedSender<Event>,
) -> Result<Infallible, Fault> {
  let (mut send, recv) = connection.accept_bi().await?;
  let mut stream = Stream { recv, frames: Frames::default() };
  send.write_all(&framed(&Hello { build: BUILD }.encode())).await?;
  Hello::decode(&stream.next().await?)?;
  loop {
    let event = tokio::select! {
      payload = stream.next() => {
        payload.and_then(|payload| Ok(Event::Message(gateway, decode(&payload)?)))
      }
      datagram = connection.read_datagram() => datagram
        .map_err(Fault::from)
        .and_then(|datagram| Ok(Event::Intents(gateway, decode(&datagram)?))),
    }?;
    drop(events.send(event));
  }
}

struct Stream {
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// The payload of the next frame of the gateway.
  async fn next(&mut self) -> Result<Vec<u8>, Fault> {
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(payload);
      }
      let size = self.recv.read(&mut chunk).await?.ok_or(Fault::Ended)?;
      self.frames.push(&chunk[..size]);
    }
  }
}

#[expect(clippy::expect_used, reason = "a hello holds 6 bytes")]
fn framed(payload: &[u8]) -> Vec<u8> {
  frame(payload).expect("a hello holds less than a frame")
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use mmo_protocol::{Angle, CharacterId, Link, PROTOCOL, Position, ZoneId, frame};
  use mmo_store::{Saved, State, Store};
  use quinn::{ConnectionError, Endpoint, TransportErrorCode};
  use sqlx::PgPool;
  use tokio::sync::mpsc::unbounded_channel;

  use super::{Gateway, Writer, serve};
  use crate::config::Config;
  use crate::peer::{Peer, World, var};
  use crate::zone::Zone;

  fn endpoint(world: &World) -> Endpoint {
    let config = Config::read(var(world.vars("127.0.0.1:0", "postgres://zone@localhost/game")));
    let config = config.expect("a config");
    Endpoint::server(config.server, config.address).expect("an endpoint")
  }

  #[sqlx::test(migrations = false)]
  async fn each_fault_of_a_gateway_ends_its_link_alone_with_its_reason(
    pool: PgPool,
  ) -> Result<(), mmo_store::Error> {
    let store = Store::migrate(pool).await?;
    let world = World::new();
    let endpoint = endpoint(&world);
    let address = endpoint.local_addr().expect("an address");
    let zone = tokio::spawn(serve(endpoint.clone(), Zone::new(ZoneId(1)), store));
    let gateways = world.gateway(true);
    let hello = |version: u16| {
      let [low, high] = version.to_le_bytes();
      frame(&[low, high, 0, 0, 0, 0]).expect("a hello")
    };
    let foreign = PROTOCOL + 1;

    let mut other = Peer::link(&gateways, address).await;
    other.write(&hello(foreign)).await;
    let mut large = Peer::link(&gateways, address).await;
    large.write(&[0x81, 0x80, 0x04]).await;
    let mut wrong = Peer::link(&gateways, address).await;
    wrong.greet().await;
    wrong.write(&frame(&[]).expect("a frame")).await;
    let mut noisy = Peer::link(&gateways, address).await;
    noisy.greet().await;
    noisy.datagram(&[0xff]);
    let mut deaf = Peer::link(&gateways, address).await;
    deaf.recv.stop(7u32.into()).expect("an open stream");
    deaf.write(&hello(PROTOCOL)).await;
    let mut gone = Peer::link(&gateways, address).await;
    gone.greet().await;
    gone.send.reset(7u32.into()).expect("an open stream");
    let mut ended = Peer::link(&gateways, address).await;
    ended.greet().await;
    ended.send.finish().expect("an open stream");
    let mut reasons = Vec::new();
    for peer in [other, large, wrong, noisy, deaf, gone, ended] {
      reasons.push(peer.closed().await);
    }
    let reason = |text: &str| Ok((1, text.to_owned()));
    let malformed = "the bytes are not a message of this version of the protocol";
    let version = format!(
      "the peer speaks version {foreign} of the protocol, and this side speaks version {PROTOCOL}"
    );
    assert_eq!(
      reasons,
      [
        reason(&version),
        reason("a frame holds 65536 bytes at most"),
        reason(malformed),
        reason(malformed),
        reason("the zone writer cannot write to the gateway: sending stopped by peer: error 7"),
        reason("the stream of the gateway fails: stream reset by peer: error 7"),
        reason("the gateway ended its stream"),
      ]
    );

    let quiet = Peer::link(&gateways, address).await;
    quiet.close();
    let stranger = Peer::link(&world.gateway(false), address).await.closed().await;
    assert!(
      matches!(
        stranger,
        Err(ConnectionError::ConnectionClosed(ref close))
          if close.error_code == TransportErrorCode::crypto(116)
      ),
      "{stranger:?}"
    );
    endpoint.close(0u32.into(), b"");
    assert_eq!(zone.await.expect("serve ends").to_string(), "the endpoint of the links closed");
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_load_that_finds_no_player_for_an_open_gateway_joins_no_one(
    pool: PgPool,
  ) -> Result<(), mmo_store::Error> {
    let (events, _heard) = unbounded_channel();
    let store = Store::migrate(pool).await?;
    let mut writer =
      Writer { zone: Zone::new(ZoneId(1)), store, events, gateways: BTreeMap::new() };
    let saved = Saved {
      character: CharacterId(7),
      zone: ZoneId(1),
      state: State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 1 },
      version: 0,
    };
    let behind = mmo_store::Error::Behind { version: 1 };
    let mut load = |found| writer.loaded(Gateway(1), Link(1), found).map_err(|why| why.to_string());
    let refusals = [Err(behind), Ok(vec![]), Ok(vec![saved])].map(&mut load);
    assert_eq!(
      refusals,
      [
        Err(
          "the database cannot load the character: the database lacks the migration 1, which \
             the keeper runs"
            .to_owned()
        ),
        Err("no character has this id".to_owned()),
        Err("the link of its gateway closed during the load".to_owned()),
      ]
    );

    let world = World::new();
    let endpoint = endpoint(&world);
    let gateways = world.gateway(true);
    let peer = Peer::link(&gateways, endpoint.local_addr().expect("an address"));
    let accepted = async { endpoint.accept().await.expect("a gateway").await.expect("a link") };
    let (peer, connection) = tokio::join!(peer, accepted);
    connection.close(0u32.into(), b"");
    writer.gateways.insert(Gateway(1), connection);
    let joined = writer.loaded(Gateway(1), Link(1), Ok(vec![saved]));
    assert!(matches!(joined, Ok(())), "{joined:?}");
    writer.tick();
    assert_eq!((writer.gateways.len(), writer.zone.tick()), (0, vec![]));
    drop(peer);
    Ok(())
  }
}
