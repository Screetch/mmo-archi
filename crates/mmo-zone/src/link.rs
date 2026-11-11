//! The links of the gateways, which relay the messages of the players to the zone writer, and its
//! snapshots to the players.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

use mmo_protocol::{
  AccountId, CharacterId, Frames, FromClient, Hello, Intents, Link, Relayed, ToZone, decode,
  encode, frame,
};
use mmo_store::{Batch, Save, Saved, Store};
use quinn::{
  Connection, ConnectionError, Endpoint, Incoming, ReadError, RecvStream, VarInt, WriteError,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time;

use crate::Error;
use crate::claim::{self, Claim, Entry};
use crate::zone::{Gateway, Leaver, Refusal, TICKS_PER_SECOND, Zone};

/// The time of one tick: the zone runs 20 ticks each second, as the step of `mmo-collide` does.
const TICK: Duration = Duration::from_millis(50);

/// The ticks between two writes of the entry of the zone in the directory: 1 s, which keeps its
/// lease of 3 s.
const ANNOUNCE_EVERY: u32 = TICKS_PER_SECOND;

/// The ticks between two saves of the players, and the presence that each save renews: 10 s. It is
/// the most play that a player loses when a writer stops, as the table of targets says.
const SAVE_EVERY: u32 = 10 * TICKS_PER_SECOND;

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
  Loaded {
    gateway: Gateway,
    link: Link,
    account: AccountId,
    found: Result<Vec<Saved>, mmo_store::Error>,
  },
  Saved {
    batch: Batch,
    result: Result<u64, mmo_store::Error>,
  },
  Entries(Result<(), claim::Error>),
  Closed(Gateway),
}

/// Runs the tick of `zone`, and takes the links of the gateways, each in a task of its own, until
/// the endpoint closes, or a newer writer holds the zone.
pub(crate) async fn serve(endpoint: Endpoint, zone: Zone, store: Store, claim: Claim) -> Error {
  let (events, mut heard) = unbounded_channel();
  let mut writer = Writer::new(zone, store, claim, events);
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
      Some(event) = heard.recv() => {
        if let Some(fault) = writer.hear(event) {
          return fault;
        }
      }
    }
  }
}

/// The zone, and the links that its messages come on.
struct Writer {
  zone: Zone,
  store: Store,
  claim: Claim,
  events: UnboundedSender<Event>,
  gateways: BTreeMap<Gateway, Connection>,
  /// The ticks since the last save, from 0 to `SAVE_EVERY - 1`.
  ticks: u32,
  /// The players that left, and whose last state the store did not acknowledge yet, by character.
  owed: BTreeMap<i64, Leaver>,
}

impl Writer {
  fn new(zone: Zone, store: Store, claim: Claim, events: UnboundedSender<Event>) -> Self {
    Self { zone, store, claim, events, gateways: BTreeMap::new(), ticks: 0, owed: BTreeMap::new() }
  }

  /// Takes an event, and gives the fault that stops the zone writer when the event brings one.
  fn hear(&mut self, event: Event) -> Option<Error> {
    match event {
      Event::Opened(gateway, connection) => {
        self.gateways.insert(gateway, connection);
      }
      Event::Message(gateway, message) => self.message(gateway, &message),
      Event::Intents(gateway, Relayed { link, message }) => {
        self.zone.intents(gateway, link, &message);
      }
      Event::Loaded { gateway, link, account, found } => {
        if let Err(why) = self.loaded(gateway, link, account, found) {
          eprintln!("the zone writer takes no player on the link {}: {why}", link.0);
        }
      }
      Event::Saved { batch, result } => return self.saved(&batch, result),
      Event::Entries(Err(error @ claim::Error::Superseded { .. })) => return Some(error.into()),
      Event::Entries(Err(error)) => eprintln!("the zone writer cannot write in Redis: {error}"),
      Event::Entries(Ok(())) => {}
      Event::Closed(gateway) => self.close(gateway),
    }
    None
  }

