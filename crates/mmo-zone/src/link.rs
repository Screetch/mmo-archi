//! The links of the gateways, which relay the messages of the players to the layer writer, and its
//! snapshots to the players.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::mem;
use std::time::{Duration, Instant};

use mmo_protocol::{
  AccountId, Batch, CharacterId, Entity, Frames, FromClient, FromNeighbour, Hello, Intents, Layer,
  Link, Relayed, SAVES, Save, TICKS_PER_SECOND, ToClient, ToNeighbour, ToZone, ZoneId, decode,
  encode, frame, relayed, unrelayed,
};
use mmo_store::{Saved, Store, keys};
use quinn::{
  Connection, ConnectionError, Endpoint, Incoming, ReadError, RecvStream, VarInt, WriteError,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::time;

use crate::Error;
use crate::claim::{self, Claim, Entry};
use crate::keeper::{self, Keeper};
use crate::remote::{self, Dial, Remote};
use crate::zone::{Crossing, Gateway, Leaver, Refusal, Zone};

/// The time of one tick: the zone runs 20 ticks each second, as the step of `mmo-collide` does.
const TICK: Duration = Duration::from_millis(50);

/// The ticks between two writes of the entry of the zone in the directory: 1 s, which keeps its
/// lease of 3 s.
const ANNOUNCE_EVERY: u32 = TICKS_PER_SECOND;

/// The ticks between two saves of the players, and the presence that each save renews: 10 s. It is
/// the most play that a player loses when a writer stops, as the table of targets says.
const SAVE_EVERY: u32 = 10 * TICKS_PER_SECOND;

/// The ticks after which a layer that holds no player closes: 30 s, the time that it keeps the
/// player of a closed link.
const EMPTY_FOR: u32 = 30 * TICKS_PER_SECOND;

/// The ticks after which a layer that held at most a quarter of its cap asks the directory to drain
/// it: 60 s. It asks again each second until it drains or holds more players.
const LOW_FOR: u32 = 60 * TICKS_PER_SECOND;

/// The build in the hello of the layer writer: the version of the workspace, as 1002003 for 1.2.3.
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

/// The time that a hand-over waits before it asks again: the next zone has no room for the player,
/// or no writer takes the layer that the placement gave.
const RETRY: Duration = Duration::from_secs(1);

/// The tries of the load of a character that joins. A stack that starts opens many connections
/// to PostgreSQL at once, and a pool that waits on a full database fails a try that a later one
/// passes.
const LOADS: u32 = 5;

/// Whether `error` passes with time: the pool found no free connection, or the link to the database
/// broke.
fn passing(error: &mmo_store::Error) -> bool {
  matches!(error, mmo_store::Error::Database(sqlx::Error::PoolTimedOut | sqlx::Error::Io(_)))
}

/// The code with which the layer writer closes the link of a gateway after a fault. The reason of
/// the close holds the text of the fault.
const FAULT: VarInt = VarInt::from_u32(1);

/// Why the layer writer ends the link of a gateway.
#[derive(Debug, thiserror::Error)]
enum Fault {
  #[error("the link fails: {0}")]
  Link(#[from] ConnectionError),
  #[error("the stream of the gateway fails: {0}")]
  Read(#[from] ReadError),
  #[error("the layer writer cannot write to the gateway: {0}")]
  Write(#[from] WriteError),
  #[error("the gateway ended its stream")]
  Ended,
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
}

/// Why the layer writer takes no player for a [`ToZone::Join`].
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

/// Why a message to the writer of a layer across a border does not end at once.
#[derive(Debug, thiserror::Error)]
enum Handover {
  #[error(transparent)]
  Claim(#[from] claim::Error),
  #[error(transparent)]
  Remote(#[from] remote::Error),
}

/// What the tasks of the links tell the layer writer.
enum Event {
  /// A peer opened a link, and the sender writes the frames of the stream to it.
  Opened(Gateway, Connection, UnboundedSender<Vec<u8>>),
  /// The writer of `to` took the player of `character`, which walked past a border.
  Crossed {
    character: CharacterId,
    to: Layer,
  },
  /// No other layer of its zone took the player of `character`, which moves out of this layer
  /// that drains: it walks again until the next try.
  Stayed {
    character: CharacterId,
  },
  /// The next zone does not take the player of `character` yet, which waits at the border. Each
  /// retry sends it again.
  Held {
    character: CharacterId,
    zone: ZoneId,
  },
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
    result: Result<u64, keeper::Error>,
  },
  Entries(Result<(), claim::Error>),
  Closed(Gateway),
  /// The answer of the close of the layer: whether it closed.
  Released(Result<bool, claim::Error>),
  /// The answer of the drain of the layer: whether it drains.
  Drained(Result<bool, claim::Error>),
}

/// What a layer writer holds once it claimed its layer: the state of the layer, the store that it
/// reads the last saves from, its claim in Redis, its link to the keeper, which writes its saves,
/// and what it needs to dial the writer of the next zone.
pub(crate) struct Claimed {
  pub(crate) zone: Zone,
  pub(crate) store: Store,
  pub(crate) claim: Claim,
  pub(crate) keeper: Keeper,
  pub(crate) dial: Dial,
}

/// The link that a peer opened to the writer: the connection, and the frames to write on its
/// stream. A peer is a gateway, or the writer of a layer that hands a player over.
struct Opened {
  connection: Connection,
  out: UnboundedSender<Vec<u8>>,
}

/// Runs the tick of the zone, and takes the links of the gateways, each in a task of its own,
/// until the endpoint closes, or a newer writer holds the zone.
pub(crate) async fn serve(endpoint: Endpoint, claimed: Claimed) -> Error {
  let (events, mut heard) = unbounded_channel();
  let mut writer = Writer::new(claimed, events);
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
  /// The batches for the task that sends them to the keeper, one after the other.
  batches: UnboundedSender<Batch>,
  /// What the writer asks of its layer in the directory, for the task that runs each ask in turn,
  /// so that a close or a drain reads the last count that the writer wrote.
  asks: UnboundedSender<Ask>,
  events: UnboundedSender<Event>,
  gateways: BTreeMap<Gateway, Opened>,
  dial: Dial,
  /// The players that a writer of the next zone handed over, which wait for the keeper to save
  /// them in this zone, with the link of the writer that answers, by character.
  pending: BTreeMap<i64, Gateway>,
  /// The ticks since the last save, from 0 to `SAVE_EVERY - 1`.
  ticks: u32,
  /// The count of the ticks since the last entry, by the bucket of their time in [`keys::ticks`].
  timing: BTreeMap<u32, u64>,
  /// The players that left, and whose last state the store did not acknowledge yet, by character.
  owed: BTreeMap<i64, Leaver>,
  /// The ticks since the layer last held a player or a save that the store did not acknowledge.
  empty: u32,
  /// The task that tells the layer across each border, by its zone, which entities of this layer
  /// stand near that border.
  edges: BTreeMap<i32, Edge>,
  /// The ticks for which the layer held at most a quarter of its cap, and did not drain.
  low: u32,
}

/// The task that tells the layer across a border what stands near it.
struct Edge {
  /// The entities to tell, which replace those of the last message that the task did not send yet.
  to: watch::Sender<Vec<Entity>>,
  /// True when the last list that the task got held an entity.
  shown: bool,
}

impl Writer {
  fn new(claimed: Claimed, events: UnboundedSender<Event>) -> Self {
    let Claimed { zone, store, claim, keeper, dial } = claimed;
    let (batches, sent) = unbounded_channel();
    tokio::spawn(deliver(keeper, sent, events.clone()));
    let (asks, asked) = unbounded_channel();
    tokio::spawn(answer(claim.clone(), asked, events.clone()));
    let (gateways, owed, pending) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let timing = BTreeMap::new();
    let (ticks, empty, low) = (0, 0, 0);
    let writer = Self {
      zone,
      store,
      claim,
      batches,
      asks,
      events,
      gateways,
      dial,
      pending,
      ticks,
      timing,
      owed,
      empty,
      edges: BTreeMap::new(),
      low,
    };
    // The first lease of the epoch of the writer opens the layer.
    writer.write(Entry::Layer { players: 0, taken: Vec::new() });
    writer
  }

  /// Takes an event, and gives the fault that stops the layer writer when the event brings one.
  fn hear(&mut self, event: Event) -> Option<Error> {
    match event {
      Event::Opened(gateway, connection, out) => {
        self.gateways.insert(gateway, Opened { connection, out });
      }
      Event::Crossed { character, to } => {
        if let Some((gateway, link)) = self.zone.depart(character) {
          self.tell(gateway, link, ToClient::Moved { zone: to.zone, layer: to.number });
        }
      }
      Event::Stayed { character } => self.zone.stay(character),
      Event::Drained(Ok(drains)) => {
        if drains {
          self.zone.drain();
        }
      }
      Event::Drained(Err(error)) => eprintln!("the layer writer cannot drain its layer: {error}"),
      Event::Held { character, zone } => {
        if let Some((gateway, link)) = self.zone.linked(character) {
          self.tell(gateway, link, ToClient::Full { zone });
        }
      }
      Event::Message(gateway, message) => self.message(gateway, message),
      Event::Intents(gateway, Relayed { link, message }) => {
        self.zone.intents(gateway, link, &message);
      }
      Event::Loaded { gateway, link, account, found } => {
        if let Err(why) = self.loaded(gateway, link, account, found) {
          eprintln!("the layer writer takes no player on the link {}: {why}", link.0);
        }
      }
      Event::Saved { batch, result } => return self.saved(&batch, result),
      Event::Entries(Err(error @ claim::Error::Superseded { .. })) => return Some(error.into()),
      Event::Entries(Err(error)) => eprintln!("the layer writer cannot write in Redis: {error}"),
      Event::Entries(Ok(())) => {}
      Event::Closed(gateway) => self.close(gateway),
      Event::Released(Ok(true)) => return Some(Error::Released),
      Event::Released(answer) => {
        // A place or a player came, or Redis failed: the layer runs on, and tries again later.
        if let Err(error) = answer {
          eprintln!("the layer writer cannot close its layer: {error}");
        }
        self.empty = 0;
      }
    }
    None
  }

  /// Hears `work` once it ends, in a task, so the tick goes on.
  fn later(&self, work: impl Future<Output = Event> + Send + 'static) {
    let events = self.events.clone();
    tokio::spawn(async move {
      // The send fails only when the layer writer stopped, and then no one needs the result.
      drop(events.send(work.await));
    });
  }

  /// Closes the layer, when it still holds no player and no place.
  fn release(&self) {
    self.ask(Ask::Close);
  }

  /// Asks the directory to drain the layer, when the other layers of its zone have room.
  fn ask_drain(&self) {
    self.ask(Ask::Drain);
  }

  /// Asks the directory in its turn: Redis runs the asks of a writer in the order that it made
  /// them, however fast it makes them.
  fn ask(&self, ask: Ask) {
    // The send fails only when the layer writer stopped, and then no one needs the answer.
    drop(self.asks.send(ask));
  }

  /// Moves the player of `moving` out of this layer that drains, to the layer of its zone that the
  /// placement gives, with the steps of the hand-off. When no layer takes it, it stays.
  fn move_out(&self, moving: Crossing) {
    let (claim, dial) = (self.claim.clone(), self.dial.clone());
    self.later(async move {
      let character = moving.state.id;
      match offer(&claim, &dial, &moving).await {
        Ok(Some(to)) => Event::Crossed { character, to },
        Ok(None) => Event::Stayed { character },
        Err(error) => {
          eprintln!("the layer writer cannot move the character {}: {error}", character.0);
          Event::Stayed { character }
        }
      }
    });
  }

  fn message(&mut self, gateway: Gateway, message: ToZone) {
    match message {
      ToZone::Join { link, account, character } => self.load(gateway, link, account, character),
      ToZone::Client(Relayed { link, message: FromClient::Logout }) => {
        self.zone.logout(gateway, link);
        self.leave();
      }
      // The layer writer sends no chat yet, so a line goes nowhere.
      ToZone::Client(Relayed { message: FromClient::Say { .. }, .. }) => {}
      ToZone::Leave { link } => self.zone.leave(gateway, link),
      ToZone::Neighbour(ToNeighbour::Transfer { account, state, version }) => {
        self.receive(gateway, account, state, version);
      }
      ToZone::Neighbour(ToNeighbour::Edge { zone, entities }) => {
        self.zone.seen(zone, entities);
        self.answer(gateway, &FromNeighbour::Seen);
      }
    }
  }

  /// Takes the player that the writer on the link `gateway` hands over, writes the count of the
  /// layer, which ends the place of the player, and its presence, and asks the keeper to save the
  /// player in this zone. The writer gets its answer when the keeper acknowledges the save.
  fn receive(&mut self, gateway: Gateway, account: AccountId, state: Entity, version: i64) {
    let character = state.id;
    match self.zone.receive(account, &state, version) {
      Err(_) => self.answer(gateway, &FromNeighbour::Full { character }),
      Ok(save) => {
        self.write(Entry::Layer { players: self.zone.count(), taken: vec![account] });
        self.write(Entry::Present(vec![(account, character)]));
        if let Some(save) = save {
          self.pending.insert(character.0, gateway);
          self.send(&[save]);
        } else {
          self.answer(gateway, &FromNeighbour::Accepted { character });
        }
      }
    }
  }

  /// Hands the player of `crossing` over to the writer of the layer that the placement gives, in a
  /// task, so the tick goes on. The task asks again each second until a writer takes the player.
  fn hand_over(&self, crossing: Crossing) {
    let (claim, dial, events) = (self.claim.clone(), self.dial.clone(), self.events.clone());
    tokio::spawn(async move {
      let character = crossing.state.id;
      let layer = loop {
        match offer(&claim, &dial, &crossing).await {
          Ok(Some(layer)) => break layer,
          Ok(None) => {}
          Err(error) => {
            eprintln!("the layer writer cannot hand the character {} over: {error}", character.0);
          }
        }
        // The sends of this task fail only when the layer writer stopped.
        drop(events.send(Event::Held { character, zone: crossing.zone }));
        time::sleep(RETRY).await;
      };
      drop(events.send(Event::Crossed { character, to: layer }));
    });
  }

  /// Tells the layer across a border which entities of this layer stand near it, in a task, so the
  /// tick goes on. A border with no entity near it, and none the last time, tells nothing.
  fn edge(&mut self, layer: Layer, entities: Vec<Entity>) {
    let edge = self.edges.entry(layer.zone.0).or_insert_with(|| {
      let (to, heard) = watch::channel(Vec::new());
      let (claim, dial, from) = (self.claim.clone(), self.dial.clone(), self.zone.zone());
      tokio::spawn(report(claim, dial, from, layer, heard));
      Edge { to, shown: false }
    });
    if !entities.is_empty() || edge.shown {
      edge.shown = !entities.is_empty();
      // The send fails only when the task ended, and it ends with the layer writer.
      drop(edge.to.send(entities));
    }
  }

  /// Writes `bytes` on the stream of the peer on the link `gateway`, when that link stands.
  fn send_to(&self, gateway: Gateway, bytes: Vec<u8>) {
    if let Some(peer) = self.gateways.get(&gateway) {
      // The send fails only when the task of the link ended, and its close comes next.
      drop(peer.out.send(bytes));
    }
  }

  /// Tells the client of `link`, through its gateway.
  fn tell(&self, gateway: Gateway, link: Link, message: ToClient) {
    self.send_to(gateway, framed(&encode(&Relayed { link, message })));
  }

  fn answer(&self, gateway: Gateway, message: &FromNeighbour) {
    self.send_to(gateway, framed(&encode(message)));
  }

  /// Loads the last save of `character`. A try that the database fails for a while, as a pool
  /// with no free connection does, goes again after `RETRY`, up to `LOADS` tries.
  fn load(&self, gateway: Gateway, link: Link, account: AccountId, character: CharacterId) {
    let store = self.store.clone();
    self.later(async move {
      let mut tries = 1;
      loop {
        match store.load(&[character]).await {
          Err(why) if tries < LOADS && passing(&why) => {
            eprintln!("the layer writer loads the character {} again: {why}", character.0);
            tries += 1;
            time::sleep(RETRY).await;
          }
          found => return Event::Loaded { gateway, link, account, found },
        }
      }
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

  /// Takes the player of `saved` into the layer, writes the count of the layer, which ends the
  /// place of the player in the directory in the same step, and writes its presence.
  fn admit(
    &mut self,
    gateway: Gateway,
    link: Link,
    account: AccountId,
    saved: &Saved,
  ) -> Result<(), Refusal> {
    self.zone.join(gateway, link, account, saved)?;
    self.write(Entry::Layer { players: self.zone.count(), taken: vec![account] });
    self.write(Entry::Present(vec![(account, saved.character)]));
    Ok(())
  }

  fn close(&mut self, gateway: Gateway) {
    self.gateways.remove(&gateway);
    self.zone.close(gateway);
  }

  /// Writes `entry` in Redis.
  fn write(&self, entry: Entry) {
    self.ask(Ask::Write(entry));
  }

  /// Sends `saves` to the keeper in batches of `SAVES` at most, so each batch fits in one frame.
  fn send(&self, saves: &[Save]) {
    for saves in saves.chunks(SAVES) {
      // The send fails only when the layer writer stopped, and then no one needs the batch.
      drop(self.batches.send(self.claim.batch(saves.to_vec())));
    }
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
    self.send(&saves);
  }

  /// The keeper takes a batch, or refuses it. A newer writer of the layer ends this one. Any other
  /// refusal, and a fault of the link to the keeper, change nothing, so the writer keeps the last
  /// state of each player that the keeper does not hold, and sends it again at the next save. A
  /// player that left keeps its presence until then.
  fn saved(&mut self, batch: &Batch, result: Result<u64, keeper::Error>) -> Option<Error> {
    match result {
      Ok(_) => {
        self.zone.saved(&batch.saves);
        for save in &batch.saves {
          if let Some(gateway) = self.pending.remove(&save.character.0) {
            self.answer(gateway, &FromNeighbour::Accepted { character: save.character });
          }
        }
        let left = batch.saves.iter().filter_map(|save| self.owed.remove(&save.character.0));
        let absent = left.map(|leaver| leaver.account).collect();
        self.write(Entry::Absent(absent));
        None
      }
      Err(keeper::Error::Stale { held }) => {
        let layer = batch.layer;
        Some(claim::Error::Superseded { layer, held: held.0, own: batch.epoch }.into())
      }
      Err(error) => {
        eprintln!(
          "the keeper takes no batch of {} saves, and the layer writer sends them again: {error}",
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
    self.send(&saves);
    self.write(Entry::Present(self.zone.present()));
  }

  /// Runs one tick, and sends each snapshot to the gateway of its player. A gateway that a
  /// snapshot cannot reach closes, so its players leave. Each second it renews the entry of the
  /// zone and adds the times of its ticks to [`keys::ticks`], asks for the drain of a layer that
  /// held at most a quarter of its cap for 60 s, and moves the players of a layer that drains.
  /// Each 10 s it saves.
  fn tick(&mut self) {
    let started = Instant::now();
    let snapshots = self.zone.tick();
    self.low = if self.zone.low() { self.low.saturating_add(1) } else { 0 };
    for crossing in self.zone.crossings() {
      self.hand_over(crossing);
    }
    for (layer, entities) in self.zone.edges() {
      self.edge(layer, entities);
    }
    let mut lost = Vec::new();
    for (gateway, peer) in &self.gateways {
      for (_, snapshot) in snapshots.iter().filter(|(via, _)| via == gateway) {
        let bytes = relayed(snapshot.link, &encode(&snapshot.message));
        if let Err(error) = peer.connection.send_datagram(bytes.into()) {
          eprintln!("a snapshot does not reach the gateway {}: {error}", gateway.0);
          lost.push(*gateway);
          break;
        }
      }
    }
    for gateway in lost {
      self.close(gateway);
    }
    *self.timing.entry(keys::tick_bucket(started.elapsed())).or_default() += 1;
    self.ticks = (self.ticks + 1) % SAVE_EVERY;
    if self.ticks.is_multiple_of(ANNOUNCE_EVERY) {
      let timing = mem::take(&mut self.timing);
      self.write(Entry::Ticks(timing));
      self.write(Entry::Layer { players: self.zone.count(), taken: Vec::new() });
      if self.low >= LOW_FOR {
        self.ask_drain();
      }
      for moving in self.zone.moves() {
        self.move_out(moving);
      }
    }
    if self.ticks == 0 {
      self.keep();
    }
    self.leave();
    let idle = self.zone.count() == 0 && self.owed.is_empty();
    self.empty = if idle { self.empty + 1 } else { 0 };
    if self.empty == EMPTY_FOR {
      self.release();
    }
  }
}

/// Asks the writer of the layer that the placement gives in the zone of `crossing` to take the
/// player, and gives that layer when it does. It gives `None` when no layer has room, no writer
/// serves the layer yet, or the writer refuses the player.
async fn offer(claim: &Claim, dial: &Dial, crossing: &Crossing) -> Result<Option<Layer>, Handover> {
  let Crossing { zone, account, state, version } = *crossing;
  let Some((layer, serving)) = claim.crossing(account, zone).await? else { return Ok(None) };
  let message = ToZone::Neighbour(ToNeighbour::Transfer { account, state, version });
  let mut remote = dial.to(serving.address, "neighbour");
  Ok(match remote.ask(&encode(&message), decode::<FromNeighbour>).await? {
    FromNeighbour::Accepted { .. } => Some(layer),
    FromNeighbour::Full { .. } | FromNeighbour::Seen => None,
  })
}

/// Tells the writer of `layer`, which is across a border of the zone `from`, the last entities that
/// stand near that border, until the layer writer stops. It tells the last list that came while it
/// waited for the writer. When no writer serves the layer, or the writer fails, it waits 1 s.
async fn report(
  claim: Claim,
  dial: Dial,
  from: ZoneId,
  layer: Layer,
  mut heard: watch::Receiver<Vec<Entity>>,
) {
  let mut remote = None;
  while heard.changed().await.is_ok() {
    let entities = heard.borrow_and_update().clone();
    let message = encode(&ToZone::Neighbour(ToNeighbour::Edge { zone: from, entities }));
    if tell(&claim, &dial, &mut remote, layer, &message).await.is_err() {
      remote = None;
      time::sleep(RETRY).await;
    }
  }
}

/// Sends `message` to the writer of `layer` on `remote`, which it opens when it holds none.
async fn tell(
  claim: &Claim,
  dial: &Dial,
  remote: &mut Option<Remote>,
  layer: Layer,
  message: &[u8],
) -> Result<(), Handover> {
  let link = match remote {
    Some(link) => link,
    None => remote.insert(dial.to(claim.serving(layer).await?.address, "neighbour")),
  };
  link.ask(message, decode::<FromNeighbour>).await?;
  Ok(())
}

/// Sends each batch to the keeper in turn, and tells the layer writer its result, until the writer
/// stops.
async fn deliver(
  mut keeper: Keeper,
  mut batches: UnboundedReceiver<Batch>,
  events: UnboundedSender<Event>,
) {
  while let Some(batch) = batches.recv().await {
    let result = keeper.save(batch.clone()).await;
    // The send fails only when the layer writer stopped, and then no one needs the result.
    drop(events.send(Event::Saved { batch, result }));
  }
}

/// What a layer writer asks of its layer in the directory.
enum Ask {
  /// Writes an entry in Redis.
  Write(Entry),
  /// Closes the layer, when it holds no player and no place.
  Close,
  /// Puts the layer in drain, when the other layers of its zone have room.
  Drain,
}

/// Runs each ask of the layer writer in turn, and tells it the answer, until the writer stops. A
/// placement reads the count that the last entry wrote, so no ask passes another.
async fn answer(claim: Claim, mut asks: UnboundedReceiver<Ask>, events: UnboundedSender<Event>) {
  while let Some(ask) = asks.recv().await {
    let event = match ask {
      Ask::Write(entry) => Event::Entries(claim.write(entry).await),
      Ask::Close => Event::Released(claim.close().await),
      Ask::Drain => Event::Drained(claim.drain().await),
    };
    // The send fails only when the layer writer stopped, and then no one needs the answer.
    drop(events.send(event));
  }
}

/// Relays the messages of the link of a gateway to the layer writer until a fault ends it.
async fn gateway(incoming: Incoming, gateway: Gateway, events: UnboundedSender<Event>) {
  let fault = match incoming.await {
    Ok(connection) => {
      // Each send of this task fails only when the layer writer stopped, and then no one needs it.
      let (out, outgoing) = unbounded_channel();
      drop(events.send(Event::Opened(gateway, connection.clone(), out)));
      let Err(fault) = relay(&connection, gateway, &events, outgoing).await;
      connection.close(FAULT, fault.to_string().as_bytes());
      fault
    }
    Err(error) => Fault::Link(error),
  };
  eprintln!("the link of the gateway {} ends: {fault}", gateway.0);
  drop(events.send(Event::Closed(gateway)));
}

/// Takes the stream that the peer opens. Each side writes its hello first, then the peer sends its
/// messages on the stream, and the intents of its players in datagrams when it is a gateway. The
/// writer answers on the stream with the frames of `outgoing`.
async fn relay(
  connection: &Connection,
  gateway: Gateway,
  events: &UnboundedSender<Event>,
  mut outgoing: UnboundedReceiver<Vec<u8>>,
) -> Result<Infallible, Fault> {
  let (mut send, recv) = connection.accept_bi().await?;
  let mut stream = Stream { recv, frames: Frames::default() };
  send.write_all(&framed(&Hello { build: BUILD }.encode())).await?;
  Hello::decode(&stream.next().await?)?;
  loop {
    let event = tokio::select! {
      payload = stream.next() => {
        payload.and_then(|payload| Ok(Some(Event::Message(gateway, decode(&payload)?))))
      }
      datagram = connection.read_datagram() => datagram
        .map_err(Fault::from)
        .and_then(|datagram| {
          let (link, intents) = unrelayed(&datagram)?;
          Ok(Some(Event::Intents(gateway, Relayed { link, message: decode(intents)? })))
        }),
      Some(bytes) = outgoing.recv() => send.write_all(&bytes).await.map(|()| None).map_err(Fault::from),
    }?;
    if let Some(event) = event {
      drop(events.send(event));
    }
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

#[expect(
  clippy::expect_used,
  reason = "a hello and a message of the writer hold less than a frame"
)]
fn framed(payload: &[u8]) -> Vec<u8> {
  frame(payload).expect("a message of the writer holds less than a frame")
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::cell::RefCell;
  use std::collections::BTreeMap;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Duration;

  use mmo_collide::SIDE_CM;
  use mmo_protocol::{
    AccountId, Angle, CharacterId, FromClient, Input, Intents, Link, PROTOCOL, Password, Position,
    Relayed, SAVES, Save, State, TICKS_PER_SECOND, Tick, ToZone, ZoneId, frame,
  };
  use mmo_protocol::{Entity, Frames, FromNeighbour, ToClient, ToNeighbour, Velocity, decode};
  use mmo_protocol::{Epoch, Layer, RealmId};
  use mmo_store::testing::Redis;
  use mmo_store::{Directory, Lease, Leased, NewCharacter, Saved, Square, Store, keys};
  use quinn::{ConnectionError, Endpoint, TransportErrorCode};
  use redis::AsyncCommands;
  use sqlx::PgPool;
  use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
  use tokio::time;

  use super::{
    ANNOUNCE_EVERY, Claimed, EMPTY_FOR, Entry, Event, Gateway, LOW_FOR, Opened, SAVE_EVERY, Writer,
    claim, keeper, serve,
  };
  use crate::claim::Taken;
  use crate::config::Config;
  use crate::peer::{Keeper, Peer, World, hash, var};
  use crate::remote::Dial;
  use crate::zone::Zone;

  const MEADOW: ZoneId = ZoneId(1);
  const REALM: RealmId = RealmId(1);
  const LAYER: Layer = Layer { zone: MEADOW, number: 1 };
  const GATE: Gateway = Gateway(1);
  const FOREST: ZoneId = ZoneId(2);
  const FOREST_LAYER: Layer = Layer { zone: FOREST, number: 1 };
  /// The zone north of the meadow, whose id is no neighbour of the id of the meadow.
  const RIDGE: ZoneId = ZoneId(5);
  const RIDGE_LAYER: Layer = Layer { zone: RIDGE, number: 1 };
  /// Brom stands one step from the east border of the meadow.
  const BORDER: State = State {
    position: Position { x: SIDE_CM - 30, y: 0, z: 10_000 },
    yaw: Angle(0),
    hit_points: 100,
  };
  /// The time that a test waits for a message: a fault fails the test, and never hangs it.
  const WAIT: Duration = Duration::from_secs(60);
  const START: State =
    State { position: Position { x: 0, y: 0, z: 10_000 }, yaw: Angle(0), hit_points: 100 };

  /// A world of one zone with one character, its Redis and its keeper.
  struct Meadow {
    world: World,
    redis: Redis,
    keeper: Keeper,
    store: Store,
    account: AccountId,
    character: CharacterId,
  }

  impl Meadow {
    async fn new(pool: PgPool) -> Self {
      Self::at(pool, START).await
    }

    /// A world whose one character starts in the meadow at `state`.
    async fn at(pool: PgPool, state: State) -> Self {
      let store = Store::migrate(pool).await.expect("a migrated database");
      store.add_zone(MEADOW, "meadow").await.expect("a zone");
      store.add_zone(FOREST, "forest").await.expect("a zone");
      store.place_zone(MEADOW, Square { column: 0, row: 0 }).await.expect("a square");
      store.place_zone(FOREST, Square { column: 1, row: 0 }).await.expect("a square");
      store.add_zone(RIDGE, "ridge").await.expect("a zone");
      store.place_zone(RIDGE, Square { column: 0, row: 1 }).await.expect("a square");
      let account = store.create_account("alice", Password("one".to_owned())).await.expect("one");
      let brom = NewCharacter { account, realm: REALM, name: "Brom", zone: MEADOW, state };
      let character = store.create_character(&brom).await.expect("Brom");
      let world = World::new();
      let keeper = Keeper::start(&world, store.clone());
      Self { world, redis: Redis::start().await, keeper, store, account, character }
    }

    /// The endpoint of the layer writer of the meadow, and what it holds once it claimed its layer
    /// from the keeper.
    async fn claimed(&self) -> (Endpoint, Claimed) {
      self.claimed_for(LAYER, |_| "zone-1:4433".to_owned()).await
    }

    /// The same for `layer`, whose lease holds the address that `address` makes of the endpoint.
    async fn claimed_for(
      &self,
      layer: Layer,
      address: impl FnOnce(&Endpoint) -> String,
    ) -> (Endpoint, Claimed) {
      let mut vars = self.world.vars("127.0.0.1:0", "postgres://zone@localhost/game", "nope");
      vars.insert(0, ("ZONE_KEEPER", self.keeper.address()));
      let config = Config::read(var(vars)).expect("a config");
      let endpoint = Endpoint::server(config.server, config.address).expect("an endpoint");
      let dial = Dial { endpoint: endpoint.clone(), client: config.client.clone(), ipv4: true };
      let mut keeper = keeper::Keeper::new(endpoint.clone(), config.client, config.keeper, true);
      let mut admin = self.admin().await;
      let () = admin.hset(keys::realm(REALM), keys::LAYER_CAP, 500).await.expect("a cap");
      let () = admin.hset(keys::layers(REALM), keys::field(layer), 0).await.expect("a layer");
      let () = admin.del(keys::writer(REALM, layer)).await.expect("no lease");
      let url = self.redis.url();
      let taken = Taken::wait(&url, REALM, address(&endpoint)).await.expect("a layer");
      assert_eq!(taken.layer, layer);
      let epoch = keeper.claim(REALM, taken.layer).await.expect("an epoch");
      let across = self.store.neighbours(taken.layer.zone).await.expect("the neighbours");
      let zone = Zone::new(taken.layer, taken.cap).beside(across);
      let claim = taken.hold(epoch);
      let store = self.store.clone();
      (endpoint, Claimed { zone, store, claim, keeper, dial })
    }

    async fn admin(&self) -> redis::aio::MultiplexedConnection {
      let client = redis::Client::open(self.redis.url()).expect("a URL");
      client.get_multiplexed_async_connection().await.expect("a connection")
    }

    /// Leases the layer to a writer of `epoch`, as a newer writer does.
    async fn supersede(&self, epoch: i64) {
      let directory = Directory { realm: REALM };
      let lease =
        Lease { layer: LAYER, epoch: Epoch(epoch), address: "z:1", players: 0, taken: &[] };
      let newer = directory.lease(&mut self.admin().await, &lease).await;
      assert_eq!(newer.expect("a lease"), Leased::Held);
    }

    /// A writer that claims the zone, and the events that its tasks send.
    async fn writer(&self) -> (Writer, UnboundedReceiver<Event>) {
      let (events, heard) = unbounded_channel();
      let writer = Writer::new(self.claimed().await.1, events);
      (writer, heard)
    }

    /// A peer that dialled `writer` as a gateway does, and the frames that the writer writes to it.
    async fn open(
      &self,
      writer: &mut Writer,
      gateway: Gateway,
    ) -> (Peer, Endpoint, UnboundedReceiver<Vec<u8>>) {
      let endpoint = self.world.keeper();
      let address = endpoint.local_addr().expect("an address");
      let accepted = async { endpoint.accept().await.expect("a peer").await.expect("a link") };
      let gateways = self.world.gateway(true);
      let (peer, connection) = tokio::join!(Peer::link(&gateways, address), accepted);
      let (out, frames) = unbounded_channel();
      assert!(writer.hear(Event::Opened(gateway, connection, out)).is_none());
      (peer, endpoint, frames)
    }

    /// Takes the character into `writer`, from its last save.
    async fn join(&self, writer: &mut Writer) -> Saved {
      let saved = self.store.load(&[self.character]).await.expect("a save")[0];
      writer.admit(GATE, Link(1), self.account, &saved).expect("a player");
      saved
    }
  }

  /// Runs ticks of `writer` from `first` to `last`, as the player of link 1 walks east.
  fn walk(writer: &mut Writer, first: u32, last: u32) {
    for tick in first..=last {
      let inputs = [tick, tick + 1, tick + 2].map(|tick| Input {
        tick: Tick(tick),
        heading: Some(Angle(0)),
        view: Angle(0),
      });
      writer.zone.intents(GATE, Link(1), &Intents { zone: MEADOW, layer: 1, ack: None, inputs });
      writer.tick();
    }
  }

  /// Holds once `entries` events came: the writes of the first lease, then of a join, which are
  /// the only events that the writer hears before its first tick.
  fn joined(entries: usize) -> impl Fn(&Writer, &Event) -> bool {
    let seen = Cell::new(0);
    move |_, _| {
      seen.set(seen.get() + 1);
      seen.get() == entries
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
      let event = time::timeout(WAIT, heard.recv()).await.expect("an event in time");
      let event = event.expect("an event");
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
    let world = &meadow.world;
    let (endpoint, claimed) = meadow.claimed().await;
    let address = endpoint.local_addr().expect("an address");
    let zone = tokio::spawn(serve(endpoint.clone(), claimed));
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
    let mut short = Peer::link(&gateways, address).await;
    short.greet().await;
    short.datagram(&[0xff]);
    let mut noisy = Peer::link(&gateways, address).await;
    noisy.greet().await;
    noisy.datagram(&[0xff; 9]);
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
    for peer in [other, large, wrong, short, noisy, deaf, gone, ended] {
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
        reason("a relayed datagram starts with a link of 8 bytes, and this one holds 1 bytes"),
        reason(malformed),
        reason("the layer writer cannot write to the gateway: sending stopped by peer: error 7"),
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

  #[test]
  fn a_load_goes_again_only_when_the_database_fails_for_a_while() {
    let timed_out = mmo_store::Error::Database(sqlx::Error::PoolTimedOut);
    let broken = mmo_store::Error::Database(sqlx::Error::Io(std::io::Error::other("reset")));
    let behind = mmo_store::Error::Behind { version: 1 };
    assert_eq!([&timed_out, &broken, &behind].map(super::passing), [true, true, false]);
  }

  #[sqlx::test(migrations = false)]
  async fn a_join_waits_for_a_free_connection_of_the_database_and_takes_its_player(pool: PgPool) {
    let meadow = Meadow::new(pool.clone()).await;
    let (mut writer, mut heard) = meadow.writer().await;
    let (_peer, _endpoint, _frames) = meadow.open(&mut writer, GATE).await;
    // A pool of one connection, which the test holds for a while, as a full database does.
    let small = sqlx::postgres::PgPoolOptions::new()
      .max_connections(1)
      .acquire_timeout(Duration::from_millis(200))
      .connect_lazy_with(pool.connect_options().as_ref().clone());
    writer.store = Store::open(small.clone()).await.expect("a store");
    let held = small.acquire().await.expect("a connection");
    tokio::spawn(async move {
      time::sleep(Duration::from_millis(500)).await;
      drop(held);
    });
    let join = ToZone::Join { link: Link(1), account: meadow.account, character: meadow.character };
    assert!(writer.hear(Event::Message(GATE, join)).is_none());
    let loaded = |_: &Writer, event: &Event| matches!(event, Event::Loaded { .. });
    assert_eq!(hear(&mut writer, &mut heard, loaded).await, None);
    assert_eq!(writer.zone.count(), 1);
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

    let (endpoint, _) = meadow.claimed().await;
    let gateways = meadow.world.gateway(true);
    let peer = Peer::link(&gateways, endpoint.local_addr().expect("an address"));
    let accepted = async { endpoint.accept().await.expect("a gateway").await.expect("a link") };
    let (peer, connection) = tokio::join!(peer, accepted);
    connection.close(0u32.into(), b"");
    let (out, _) = unbounded_channel();
    writer.gateways.insert(Gateway(1), Opened { connection, out });
    let joined = writer.loaded(Gateway(1), Link(1), account, Ok(vec![saved]));
    assert!(matches!(joined, Ok(())), "{joined:?}");
    writer.tick();
    assert_eq!((writer.gateways.len(), writer.zone.tick()), (0, vec![]));
    drop(peer);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_adds_the_time_of_each_tick_to_its_hash_in_redis_each_second(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    for _ in 0..2 * TICKS_PER_SECOND {
      writer.tick();
    }
    // The first lease, then each second the ticks and the lease of the layer: all of them writes.
    let entries = |_: &Writer, _: &Event| true;
    let counts = loop {
      let ticks = hash(&meadow.redis, "ticks:1:1:1").await;
      let counts: Vec<(u32, u64)> = ticks
        .iter()
        .map(|(bucket, count)| (bucket.parse().expect("a bucket"), count.parse().expect("a count")))
        .collect();
      if counts.iter().map(|(_, count)| count).sum::<u64>() == 40 {
        break counts;
      }
      assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
    };
    assert!(counts.iter().all(|(bucket, _)| *bucket < keys::TICK_BUCKETS), "{counts:?}");
  }

  /// Doc line 20: a writer that stops loses 10 s of play at most.
  #[sqlx::test(migrations = false)]
  async fn a_writer_that_stops_loses_at_most_10_s_and_its_successor_starts_from_the_last_save(
    pool: PgPool,
  ) {
    let mut meadow = Meadow::new(pool).await;
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
    meadow.keeper.ended().await;

    let (mut next, _heard) = meadow.writer().await;
    let last = meadow.join(&mut next).await;
    assert_eq!(last, kept);
    let [(_, snapshot)] = next.zone.tick().try_into().expect("one snapshot");
    assert_eq!(snapshot.message.entered[0].position, kept.state.position);
    assert_eq!(hash(&meadow.redis, "writer:1:1:1").await["epoch"], "2");
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_that_a_newer_epoch_holds_stops_at_its_first_save_or_its_first_entry(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    meadow.store.claim(REALM, LAYER).await.expect("a newer epoch");
    walk(&mut writer, 1, SAVE_EVERY);
    let fault = hear(&mut writer, &mut heard, saved_event).await;
    assert_eq!(
      fault.as_deref(),
      Some("layer 1:1 is held by a writer of epoch 2, which is newer than the epoch 1")
    );

    let (mut other, mut heard) = meadow.writer().await;
    meadow.supersede(9).await;
    walk(&mut other, 1, 20);
    let fault = hear(&mut other, &mut heard, |_, _| false).await;
    assert_eq!(
      fault.as_deref(),
      Some("layer 1:1 is held by a writer of epoch 9, which is newer than the epoch 3")
    );
  }

  #[sqlx::test(migrations = false)]
  async fn a_logout_saves_its_player_and_ends_its_presence_after_the_save(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    let entries = |_: &Writer, event: &Event| matches!(event, Event::Entries(_));
    let present = BTreeMap::from(
      [("character", "1"), ("realm", "1"), ("zone", "1"), ("layer", "1"), ("epoch", "1")]
        .map(|(k, v)| (k.into(), v.into())),
    );
    assert_eq!(hear(&mut writer, &mut heard, joined(3)).await, None);
    let key = format!("presence:{}", meadow.account.0);
    assert_eq!(hash(&meadow.redis, &key).await, present);
    assert_eq!(hash(&meadow.redis, "writer:1:1:1").await["address"], "zone-1:4433");

    walk(&mut writer, 1, 3);
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert_eq!(writer.owed.len(), 1);
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    assert!(writer.owed.is_empty());
    loop {
      assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
      if hash(&meadow.redis, &key).await.is_empty() {
        break;
      }
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
    let refused =
      Event::Saved { batch: writer.claim.batch(moved), result: Err(keeper::Error::Unavailable) };
    assert!(writer.hear(refused).is_none());
    let redis = claim::Error::Redis(redis::Client::open("nope").expect_err("a bad URL"));
    assert!(writer.hear(Event::Entries(Err(redis))).is_none());
    assert_eq!(writer.zone.dirty().len(), 1);

    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert_eq!(writer.owed.len(), 1);
    let refused = Event::Saved {
      batch: writer.claim.batch(vec![writer.owed[&1].save.expect("a save")]),
      result: Err(keeper::Error::Unavailable),
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

  /// Each placement reads the count of a layer that its writer wrote last, so the entries of a
  /// writer reach Redis in the order that it made them. The binary runs its tasks on many threads,
  /// where a task that a task spawned last can run first.
  #[sqlx::test(migrations = false)]
  async fn the_entries_of_a_writer_reach_redis_in_the_order_that_it_made_them(pool: PgPool) {
    const COUNTS: usize = 400;
    let runtime = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_all()
      .build()
      .expect("a runtime");
    let body = runtime.spawn(async move {
      let meadow = Meadow::new(pool).await;
      let (mut writer, mut heard) = meadow.writer().await;
      for players in 1..=COUNTS {
        writer.write(Entry::Layer { players, taken: Vec::new() });
      }
      let entries = AtomicUsize::new(0);
      let all = |_: &Writer, _: &Event| entries.fetch_add(1, Ordering::Relaxed) + 1 == COUNTS + 1;
      assert_eq!(hear(&mut writer, &mut heard, all).await, None);
      assert_eq!(hash(&meadow.redis, "layers:1").await["1:1"], COUNTS.to_string());
    });
    let done = body.await;
    runtime.shutdown_background();
    done.expect("the entries in order");
  }

  /// Doc lines 105-110: the keeper stops and comes back, and each layer writer sends it again the
  /// last state of each player that it did not save.
  #[sqlx::test(migrations = false)]
  async fn a_keeper_that_stops_and_comes_back_loses_no_save(pool: PgPool) {
    let mut meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    walk(&mut writer, 1, 3);
    meadow.keeper.stop();
    writer.keep();
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    walk(&mut writer, 4, 6);
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    let key = format!("presence:{}", meadow.account.0);
    let unsaved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!(
      (unsaved.version, writer.owed.len(), hash(&meadow.redis, &key).await.len()),
      (0, 1, 5)
    );

    meadow.keeper.resume(meadow.store.clone());
    writer.keep();
    let empty = |writer: &Writer, _: &Event| writer.owed.is_empty();
    assert_eq!(hear(&mut writer, &mut heard, empty).await, None);
    let saved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!((saved.version, saved.state.position.x), (2, 180));
  }

  #[sqlx::test(migrations = false)]
  async fn the_writer_sends_its_saves_in_batches_that_each_fit_in_a_frame(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    let count = i64::try_from(SAVES).expect("1926") + 1;
    let save = |character| Save { character: CharacterId(character), state: START, version: 1 };
    writer.send(&(1..=count).map(save).collect::<Vec<_>>());
    let batches = RefCell::new(Vec::new());
    let both = |_: &Writer, event: &Event| {
      if let Event::Saved { batch, result } = event {
        batches.borrow_mut().push((batch.saves.len(), result.as_ref().ok().copied()));
      }
      batches.borrow().len() == 2
    };
    assert_eq!(hear(&mut writer, &mut heard, both).await, None);
    assert_eq!(batches.into_inner(), [(SAVES, Some(1)), (1, Some(0))]);
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_that_a_newer_epoch_holds_ends_the_serve_of_its_links(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (endpoint, claimed) = meadow.claimed().await;
    meadow.supersede(9).await;
    let fault = serve(endpoint, claimed).await;
    assert_eq!(
      fault.to_string(),
      "layer 1:1 is held by a writer of epoch 9, which is newer than the epoch 1"
    );
  }

  #[sqlx::test(migrations = false)]
  async fn a_layer_that_holds_no_player_for_30_s_closes_and_its_writer_is_idle_again(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    assert!(writer.owed.is_empty(), "the store holds the state of the player already");
    let mut admin = meadow.admin().await;
    let () = admin.zadd("placed:1:1:1", 9, u64::MAX).await.expect("a place");
    for _ in 0..EMPTY_FOR {
      writer.tick();
    }
    let released = |_: &Writer, event: &Event| matches!(event, Event::Released(_));
    assert_eq!(hear(&mut writer, &mut heard, released).await, None, "a place holds the layer");
    assert_eq!(writer.empty, 0);
    let () = admin.del("placed:1:1:1").await.expect("the place taken");
    for _ in 0..EMPTY_FOR {
      writer.tick();
    }
    let fault = hear(&mut writer, &mut heard, released).await;
    assert_eq!(fault.as_deref(), Some("the layer closed, and its writer is idle again"));
    assert_eq!(hash(&meadow.redis, "layers:1").await, BTreeMap::new());
    assert_eq!(hash(&meadow.redis, "writer:1:1:1").await, BTreeMap::new());
    let refused = Event::Released(Err(claim::Error::Closed(LAYER)));
    writer.empty = 9;
    assert!(writer.hear(refused).is_none());
    assert_eq!(writer.empty, 0);
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_that_leaves_with_nothing_to_save_ends_its_presence_at_once(pool: PgPool) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    meadow.join(&mut writer).await;
    let entries = |_: &Writer, event: &Event| matches!(event, Event::Entries(_));
    assert_eq!(hear(&mut writer, &mut heard, joined(3)).await, None);
    let key = format!("presence:{}", meadow.account.0);
    assert_eq!(hash(&meadow.redis, &key).await.len(), 5);
    let logout = ToZone::Client(Relayed { link: Link(1), message: FromClient::Logout });
    assert!(writer.hear(Event::Message(GATE, logout)).is_none());
    loop {
      assert_eq!(hear(&mut writer, &mut heard, entries).await, None);
      if hash(&meadow.redis, &key).await.is_empty() {
        break;
      }
    }
    assert!(writer.owed.is_empty());
    let saved = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!(saved.version, 0);
  }

  /// Waits for `work`, which a fault of the code stops, so a test never hangs.
  async fn within<T>(work: impl Future<Output = T>) -> T {
    time::timeout(WAIT, work).await.expect("the work ends in time")
  }

  /// The payload of the next frame that the writer writes to a peer.
  async fn payload(frames: &mut UnboundedReceiver<Vec<u8>>) -> Vec<u8> {
    let bytes = within(frames.recv()).await.expect("a frame");
    let mut parts = Frames::default();
    parts.push(&bytes);
    parts.pop().expect("a frame").expect("a whole frame")
  }

  /// The answer that the writer writes to the writer of a layer that hands a player over.
  async fn answer(frames: &mut UnboundedReceiver<Vec<u8>>) -> FromNeighbour {
    decode(&payload(frames).await).expect("an answer")
  }

  /// Waits until the writer that holds `key` leases its layer with the epoch 1 of the keeper.
  async fn served(redis: &Redis, key: &str) {
    holds(redis, key, "epoch", "1").await;
  }

  /// Waits until the hash `key` of `redis` holds `value` in `field`.
  async fn holds(redis: &Redis, key: &str, field: &str, value: &str) {
    within(async {
      while hash(redis, key).await.get(field).map(String::as_str) != Some(value) {
        time::sleep(Duration::from_millis(20)).await;
      }
    })
    .await;
  }

  /// The address that a writer leases when its endpoint takes the links of other writers.
  fn dialable(endpoint: &Endpoint) -> String {
    format!("localhost:{}", endpoint.local_addr().expect("an address").port())
  }

  fn held(_: &Writer, event: &Event) -> bool {
    matches!(event, Event::Held { .. })
  }

  /// A writer of the meadow with Brom linked on a gateway, who walked one step past the east
  /// border and waits for the next zone.
  struct Border {
    writer: Writer,
    heard: UnboundedReceiver<Event>,
    frames: UnboundedReceiver<Vec<u8>>,
    _gateway: (Peer, Endpoint),
  }

  async fn border(meadow: &Meadow) -> Border {
    let (mut writer, heard) = meadow.writer().await;
    let (peer, endpoint, frames) = meadow.open(&mut writer, GATE).await;
    meadow.join(&mut writer).await;
    Border { writer, heard, frames, _gateway: (peer, endpoint) }
  }

  impl Border {
    /// Brom walks one step east, which takes him past the border.
    fn cross(&mut self) {
      walk(&mut self.writer, 1, 1);
    }

    /// Waits until the writer tells the client why Brom waits, and checks the message.
    async fn waits(&mut self) {
      assert_eq!(hear(&mut self.writer, &mut self.heard, held).await, None);
      let told = decode::<Relayed<ToClient>>(&payload(&mut self.frames).await);
      let why = Relayed { link: Link(1), message: ToClient::Full { zone: FOREST } };
      assert_eq!(told.expect("a message"), why);
      assert_eq!(self.writer.zone.count(), 1);
    }
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_at_the_border_waits_for_the_next_zone_and_crosses_once_a_writer_leases_a_layer(
    pool: PgPool,
  ) {
    let meadow = Meadow::at(pool, BORDER).await;
    let mut at_border = border(&meadow).await;
    at_border.cross();
    at_border.waits().await;
    assert_eq!(hash(&meadow.redis, "layers:1").await["2:1"], "0", "the placement opened a layer");

    let (endpoint, next) = meadow.claimed_for(FOREST_LAYER, dialable).await;
    let forest = tokio::spawn(serve(endpoint, next));
    served(&meadow.redis, "writer:1:2:1").await;
    let crossed = |_: &Writer, event: &Event| matches!(event, Event::Crossed { .. });
    assert_eq!(hear(&mut at_border.writer, &mut at_border.heard, crossed).await, None);
    let told = decode::<Relayed<ToClient>>(&payload(&mut at_border.frames).await);
    let to = ToClient::Moved { zone: FOREST, layer: 1 };
    assert_eq!(told.expect("a message"), Relayed { link: Link(1), message: to });
    assert_eq!(at_border.writer.zone.count(), 0);
    forest.abort();
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_waits_when_the_writer_of_the_next_layer_has_no_room(pool: PgPool) {
    let meadow = Meadow::at(pool, BORDER).await;
    let (endpoint, mut full) = meadow.claimed_for(FOREST_LAYER, dialable).await;
    full.zone = Zone::new(FOREST_LAYER, 0);
    let forest = tokio::spawn(serve(endpoint, full));
    served(&meadow.redis, "writer:1:2:1").await;
    let mut at_border = border(&meadow).await;
    at_border.cross();
    at_border.waits().await;
    forest.abort();
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_waits_when_the_lease_of_the_next_layer_names_no_address_to_dial(pool: PgPool) {
    let meadow = Meadow::at(pool, BORDER).await;
    let mut admin = meadow.admin().await;
    let () = admin.hset(keys::layers(REALM), keys::field(FOREST_LAYER), 0).await.expect("a layer");
    let lease = [(keys::ADDRESS, "[::1]:4433"), (keys::EPOCH, "1")];
    let () = admin.hset_multiple(keys::writer(REALM, FOREST_LAYER), &lease).await.expect("a lease");
    let mut at_border = border(&meadow).await;
    at_border.cross();
    at_border.waits().await;
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_waits_when_redis_refuses_the_placement(pool: PgPool) {
    let meadow = Meadow::at(pool, BORDER).await;
    let mut at_border = border(&meadow).await;
    let () = meadow.admin().await.hdel(keys::realm(REALM), keys::LAYER_CAP).await.expect("no cap");
    at_border.cross();
    at_border.waits().await;
  }

  /// Brom starts at `start`, walks along `heading` over the border into the zone of `target`, and
  /// its client follows: the writer of `target` holds him at `enters`.
  async fn crosses(pool: PgPool, start: State, heading: Angle, target: Layer, enters: Position) {
    let meadow = Meadow::at(pool, start).await;
    let (a_endpoint, a) = meadow.claimed_for(LAYER, dialable).await;
    let (b_endpoint, b) = meadow.claimed_for(target, dialable).await;
    let at = |endpoint: &Endpoint| endpoint.local_addr().expect("an address");
    let (a_at, b_at) = (at(&a_endpoint), at(&b_endpoint));
    let writers = [tokio::spawn(serve(a_endpoint, a)), tokio::spawn(serve(b_endpoint, b))];
    served(&meadow.redis, "writer:1:1:1").await;
    served(&meadow.redis, &format!("writer:1:{}:1", target.zone.0)).await;
    let gateways = meadow.world.gateway(true);
    let mut one = Peer::link(&gateways, a_at).await;
    one.greet().await;
    let mut two = Peer::link(&gateways, b_at).await;
    two.greet().await;
    let join = ToZone::Join { link: Link(1), account: meadow.account, character: meadow.character };
    one.tell(&join).await;
    let first = within(one.snapshot(Link(1))).await;
    let inputs = [1, 2, 3].map(|ahead| Input {
      tick: Tick(first.tick.0 + ahead),
      heading: Some(heading),
      view: Angle(0),
    });
    let intents = Intents { zone: MEADOW, layer: 1, ack: None, inputs };
    one.intents(&Relayed { link: Link(1), message: intents });
    let moved = decode::<Relayed<ToClient>>(&within(one.read()).await).expect("a message");
    let to = ToClient::Moved { zone: target.zone, layer: 1 };
    assert_eq!(moved, Relayed { link: Link(1), message: to });

    two.tell(&join).await;
    let arrived = within(two.snapshot(Link(1))).await;
    assert_eq!(arrived.entered.iter().map(|one| one.position).collect::<Vec<_>>(), [enters]);
    let kept = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!((kept.zone, kept.version, kept.state.position), (target.zone, 1, enters));
    let presence = hash(&meadow.redis, &format!("presence:{}", meadow.account.0)).await;
    let zone = target.zone.0.to_string();
    assert_eq!([&presence["zone"], &presence["layer"], &presence["epoch"]], [&zone, "1", "1"]);
    for writer in writers {
      writer.abort();
    }
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_that_walks_past_the_east_border_enters_a_layer_of_the_zone_east_of_it(
    pool: PgPool,
  ) {
    let at_start = Position { x: 0, y: 0, z: 10_000 };
    crosses(pool, BORDER, Angle(0), FOREST_LAYER, at_start).await;
  }

  #[sqlx::test(migrations = false)]
  async fn a_player_that_walks_past_the_north_border_enters_a_layer_of_the_zone_north_of_it(
    pool: PgPool,
  ) {
    let position = Position { x: 10_000, y: 0, z: SIDE_CM - 30 };
    let start = State { position, yaw: Angle(0), hit_points: 100 };
    let enters = Position { x: 10_000, y: 0, z: 0 };
    crosses(pool, start, Angle(16384), RIDGE_LAYER, enters).await;
  }

  /// The place of `who` in the first snapshot of the link 1 of `peer` that holds him, within 200
  /// snapshots.
  async fn sight(peer: &Peer, who: CharacterId) -> Position {
    let mut found = None;
    for _ in 0..200 {
      let snapshot = within(peer.snapshot(Link(1))).await;
      found = snapshot.entered.iter().find(|one| one.id == who).map(|one| one.position);
      if found.is_some() {
        break;
      }
    }
    found.expect("the other player in view")
  }

  #[sqlx::test(migrations = false)]
  async fn two_players_near_the_border_of_two_zones_see_each_other_across_it(pool: PgPool) {
    let near =
      |x| State { position: Position { x, y: 0, z: 10_000 }, yaw: Angle(0), hit_points: 100 };
    let meadow = Meadow::at(pool, near(SIDE_CM - 300)).await;
    let cael = NewCharacter {
      account: meadow.account,
      realm: REALM,
      name: "Cael",
      zone: FOREST,
      state: near(300),
    };
    let cael = meadow.store.create_character(&cael).await.expect("Cael");
    let (a_endpoint, a) = meadow.claimed_for(LAYER, dialable).await;
    let (b_endpoint, b) = meadow.claimed_for(FOREST_LAYER, dialable).await;
    let at = |endpoint: &Endpoint| endpoint.local_addr().expect("an address");
    let (a_at, b_at) = (at(&a_endpoint), at(&b_endpoint));
    let writers = [tokio::spawn(serve(a_endpoint, a)), tokio::spawn(serve(b_endpoint, b))];
    served(&meadow.redis, "writer:1:1:1").await;
    served(&meadow.redis, "writer:1:2:1").await;
    let gateways = meadow.world.gateway(true);
    let mut one = Peer::link(&gateways, a_at).await;
    one.greet().await;
    let mut two = Peer::link(&gateways, b_at).await;
    two.greet().await;
    let account = meadow.account;
    one.tell(&ToZone::Join { link: Link(1), account, character: meadow.character }).await;
    two.tell(&ToZone::Join { link: Link(1), account, character: cael }).await;
    let in_meadow = Position { x: SIDE_CM + 300, y: 0, z: 10_000 };
    assert_eq!(sight(&one, cael).await, in_meadow);
    let in_forest = Position { x: -300, y: 0, z: 10_000 };
    assert_eq!(sight(&two, meadow.character).await, in_forest);
    for writer in writers {
      writer.abort();
    }
  }

  /// The second layer of the meadow.
  const SECOND: Layer = Layer { zone: MEADOW, number: 2 };

  /// The cap of a layer in the tests of the drain: a layer of 1 player holds a quarter of it.
  const SMALL: usize = 4;

  /// Gives the layers of the realm the cap [`SMALL`], in the directory and in `claimed`, which
  /// holds `layer`.
  async fn small(meadow: &Meadow, claimed: &mut Claimed, layer: Layer) {
    let mut admin = meadow.admin().await;
    let () = admin.hset(keys::realm(REALM), keys::LAYER_CAP, SMALL).await.expect("a cap");
    let across = meadow.store.neighbours(layer.zone).await.expect("the neighbours");
    claimed.zone = Zone::new(layer, SMALL).beside(across);
  }

  /// A writer of the second layer of the meadow with the cap [`SMALL`], with Brom on a gateway.
  async fn sparse(meadow: &Meadow) -> (Border, Endpoint) {
    let (endpoint, mut second) = meadow.claimed_for(SECOND, dialable).await;
    small(meadow, &mut second, SECOND).await;
    let (events, heard) = unbounded_channel();
    let mut writer = Writer::new(second, events);
    let (peer, gateway, frames) = meadow.open(&mut writer, GATE).await;
    meadow.join(&mut writer).await;
    (Border { writer, heard, frames, _gateway: (peer, gateway) }, endpoint)
  }

  /// Runs `ticks` ticks of `writer`, in which no player sends an input.
  fn idle(writer: &mut Writer, ticks: u32) {
    for _ in 0..ticks {
      writer.tick();
    }
  }

  fn drained(_: &Writer, event: &Event) -> bool {
    matches!(event, Event::Drained(_))
  }

  fn stayed(_: &Writer, event: &Event) -> bool {
    matches!(event, Event::Stayed { .. })
  }

  async fn draining(meadow: &Meadow) -> Vec<String> {
    meadow.admin().await.smembers(keys::draining(REALM)).await.expect("a set")
  }

  #[sqlx::test(migrations = false)]
  async fn a_layer_that_drains_moves_its_player_to_another_layer_of_its_zone_and_closes(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (endpoint, mut first) = meadow.claimed_for(LAYER, dialable).await;
    small(&meadow, &mut first, LAYER).await;
    let target = tokio::spawn(serve(endpoint, first));
    served(&meadow.redis, "writer:1:1:1").await;
    let (mut source, _endpoint) = sparse(&meadow).await;
    let places = [(u64::MAX, 8), (u64::MAX, 9)];
    let () = meadow.admin().await.zadd_multiple("placed:1:1:1", &places).await.expect("places");
    idle(&mut source.writer, LOW_FOR);
    assert_eq!(hear(&mut source.writer, &mut source.heard, drained).await, None);
    assert_eq!(draining(&meadow).await, ["1:2"]);
    assert!(!source.writer.zone.low());
    idle(&mut source.writer, ANNOUNCE_EVERY);
    let crossed = |_: &Writer, event: &Event| matches!(event, Event::Crossed { .. });
    assert_eq!(hear(&mut source.writer, &mut source.heard, crossed).await, None);
    let told = decode::<Relayed<ToClient>>(&payload(&mut source.frames).await);
    let to = ToClient::Moved { zone: MEADOW, layer: 1 };
    assert_eq!(told.expect("a message"), Relayed { link: Link(1), message: to });
    assert_eq!(source.writer.zone.count(), 0);
    holds(&meadow.redis, "layers:1", "1:1", "1").await;
    let key = format!("presence:{}", meadow.account.0);
    holds(&meadow.redis, &key, "layer", "1").await;
    let kept = meadow.store.load(&[meadow.character]).await.expect("a save")[0];
    assert_eq!((kept.zone, kept.version, kept.state), (MEADOW, 1, START));
    let held: Vec<String> =
      meadow.admin().await.zrange("placed:1:1:1", 0, -1).await.expect("places");
    assert_eq!(held, ["8", "9"], "layer 1 holds Brom and 2 places, 3 of its cap of 4");

    idle(&mut source.writer, EMPTY_FOR);
    let released = |_: &Writer, event: &Event| matches!(event, Event::Released(_));
    let fault = hear(&mut source.writer, &mut source.heard, released).await;
    assert_eq!(fault.as_deref(), Some("the layer closed, and its writer is idle again"));
    let layers = hash(&meadow.redis, "layers:1").await;
    assert_eq!(layers, BTreeMap::from([("1:1".to_owned(), "1".to_owned())]));
    assert_eq!(draining(&meadow).await, Vec::<String>::new());
    target.abort();
  }

  #[sqlx::test(migrations = false)]
  async fn a_layer_drains_once_another_layer_has_room_and_a_player_that_no_layer_takes_stays(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (mut source, _endpoint) = sparse(&meadow).await;
    idle(&mut source.writer, LOW_FOR);
    assert_eq!(hear(&mut source.writer, &mut source.heard, drained).await, None);
    assert!(source.writer.zone.low(), "no other layer has room for Brom");
    let mut admin = meadow.admin().await;
    let () = admin.hset(keys::layers(REALM), keys::field(LAYER), 0).await.expect("a layer");
    let lease = [(keys::ADDRESS, "[::1]:4433"), (keys::EPOCH, "1")];
    let () = admin.hset_multiple(keys::writer(REALM, LAYER), &lease).await.expect("a lease");
    idle(&mut source.writer, ANNOUNCE_EVERY);
    assert_eq!(hear(&mut source.writer, &mut source.heard, drained).await, None);
    assert_eq!(draining(&meadow).await, ["1:2"]);
    idle(&mut source.writer, ANNOUNCE_EVERY);
    assert_eq!(hear(&mut source.writer, &mut source.heard, stayed).await, None);
    assert_eq!(source.writer.zone.count(), 1, "no address of the lease of layer 1 answers");
    let () = admin.hset(keys::layers(REALM), keys::field(LAYER), SMALL).await.expect("a count");
    idle(&mut source.writer, ANNOUNCE_EVERY);
    assert_eq!(hear(&mut source.writer, &mut source.heard, stayed).await, None);
    assert_eq!(source.writer.zone.count(), 1, "layer 1 is full");
    assert_eq!(hash(&meadow.redis, "layers:1").await["1:3"], "0", "the placement opened a layer");
    let refused = Event::Drained(Err(claim::Error::Closed(SECOND)));
    assert!(source.writer.hear(refused).is_none());
  }

  #[sqlx::test(migrations = false)]
  async fn a_writer_answers_a_hand_over_after_the_keeper_saved_the_player_in_its_zone(
    pool: PgPool,
  ) {
    let meadow = Meadow::new(pool).await;
    let (mut writer, mut heard) = meadow.writer().await;
    let (_peer, _endpoint, mut answers) = meadow.open(&mut writer, Gateway(5)).await;
    let state = Entity {
      id: meadow.character,
      position: Position { x: 0, y: 0, z: 10_000 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    };
    let transfer = |from, state| {
      let account = meadow.account;
      Event::Message(from, ToZone::Neighbour(ToNeighbour::Transfer { account, state, version: 0 }))
    };
    let accepted = FromNeighbour::Accepted { character: meadow.character };
    assert!(writer.hear(transfer(Gateway(5), state)).is_none());
    assert_eq!((writer.zone.count(), writer.pending.len()), (1, 1));
    assert_eq!(answers.try_recv().ok(), None, "the answer waits for the save");
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    assert_eq!(answer(&mut answers).await, accepted);
    assert_eq!(meadow.store.load(&[meadow.character]).await.expect("a save")[0].version, 1);

    assert!(writer.hear(transfer(Gateway(5), state)).is_none());
    assert_eq!(answer(&mut answers).await, accepted);
    writer.zone = Zone::new(LAYER, 0);
    assert!(writer.hear(transfer(Gateway(5), state)).is_none());
    let full = FromNeighbour::Full { character: meadow.character };
    assert_eq!(answer(&mut answers).await, full);
    writer.zone = Zone::new(LAYER, 500);
    assert!(writer.hear(transfer(Gateway(9), state)).is_none());
    assert_eq!(hear(&mut writer, &mut heard, saved_event).await, None);
    assert_eq!(answers.try_recv().ok(), None, "no link of Gateway(9) stands to answer on");
    let gone = CharacterId(99);
    assert!(writer.hear(Event::Crossed { character: gone, to: FOREST_LAYER }).is_none());
    assert!(writer.hear(Event::Held { character: gone, zone: FOREST }).is_none());
  }
}