  fn message(&mut self, gateway: Gateway, message: &ToZone) {
    match *message {
      ToZone::Join { link, account, character } => self.load(gateway, link, account, character),
      ToZone::Client(Relayed { link, message: FromClient::Logout }) => {
        self.zone.logout(gateway, link);
        self.leave();
      }
      // The zone writer sends no chat yet, so a line goes nowhere.
      ToZone::Client(Relayed { message: FromClient::Say { .. }, .. }) => {}
      ToZone::Leave { link } => self.zone.leave(gateway, link),
    }
  }

  /// Loads the last save of `character` in a task, so the tick goes on.
  fn load(&self, gateway: Gateway, link: Link, account: AccountId, character: CharacterId) {
    let store = self.store.clone();
    let events = self.events.clone();
    tokio::spawn(async move {
      let found = store.load(&[character]).await;
      // The send fails only when the zone writer stopped, and then no player waits for the load.
      drop(events.send(Event::Loaded { gateway, link, account, found }));
    });
  }

  fn loaded(
    &mut self,
    gateway: Gateway,
    link: Link,
    account: AccountId,
    found: Result<Vec<Saved>, mmo_store::Error>,
  ) -> Result<(), Refused> {
    let saved = found?.into_iter().next().ok_or(Refused::NoCharacter)?;
    if !self.gateways.contains_key(&gateway) {
      return Err(Refused::Gone);
    }
    Ok(self.admit(gateway, link, account, &saved)?)
  }

  /// Takes the player of `saved` into the zone, and writes its presence.
  fn admit(
    &mut self,
    gateway: Gateway,
    link: Link,
    account: AccountId,
    saved: &Saved,
  ) -> Result<(), Refusal> {
    self.zone.join(gateway, link, account, saved)?;
    self.write(Entry::Present(vec![(account, saved.character)]));
    Ok(())
  }

  fn close(&mut self, gateway: Gateway) {
    self.gateways.remove(&gateway);
    self.zone.close(gateway);
  }

  /// Writes `entry` in Redis in a task, so the tick goes on.
  fn write(&self, entry: Entry) {
    let claim = self.claim.clone();
    let events = self.events.clone();
    tokio::spawn(async move {
      // The send fails only when the zone writer stopped, and then no one needs the result.
      drop(events.send(Event::Entries(claim.write(entry).await)));
    });
  }

  /// Sends `saves` to the store in a task, so the tick goes on.
  fn send(&self, saves: Vec<Save>) {
    if saves.is_empty() {
      return;
    }
    let store = self.store.clone();
    let events = self.events.clone();
    let batch = self.claim.batch(saves);
    tokio::spawn(async move {
      let result = store.save(&batch).await;
      // The send fails only when the zone writer stopped, and then no one needs the result.
      drop(events.send(Event::Saved { batch, result }));
    });
  }

  /// Sends the last state of each player that left, and ends the presence of a player that the
  /// store holds already.
  fn leave(&mut self) {
    let mut saves = Vec::new();
    let mut absent = Vec::new();
    for leaver in self.zone.leavers() {
      match leaver.save {
        Some(save) => {
          saves.push(save);
          self.owed.insert(save.character.0, leaver);
        }
        None => absent.push(leaver.account),
      }
    }
    self.write(Entry::Absent(absent));
    self.send(saves);
  }

  /// The store takes a batch, or refuses it. A newer writer of the zone ends this one. Any other
  /// refusal changes nothing, so each state that the store does not hold goes again at the next
  /// save, and a player that left keeps its presence until then.
  fn saved(&mut self, batch: &Batch, result: Result<u64, mmo_store::Error>) -> Option<Error> {
    match result {
      Ok(_) => {
        self.zone.saved(&batch.saves);
        let left = batch.saves.iter().filter_map(|save| self.owed.remove(&save.character.0));
        let absent = left.map(|leaver| leaver.account).collect();
        self.write(Entry::Absent(absent));
        None
      }
      Err(error @ mmo_store::Error::Epoch { .. }) => Some(error.into()),
      Err(error) => {
        eprintln!(
          "the store refuses {} saves, and the zone writer sends them again: {error}",
          batch.saves.len()
        );
        None
      }
    }
  }

  /// Sends each state that the store does not hold, and renews the presence of the players that
  /// have a link.
  fn keep(&mut self) {
    let mut saves = self.zone.dirty();
    saves.extend(self.owed.values().filter_map(|leaver| leaver.save));
    self.send(saves);
    self.write(Entry::Present(self.zone.present()));
  }

  /// Runs one tick, and sends each snapshot to the gateway of its player. A gateway that a
  /// snapshot cannot reach closes, so its players leave. Each second it renews the entry of the
  /// zone, and each 10 s it saves.
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
    self.ticks = (self.ticks + 1) % SAVE_EVERY;
    if self.ticks.is_multiple_of(ANNOUNCE_EVERY) {
      self.write(Entry::Zone { players: self.zone.count() });
    }
    if self.ticks == 0 {
      self.keep();
    }
    self.leave();
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

  use mmo_protocol::{
    AccountId, Angle, CharacterId, FromClient, Input, Intents, Link, PROTOCOL, Password, Position,
    Relayed, State, Tick, ToZone, ZoneId, frame,
  };
  use mmo_store::testing::Redis;
  use mmo_store::{Saved, Store};
  use quinn::{ConnectionError, Endpoint, TransportErrorCode};
  use sqlx::PgPool;
  use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

  use super::{Claim, Entry, Event, Gateway, SAVE_EVERY, Writer, claim, serve};
  use crate::config::Config;
  use crate::peer::{Peer, World, hash, var};
  use crate::zone::Zone;

  const MEADOW: ZoneId = ZoneId(1);
  const GATE: Gateway = Gateway(1);
  const START: State =
    State { position: Position { x: 0, y: 0, z: 10_000 }, yaw: Angle(0), hit_points: 100 };

  /// A world of one zone with one character, and its Redis.
  struct Meadow {
    redis: Redis,
    store: Store,
    account: AccountId,
    character: CharacterId,
  }

  impl Meadow {
    async fn new(pool: PgPool) -> Self {
      let store = Store::migrate(pool).await.expect("a migrated database");
      store.add_zone(MEADOW, "meadow").await.expect("a zone");
      let account = store.create_account("alice", Password("one".to_owned())).await.expect("one");
      let character = store.create_character(account, "Brom", MEADOW, START).await.expect("Brom");
      Self { redis: Redis::start().await, store, account, character }
    }

    /// A writer that claims the zone, and the events that its tasks send.
    async fn claim(&self) -> Claim {
      let epoch = self.store.claim(MEADOW).await.expect("an epoch");
      let address = "zone-1:4433".to_owned();
      Claim::open(&self.redis.url(), MEADOW, epoch, address).await.expect("a claim")
    }

    async fn writer(&self) -> (Writer, UnboundedReceiver<Event>) {
      let (events, heard) = unbounded_channel();
      let writer = Writer::new(Zone::new(MEADOW), self.store.clone(), self.claim().await, events);
      (writer, heard)
    }

    /// Takes the character into `writer`, from its last save.
    async fn join(&self, writer: &mut Writer) -> Saved {
      let saved = self.store.load(&[self.character]).await.expect("a save")[0];
      writer.admit(GATE, Link(1), self.account, &saved).expect("a player");
      saved
    }
  }

  fn endpoint(world: &World) -> Endpoint {
    let vars = world.vars("127.0.0.1:0", "postgres://zone@localhost/game", "redis://localhost");
    let config = Config::read(var(vars)).expect("a config");
    Endpoint::server(config.server, config.address).expect("an endpoint")
  }

  /// Runs ticks of `writer` from `first` to `last`, as the player of link 1 walks east.
  fn walk(writer: &mut Writer, first: u32, last: u32) {
    for tick in first..=last {
      let inputs = [tick, tick + 1, tick + 2].map(|tick| Input {
        tick: Tick(tick),
        heading: Some(Angle(0)),
        view: Angle(0),
      });
      writer.zone.intents(GATE, Link(1), &Intents { zone: MEADOW, ack: None, inputs });
      writer.tick();
    }
  }

  /// Hears the events of `writer` until `done` holds, and gives the fault of the first event that
  /// stops the writer.
  async fn hear(
    writer: &mut Writer,
    heard: &mut UnboundedReceiver<Event>,
    done: impl Fn(&Writer, &Event) -> bool,
  ) -> Option<String> {
    loop {
      let event = heard.recv().await.expect("an event");
      let finished = done(writer, &event);
      if let Some(fault) = writer.hear(event) {
        return Some(fault.to_string());
      }
      if finished {
        return None;
      }
    }
  }

  fn saved_event(_: &Writer, event: &Event) -> bool {
    matches!(event, Event::Saved { .. })
  }

  #[sqlx::test(migrations = false)]
  async fn each_fault_of_a_gateway_ends_its_link_alone_with_its_reason(
    pool: PgPool,
  ) -> Result<(), mmo_store::Error> {
    let meadow = Meadow::new(pool).await;
    let world = World::new();
    let endpoint = endpoint(&world);
    let address = endpoint.local_addr().expect("an address");
    let claim = meadow.claim().await;
    let zone = tokio::spawn(serve(endpoint.clone(), Zone::new(MEADOW), meadow.store, claim));
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
    let meadow = Meadow::new(pool).await;
    let (mut writer, _heard) = meadow.writer().await;
    let saved = Saved { character: meadow.character, zone: MEADOW, state: START, version: 0 };
    let behind = mmo_store::Error::Behind { version: 1 };
    let account = meadow.account;
    let mut load =
      |found| writer.loaded(Gateway(1), Link(1), account, found).map_err(|why| why.to_string());
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
    let joined = writer.loaded(Gateway(1), Link(1), account, Ok(vec![saved]));
    assert!(matches!(joined, Ok(())), "{joined:?}");
    writer.tick();
    assert_eq!((writer.gateways.len(), writer.zone.tick()), (0, vec![]));
    drop(peer);
    Ok(())
  }

  /// Doc line 20: a writer that stops loses 10 s of play at most.
  #[sqlx::test(migrations = false)]
  async fn a_writer_that_stops_loses_at_most_10_s_and_its_successor_starts_from_the_last_save(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    walk(&mut writer, 1, SAVE_EVERY);
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    let kept = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    let step = kept.state.position.x / i32::try_from(SAVE_EVERY).expect("200");
    assert_eq!((kept.version, kept.state.position.x % 200, step > 0), (1, 0, true));

    walk(&mut writer, SAVE_EVERY + 1, 2 * SAVE_EVERY - 1);
    let unsaved = writer.zone.dirty()[0].state.position.x - kept.state.position.x;
    let lost = unsaved / step;
    assert_eq!(lost, 199);
    assert!(lost <= i32::try_from(SAVE_EVERY).expect("200"), "a stop loses {lost} ticks");
    drop(writer);

    let (mut next, _heard) = meadow.writer().await;
    let last = meadow.join(&mut next).await;
    assert_eq!(last, kept);
    let [(_, snapshot)] = next.zone.tick().try_into().expect("one snapshot");
    assert_eq!(snapshot.message.entered[0].position, kept.state.position);
    assert_eq!(hash(&meadow.redis, "zone:1").await["epoch"], "2");
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_that_a_newer_epoch_holds_stops_at_its_first_save_or_its_first_entry(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    meadow.store.claim(MEADOW).await.expect("a newer epoch");
    walk(&mut writer, 1, SAVE_EVERY);
    let fault = hear(&mut writer, &mut heard, saved_event).await;
    assert_eq!(
      fault.as_deref(),
      Some(
        "the database refuses the zone writer: zone 1 is at epoch 2, and the batch is of epoch 1"
      )
    );

    let (mut other, mut heard) = meadow.writer().await;
    let url = meadow.redis.url();
    let newer = Claim::open(&url, MEADOW, mmo_protocol::Epoch(9), "z:1".to_owned());
    newer.await.expect("a claim of a newer epoch");
    walk(&mut other, 1, 20);
    let fault = hear(&mut other, &mut heard, |_, _| false).await;
    assert_eq!(
      fault.as_deref(),
      Some("zone 1 is held by a writer of epoch 9, which is newer than the epoch 3")
    );
  }

  #[sqlx::test(migrations = false)]
  async fn a_logout_saves_its_player_and_ends_its_presence_after_the_save(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    let entries = |_: &Writer, event: &Event| matches!(event, Event::Entries(_));
    assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
    let present = BTreeMap::from(
      [("character", "1"), ("zone", "1"), ("epoch", "1")].map(|(k, v)| (k.into(), v.into())),
    );
    let key = format!("presence:{}", meadow.account.0);
    assert_eq!(hash(&meadow.redis, &key).await, present);
    assert_eq!(hash(&meadow.redis, "zone:1").await["address"], "zone-1:4433");

    walk(&mut writer, 1, 3);
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert_eq!(writer.owed.len(), 1);
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    assert!(writer.owed.is_empty());
    while !hash(&meadow.redis, &key).await.is_empty() {
      assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
    }
    let saved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!((saved.version, saved.state.position.x), (1, 90));
  }

  #[sqlx::test(migrations = false)]
  async fn a_refused_save_changes_nothing_and_goes_again_at_the_next_save(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    walk(&mut writer, 1, 3);
    let moved = writer.zone.dirty();
    let refused = Event::Saved {
      batch: writer.claim.batch(moved),
      result: Err(mmo_store::Error::Behind { version: 1 }),
    };
    assert!(writer.hear(refused).is_none());
    let redis = claim::Error::Redis(redis::Client::open("nope").expect_err("a bad URL"));
    assert!(writer.hear(Event::Entries(Err(redis))).is_none());
    assert_eq!(writer.zone.dirty().len(), 1);

    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert_eq!(writer.owed.len(), 1);
    let refused = Event::Saved {
      batch: writer.claim.batch(vec![writer.owed[&1].save.expect("a save")]),
      result: Err(mmo_store::Error::Behind { version: 1 }),
    };
    assert!(writer.hear(refused).is_none());
    assert_eq!(writer.owed.len(), 1);
    writer.keep();
    let empty = |writer: &Writer, _: &Event| writer.owed.is_empty();
    assert_eq!(hear(&mut writer, &mut heard, empty).await, None);
    let saved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!(saved.state.position.x, 90);
    writer.write(Entry::Absent(Vec::new()));
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_that_a_newer_epoch_holds_ends_the_serve_of_its_links(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let claim = meadow.claim().await;
    let url = meadow.redis.url();
    let newer = Claim::open(&url, MEADOW, mmo_protocol::Epoch(9), "z:1".to_owned());
    newer.await.expect("a claim of a newer epoch");
    let endpoint = endpoint(&World::new());
    let fault = serve(endpoint, Zone::new(MEADOW), meadow.store.clone(), claim).await;
    assert_eq!(
      fault.to_string(),
      "zone 1 is held by a writer of epoch 9, which is newer than the epoch 1"
    );
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_that_leaves_with_nothing_to_save_ends_its_presence_at_once(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    let entries = |_: &Writer, event: &Event| matches!(event, Event::Entries(_));
    assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
    let key = format!("presence:{}", meadow.account.0);
    assert_eq!(hash(&meadow.redis, &key).await.len(), 3);
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    while !hash(&meadow.redis, &key).await.is_empty() {
      assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
    }
    assert!(writer.owed.is_empty());
    let saved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!(saved.version, 0);
  }
}
