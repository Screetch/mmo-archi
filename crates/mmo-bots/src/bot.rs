//! One bot: it logs in, waits in the queue, attaches to a gateway, and walks as a player does.

use std::collections::{HashMap, HashSet};
use std::iter;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mmo_client::{Account, Down, Fault, Gap, Place, Play, Predictor, Trust, Up, World, enter};
use mmo_collide::{Terrain, ground};
use mmo_protocol::{
  Angle, CharacterId, Intents, Layer, Refusal, Snapshot, ToClient, Token, ZoneId, encode,
};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep};

use crate::rng::Rng;
use crate::seed::Bot;

/// The time of one tick of the game.
const TICK: Duration = Duration::from_millis(50);

/// A bot picks a new walk each 20 ticks (1 s): one of 8 headings, or a stop.
const TURN: u64 = 20;

/// A bot says a line in the chat each 400 ticks (20 s), and at its first tick.
const SAY: u64 = 400;

/// The wait before a bot that lost its link attaches again.
const RETRY: Duration = Duration::from_millis(200);

/// What the bots of one run share.
pub(crate) struct Shared {
  pub(crate) trust: Trust,
  pub(crate) gateway: String,
  pub(crate) zone: ZoneId,
  pub(crate) seed: u64,
  /// The count of the bots that play, or that failed before they played.
  pub(crate) ready: AtomicUsize,
  /// A bot that loses its link after it played attaches again with its token, as a client does.
  pub(crate) reconnect: bool,
  /// The count of the bots that cross a border. Each other bot logs in after each of them played.
  pub(crate) crossers: usize,
  /// The realm of each character of the bots, by its id.
  pub(crate) realms: HashMap<i64, u32>,
  /// The count of the first bots that log out when `leave` changes.
  pub(crate) leavers: u32,
  /// The signal for the leavers to log out.
  pub(crate) leave: watch::Receiver<bool>,
  /// The logins that start each second.
  pub(crate) ramp: u32,
  /// The count of the bots that play, or that failed, after which the rush logs in.
  pub(crate) rushing: usize,
  /// The count of the bots of the rush that got a place in the queue.
  pub(crate) rushed: AtomicUsize,
}

/// What one bot measured.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
  /// The place that the login service first gave in the queue.
  pub(crate) place: Option<Place>,
  /// Each place that the login service gave in the queue, with the time that it came.
  pub(crate) places: Vec<(Instant, Place)>,
  /// True when the measure ended while the bot waited in the queue: it is no fault.
  pub(crate) queued: bool,
  /// The time that the bot ended.
  pub(crate) ended: Option<Instant>,
  /// What the login service answered when the bot asked for a new character before the rush.
  pub(crate) probe: Option<Probe>,
  /// The time without a snapshot of each loss of the link that ended with the player in play again.
  pub(crate) recoveries: Vec<Duration>,
  /// The losses of the link that ended on another layer than the one that the bot left.
  pub(crate) relocated: u32,
  /// True when the bot lost its link and the measure ended before its player showed again.
  pub(crate) away: bool,
  /// The time from the start of the login to the session.
  pub(crate) waited: Option<Duration>,
  /// The time from the start of the login to the first snapshot that shows the player.
  pub(crate) entered: Option<Duration>,
  pub(crate) joined: Option<Instant>,
  /// The realm that the bot plays in.
  pub(crate) realm: u32,
  pub(crate) role: Role,
  pub(crate) end: End,
  /// Each move of the bot to another layer, which the gateway tells it, from the layer that it
  /// left to the layer that it entered.
  pub(crate) crossings: Vec<(Layer, Layer)>,
  /// The characters of another realm that a snapshot gave to the bot.
  pub(crate) strangers: HashSet<i64>,
  /// What the bot did last: it waited for the crossers, logged in, attached to the gateway, or
  /// waited for the first snapshot that shows its player.
  pub(crate) phase: &'static str,
  /// The time from the first snapshot that shows the player to the end.
  pub(crate) played: Duration,
  /// The bytes of the datagrams up and down, with no header of IP, UDP or QUIC.
  pub(crate) up: usize,
  pub(crate) down: usize,
  pub(crate) snapshots: u64,
  /// The snapshots that the world refused.
  pub(crate) gaps: u64,
  /// The largest snapshot, in bytes.
  pub(crate) largest: usize,
  pub(crate) fault: Option<String>,
}

/// What the login service answered when the bot before the rush asked for a new character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
  /// The login service refused the character for this cause.
  Refused(Refusal),
  /// The login service made the character.
  Made,
}

/// What a bot does in a run, beside its walk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Role {
  #[default]
  Walker,
  /// It walks east from near the border of its zone, to cross it.
  Crosser,
  /// It logs out at the signal of the run, as a player who ends the game does.
  Leaver,
}

/// Where the player of a bot stands when the run ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum End {
  /// No snapshot showed the player since its last move or its last loss of the link.
  #[default]
  Unseen,
  /// A snapshot showed the player after its last move and its last loss of the link.
  Playing,
  /// The bot logged out. It then sends nothing more, and the gateway ends its link, which ends the
  /// bot with no fault.
  LoggedOut,
}

#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Fault(#[from] Fault),
  #[error("the gateway sends {0:?}")]
  Message(ToClient),
}

impl Outcome {
  /// Notes each entity of the `snapshot` that is the character of a bot of another realm: a realm
  /// is a world of its own, so no snapshot may show one.
  fn note_strangers(&mut self, realms: &HashMap<i64, u32>, snapshot: &Snapshot) {
    let others = snapshot.entered.iter().map(|entity| entity.id.0);
    let others = others.filter(|id| realms.get(id).is_some_and(|realm| *realm != self.realm));
    self.strangers.extend(others);
  }
}

/// The body of the player, and what it knows of the world.
struct Walk {
  character: CharacterId,
  /// The layer of the player, which the gateway names first, before the snapshot that shows the
  /// player and so before the first datagram of the bot.
  layer: Layer,
  /// True when the bot walks east and never turns.
  crosser: bool,
  rng: Rng,
  world: World,
  body: Option<Predictor>,
  heading: Option<Angle>,
  ticks: u64,
  /// The time of the last snapshot.
  last: Option<Instant>,
  /// The time of the last snapshot before a loss of the link, and the layer that the bot held.
  lost: Option<(Instant, Layer)>,
}

#[expect(
  clippy::expect_used,
  reason = "the ground of a zone is valid, as the test of mmo-collide proves"
)]
fn terrain() -> Terrain {
  Terrain::new(&ground()).expect("the ground is valid")
}

impl Walk {
  fn new(shared: &Shared, bot: &Bot) -> Self {
    let rng = Rng::new(shared.seed.wrapping_mul(0x9e37_79b9) ^ u64::from(bot.index));
    let layer = Layer { zone: shared.zone, number: 0 };
    Self {
      character: bot.character,
      layer,
      crosser: bot.crosser,
      rng,
      world: World::default(),
      body: None,
      heading: None,
      ticks: 0,
      last: None,
      lost: None,
    }
  }

  /// Counts a snapshot of the gateway in `out`, and takes it. The first snapshot that shows the
  /// player says that the bot plays, and a later one ends a loss of the link.
  fn count(&mut self, shared: &Shared, out: &mut Outcome, started: Instant, snapshot: &Snapshot) {
    let size = encode(snapshot).len();
    (out.snapshots, out.down, out.largest) =
      (out.snapshots + 1, out.down + size, out.largest.max(size));
    out.note_strangers(&shared.realms, snapshot);
    match self.hear(snapshot) {
      Ok(true) => {
        if out.joined.is_none() {
          (out.entered, out.joined) = (Some(started.elapsed()), Some(Instant::now()));
          shared.ready.fetch_add(1, Ordering::Relaxed);
        }
        self.found(out);
      }
      Ok(false) => {}
      Err(_) => out.gaps += 1,
    }
  }

  /// Takes a snapshot. It gives true when the snapshot shows the player. A snapshot that starts
  /// from one that the world does not hold empties the world, so the writer sends a full snapshot.
  fn hear(&mut self, snapshot: &Snapshot) -> Result<bool, Gap> {
    self.last = Some(Instant::now());
    self.world.apply(snapshot).inspect_err(|gap| {
      if matches!(gap, Gap::Base { .. }) {
        self.world.clear();
      }
    })?;
    let Some(entity) = self.world.entity(self.character) else {
      return Ok(false);
    };
    match self.body.as_mut() {
      Some(body) => body.correct(snapshot.tick, entity),
      None => self.body = Some(Predictor::start(terrain(), snapshot.tick, entity)),
    }
    Ok(true)
  }

  /// The link ended. The walk forgets the world and its body: the next writer sends the world in
  /// full, and the bot sends nothing before a snapshot shows its player again.
  fn lose(&mut self, out: &mut Outcome) {
    self.lost = self.lost.or(self.last.map(|at| (at, self.layer)));
    self.world.clear();
    self.body = None;
    out.away = true;
  }

  /// The first snapshot that shows the player after a loss ends it. The outcome keeps the time
  /// without a snapshot, and whether the bot came back to another layer.
  fn found(&mut self, out: &mut Outcome) {
    let Some((at, layer)) = self.lost.take() else {
      return;
    };
    out.recoveries.push(at.elapsed());
    out.relocated += u32::from(layer != self.layer);
    out.away = false;
  }

  /// The gateway names the layer of the player. After the first time, a new layer is a crossing:
  /// the walk forgets the world and its body, as a client does, since the next writer sends the
  /// world in full.
  fn moved(&mut self, to: Layer, out: &mut Outcome) {
    if self.layer.number != 0 && self.layer != to {
      out.crossings.push((self.layer, to));
      self.world.clear();
      self.body = None;
    }
    self.layer = to;
  }

  /// Runs one tick of the walk, and gives the datagram of the inputs. A bot sends nothing before
  /// the first snapshot that shows its player.
  fn tick(&mut self) -> Option<Intents> {
    let body = self.body.as_mut()?;
    if self.crosser {
      self.heading = Some(Angle(0));
    } else if self.ticks.is_multiple_of(TURN) {
      self.heading = u16::try_from(self.rng.below(9) * 8192).ok().map(Angle);
    }
    self.ticks += 1;
    body.step(self.heading, self.heading.unwrap_or(Angle(0)));
    let Layer { zone, number: layer } = self.layer;
    body.recent().map(|inputs| Intents { zone, layer, ack: self.world.ack(), inputs })
  }
}

fn say() -> Up {
  Up::Say { channel: "world".to_owned(), text: "hello".to_owned() }
}

/// Attaches to the gateway with `token` until it gives a link, with a short wait after each
/// refusal, or until the measure ends.
async fn again(shared: &Shared, token: Token, stop: &mut watch::Receiver<bool>) -> Option<Play> {
  loop {
    let tried = tokio::select! {
      got = Play::attach(&shared.trust, &shared.gateway, token) => got,
      _ = stop.changed() => return None,
    };
    if let Ok(link) = tried {
      return Some(link);
    }
    tokio::select! {
      () = sleep(RETRY) => {}
      _ = stop.changed() => return None,
    }
  }
}

/// Runs one tick of the walk and sends its inputs, unless the bot logged out.
async fn send_walk(walk: &mut Walk, out: &mut Outcome, link: &mut Play) {
  if out.end != End::LoggedOut
    && let Some(intents) = walk.tick()
  {
    out.up += encode(&intents).len();
    let chat = (walk.ticks % SAY == 1).then(say);
    for up in iter::once(Up::Intents(intents)).chain(chat) {
      // A dead link ends `link.next()` at once, so a send that fails ends nothing here.
      drop(link.send(up).await);
    }
  }
}

/// Waits until the crossers play, unless the bot is one, and a bot of the rush until each other
/// bot plays, and then for its rank at the pace of the ramp. It gives false when the run ended
/// first.
async fn after_others(
  shared: &Shared,
  bot: &Bot,
  stop: &mut watch::Receiver<bool>,
  out: &mut Outcome,
) -> bool {
  let (phase, others) = match bot.rush {
    Some(_) => ("rush", shared.rushing),
    None if bot.crosser => ("crossers", 0),
    None => ("crossers", shared.crossers),
  };
  while shared.ready.load(Ordering::Relaxed) < others {
    out.phase = phase;
    tokio::select! {
      () = sleep(RETRY) => {}
      _ = stop.changed() => return false,
    }
  }
  let rank = bot.rush.unwrap_or_default();
  sleep(Duration::from_secs_f64(f64::from(rank) / f64::from(shared.ramp.max(1)))).await;
  true
}

/// Keeps a place that the login service gave the bot in the queue, and counts the first place of
/// a bot of the rush.
fn note(shared: &Shared, bot: &Bot, out: &mut Outcome, place: Place) {
  if bot.rush.is_some() && out.places.is_empty() {
    shared.rushed.fetch_add(1, Ordering::Relaxed);
  }
  out.place.get_or_insert(place);
  out.places.push((Instant::now(), place));
}

/// Asks for the new character of the probe, when the bot has one, and keeps the answer. A bot
/// that the realm refused logs in again with its own character in its slot.
async fn probe(shared: &Shared, bot: &Bot, out: &mut Outcome) -> Result<(), Error> {
  let Some(name) = &bot.probe else {
    return Ok(());
  };
  out.phase = "probe";
  let account = Account { character: name.clone(), ..bot.account.clone() };
  let mut queued = |place| note(shared, bot, out, place);
  let tried = enter(&shared.trust, &bot.login, &account, &mut queued).await;
  out.probe = Some(match tried {
    Ok(_) => Probe::Made,
    Err(Fault::Refused(why)) => Probe::Refused(why),
    Err(fault) => return Err(fault.into()),
  });
  Ok(())
}

async fn play(
  shared: &Shared,
  bot: &Bot,
  mut stop: watch::Receiver<bool>,
  out: &mut Outcome,
) -> Result<bool, Error> {
  // The crossers log in first: they fill the first layer of their realm, so a crosser enters the
  // layer with its number in the next zone, which the first crossing opens.
  if !after_others(shared, bot, &mut stop, out).await {
    return Ok(false);
  }
  let started = Instant::now();
  tokio::select! {
    probed = probe(shared, bot, out) => probed?,
    _ = stop.changed() => return Ok(true),
  }
  out.phase = "login";
  let queued = &mut |place| note(shared, bot, out, place);
  let token = tokio::select! {
    session = enter(&shared.trust, &bot.login, &bot.account, queued) => session?.token,
    _ = stop.changed() => return Ok(true),
  };
  out.waited = Some(started.elapsed());
  out.phase = "attach";
  let mut link = Play::attach(&shared.trust, &shared.gateway, token).await?;
  out.phase = "snapshot";
  let mut walk = Walk::new(shared, bot);
  let mut leave = shared.leave.clone();
  let mut clock = interval(TICK);
  clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
  loop {
    tokio::select! {
      _ = clock.tick() => {
        send_walk(&mut walk, out, &mut link).await;
      }
      down = link.next() => match down {
        Err(_) if out.end == End::LoggedOut => return Ok(false),
        Err(_) if shared.reconnect && out.joined.is_some() => {
          walk.lose(out);
          match again(shared, token, &mut stop).await {
            Some(next) => link = next,
            None => return Ok(false),
          }
        }
        down => match down? {
        Down::Snapshot(snapshot) => walk.count(shared, out, started, &snapshot),
        Down::Message(ToClient::Moved { zone, layer }) => {
          walk.moved(Layer { zone, number: layer }, out);
        }
        Down::Message(ToClient::Line { .. } | ToClient::Full { .. }) => {}
        Down::Message(other) => return Err(Error::Message(other)),
        },
      },
      _ = leave.changed(), if out.role == Role::Leaver && out.end != End::LoggedOut => {
        out.end = End::LoggedOut;
        // A dead link ends `link.next()` at once, so a send that fails ends nothing here.
        drop(link.send(Up::Logout).await);
      }
      _ = stop.changed() => {
        if out.end != End::LoggedOut && walk.body.is_some() {
          out.end = End::Playing;
        }
        return Ok(false);
      }
    }
  }
}

/// Runs one bot until `stop` changes, and gives what it measured. A fault ends the bot, and the
/// outcome holds it. A bot that the end of the measure finds in the queue says so, and has no
/// fault. A bot leaves as a player who closes the game does: it closes its link, and the layer
/// writer keeps its player for 30 s.
pub(crate) async fn run(shared: &Shared, bot: &Bot, stop: watch::Receiver<bool>) -> Outcome {
  let role = match (bot.index < shared.leavers, bot.crosser) {
    (true, _) => Role::Leaver,
    (false, true) => Role::Crosser,
    (false, false) => Role::Walker,
  };
  let mut out = Outcome { realm: bot.realm, role, ..Outcome::default() };
  let ended = play(shared, bot, stop, &mut out).await;
  out.queued = matches!(ended, Ok(true));
  out.fault = ended.err().map(|error| error.to_string());
  out.ended = Some(Instant::now());
  out.played = out.joined.map(|at| at.elapsed()).unwrap_or_default();
  if out.joined.is_none() {
    shared.ready.fetch_add(1, Ordering::Relaxed);
  }
  out
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Duration;

  use mmo_client::World;
  use mmo_client::testing::{Fake, hold};
  use mmo_client::{Account, Place, Trust};
  use std::future::Future;

  use mmo_client::testing::Peer;
  use mmo_protocol::{
    AccountId, Angle, Attach, Character, CharacterId, FromClient, FromLogin, Layer, Password,
    Refusal, Snapshot, ToClient, ToLogin, Token, ZoneId, encode, frame,
  };
  use tokio::sync::{oneshot, watch};
  use tokio::time::{sleep, timeout};

  use super::{End, Outcome, Probe, Role, Shared, TURN, Walk, again, run};
  use crate::fixture::{authority, entity, login_service, send, snapshot};
  use crate::rng::Rng;
  use crate::seed::Bot;

  fn bot(login: &Fake) -> Bot {
    let account = Account {
      name: "ada".to_owned(),
      password: Password("a long password".to_owned()),
      character: "Brom".to_owned(),
      create: false,
    };
    Bot {
      index: 0,
      realm: 1,
      login: login.address.clone(),
      id: AccountId(7),
      account,
      character: CharacterId(42),
      crosser: false,
      rush: None,
      probe: None,
    }
  }

  fn shared_by(login: &Fake, gateway: &Fake) -> Shared {
    let (_folder, path) = authority(&[login, gateway]);
    Shared {
      trust: Trust::from_pem(&path).expect("a trust"),
      gateway: gateway.address.clone(),
      zone: ZoneId(1),
      seed: 1,
      ready: AtomicUsize::new(0),
      reconnect: false,
      crossers: 0,
      realms: HashMap::new(),
      leavers: 0,
      leave: watch::channel(false).1,
      ramp: 50,
      rushing: 0,
      rushed: AtomicUsize::new(0),
    }
  }

  /// A login service that answers the first login of a probe with `first`, and then gives the
  /// character Brom and a session, as `login_service` does.
  fn probed<S, F>(first: S) -> Fake
  where
    S: FnOnce(Peer) -> F + Send + 'static,
    F: Future<Output = Peer> + Send + 'static,
  {
    Fake::start_twice(first, |mut peer| async move {
      peer.read::<ToLogin>().await;
      let character = Character { id: CharacterId(42), name: "Brom".to_owned() };
      peer.write(&FromLogin::Characters(vec![character])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(42) });
      peer.write(&FromLogin::Session { token: Token([5; 32]) }).await;
      peer
    })
  }

  /// Runs `bot` against `login` and a gateway that shows its player once, and stops the run once
  /// the bot plays.
  async fn plays(login: Fake, bot: Bot, shared: impl FnOnce(Shared) -> Shared) -> (Outcome, usize) {
    let (seen, played) = oneshot::channel();
    let gateway = Fake::start(move |mut peer| async move {
      peer.read::<Attach>().await;
      peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 1 }).await;
      send(&peer, &snapshot(10, None, vec![entity(42)]));
      seen.send(()).expect("the test waits for the play");
      peer
    });
    let shared = shared(shared_by(&login, &gateway));
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      played.await.expect("a bot that plays");
      while shared.ready.load(Ordering::Relaxed) == 0 {
        sleep(Duration::from_millis(20)).await;
      }
      stop.send_replace(true);
    };
    let (out, ()) = tokio::join!(run(&shared, &bot, stopped), stopping);
    gateway.end().await;
    login.end().await;
    (out, shared.rushed.load(Ordering::Relaxed))
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_bot_before_the_rush_asks_for_a_new_character_then_plays_its_own() {
    let refusing = probed(|mut peer| async move {
      peer.read::<ToLogin>().await;
      let place = FromLogin::Queued { position: 0, length: 1, wait: 1, resume: Token([1; 32]) };
      peer.write(&place).await;
      let character = Character { id: CharacterId(42), name: "Brom".to_owned() };
      peer.write(&FromLogin::Characters(vec![character])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::NewCharacter { name: "Nada".to_owned() });
      peer.write(&FromLogin::Refused(Refusal::Full)).await;
      peer
    });
    let making = probed(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Characters(vec![])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::NewCharacter { name: "Nada".to_owned() });
      let character = Character { id: CharacterId(43), name: "Nada".to_owned() };
      peer.write(&FromLogin::Characters(vec![character])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(43) });
      peer.write(&FromLogin::Session { token: Token([6; 32]) }).await;
      peer
    });
    let mut probes = Vec::new();
    for login in [refusing, making] {
      let asking = Bot { probe: Some("Nada".to_owned()), ..bot(&login) };
      let (out, _) = plays(login, asking, |shared| shared).await;
      let heard = out.places.iter().map(|(_, place)| place.position).collect::<Vec<_>>();
      probes.push((out.probe, out.fault, out.end, heard));
    }
    assert_eq!(
      probes,
      [
        (Some(Probe::Refused(Refusal::Full)), None, End::Playing, vec![0]),
        (Some(Probe::Made), None, End::Playing, vec![]),
      ]
    );
    let garbling = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write_raw(&frame(&[0xff]).expect("a frame")).await;
      peer
    });
    let idle = Fake::start(hold);
    let shared = shared_by(&garbling, &idle);
    let (_stop, stopped) = watch::channel(false);
    let broken = Bot { probe: Some("Nada".to_owned()), ..bot(&garbling) };
    let out = run(&shared, &broken, stopped).await;
    let ended = (out.probe, out.fault.as_deref(), out.phase);
    let garbled = "the bytes are not a message of this version of the protocol";
    assert_eq!(ended, (None, Some(garbled), "probe"));
    garbling.end().await;
    let holding = Fake::start(hold);
    let shared = shared_by(&holding, &idle);
    let (stop, stopped) = watch::channel(false);
    let waiting = Bot { probe: Some("Nada".to_owned()), ..bot(&holding) };
    let stopping = async {
      sleep(Duration::from_millis(300)).await;
      stop.send_replace(true);
    };
    let (out, ()) = tokio::join!(run(&shared, &waiting, stopped), stopping);
    assert_eq!((out.probe, out.phase, out.queued, out.fault), (None, "probe", true, None));
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_of_the_rush_logs_in_once_each_other_bot_plays_and_counts_its_first_place() {
    let login = login_service("Brom", 42);
    let late = Bot { rush: Some(1), ..bot(&login) };
    let (out, counted) = plays(login, late, |shared| Shared { ramp: 100, ..shared }).await;
    assert_eq!((out.fault, out.end, out.places.len(), counted), (None, End::Playing, 1, 1));
    let login = Fake::start(hold);
    let idle = Fake::start(hold);
    let shared = Shared { rushing: 1, ..shared_by(&login, &idle) };
    let (stop, stopped) = watch::channel(false);
    let waiting = Bot { rush: Some(0), ..bot(&login) };
    let stopping = async {
      sleep(Duration::from_millis(300)).await;
      stop.send_replace(true);
    };
    let (out, ()) = tokio::join!(run(&shared, &waiting, stopped), stopping);
    assert_eq!((out.phase, out.queued, out.fault), ("rush", false, None));
  }

  /// A full snapshot, a late one, one that starts from a snapshot that the bot lacks, one that
  /// does not show the player, and a full one again.
  fn sent() -> [Snapshot; 5] {
    [
      snapshot(10, None, vec![entity(42)]),
      snapshot(10, None, vec![entity(42)]),
      snapshot(12, Some(5), vec![]),
      snapshot(13, None, vec![entity(7)]),
      snapshot(14, None, vec![entity(42)]),
    ]
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_enters_walks_says_a_line_and_ends_when_it_is_stopped() {
    let login = login_service("Brom", 42);
    let (done, finished) = oneshot::channel();
    let gateway = Fake::start(move |mut peer| async move {
      assert_eq!(peer.read::<Attach>().await, Attach { session: Token([5; 32]) });
      peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 2 }).await;
      peer.write(&ToClient::Full { zone: ZoneId(2) }).await;
      let line = ToClient::Line {
        channel: "world".to_owned(),
        from: "Ada".to_owned(),
        text: "hi".to_owned(),
      };
      peer.write(&line).await;
      // The bot reads a datagram before a stream, so give it the time to read the line first.
      sleep(Duration::from_millis(300)).await;
      for one in &sent() {
        send(&peer, one);
      }
      let line = FromClient::Say { channel: "world".to_owned(), text: "hello".to_owned() };
      assert_eq!(peer.read::<FromClient>().await, line);
      // A bot that read the datagrams first still has the line on its stream: give it the time.
      sleep(Duration::from_millis(300)).await;
      done.send(()).expect("the test waits for the line");
      peer
    });
    let shared = shared_by(&login, &gateway);
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      finished.await.expect("a line of chat");
      stop.send_replace(true);
    };
    let walker = bot(&login);
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    let sizes: Vec<usize> = sent().iter().map(|one| encode(one).len()).collect();
    assert_eq!(out.fault, None);
    assert_eq!(out.place, Some(Place { position: 3, length: 9, wait: 1 }));
    let heard: Vec<Place> = out.places.iter().map(|(_, place)| *place).collect();
    assert_eq!(heard, [Place { position: 3, length: 9, wait: 1 }]);
    assert!(!out.queued && out.ended.is_some());
    assert_eq!((out.snapshots, out.gaps), (5, 2));
    assert_eq!(
      (out.down, out.largest),
      (sizes.iter().sum(), sizes.iter().copied().max().unwrap_or(0))
    );
    assert!(out.up > 0 && out.waited.is_some() && out.entered.is_some() && out.joined.is_some());
    assert_eq!(shared.ready.load(Ordering::Relaxed), 1);
    assert_eq!((out.role, out.end), (Role::Walker, End::Playing));
    gateway.end().await;
    login.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_leaver_logs_out_at_the_signal_of_the_run_and_ends_with_no_fault() {
    for closes in [true, false] {
      let out = leave(closes).await;
      let ended = (out.fault, out.role, out.end);
      assert_eq!(ended, (None, Role::Leaver, End::LoggedOut), "the gateway closes: {closes}");
    }
  }

  /// Runs one leaver to its logout. Then the gateway closes its link when `closes` holds, as it
  /// does, and else the measure ends first and stops the bot.
  async fn leave(closes: bool) -> Outcome {
    let login = login_service("Brom", 42);
    let (seen, played) = oneshot::channel();
    let (done, finished) = oneshot::channel();
    let gateway = Fake::start(move |mut peer| async move {
      peer.read::<Attach>().await;
      peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 1 }).await;
      sleep(Duration::from_millis(300)).await;
      send(&peer, &snapshot(10, None, vec![entity(42)]));
      let line = FromClient::Say { channel: "world".to_owned(), text: "hello".to_owned() };
      assert_eq!(peer.read::<FromClient>().await, line);
      seen.send(()).expect("the test waits for the play");
      assert_eq!(peer.read::<FromClient>().await, FromClient::Logout);
      if closes {
        peer.connection.close(1_u32.into(), b"the link to the writer of layer 1:1 ends: closed");
      }
      done.send(()).expect("the test waits for the logout");
      peer
    });
    let (leaving, leave) = watch::channel(false);
    let shared = Shared { leavers: 1, leave, ..shared_by(&login, &gateway) };
    let (stop, stopped) = watch::channel(false);
    let signals = async {
      played.await.expect("a bot that plays");
      leaving.send_replace(true);
      finished.await.expect("a logout");
      if !closes {
        stop.send_replace(true);
      }
    };
    let leaver = bot(&login);
    let (out, ()) = tokio::join!(run(&shared, &leaver, stopped), signals);
    gateway.end().await;
    login.end().await;
    out
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_ends_with_the_fault_of_the_login_service_or_of_the_gateway() {
    let login = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Refused(Refusal::Password)).await;
      peer
    });
    let idle = Fake::start(hold);
    let shared = shared_by(&login, &idle);
    let (_stop, stopped) = watch::channel(false);
    let out = run(&shared, &Bot { crosser: true, ..bot(&login) }, stopped).await;
    let refused = (out.fault.as_deref(), out.place, out.joined.is_none(), out.role);
    assert_eq!(refused, (Some("the login service refuses: Password"), None, true, Role::Crosser));
    assert_eq!(shared.ready.load(Ordering::Relaxed), 1);
    login.end().await;

    for (message, expected) in
      [(Some(ToClient::NoSession), "the gateway sends NoSession"), (None, "the link fails")]
    {
      let login = login_service("Brom", 42);
      let gateway = Fake::start(move |mut peer| async move {
        peer.read::<Attach>().await;
        match message {
          Some(message) => peer.write(&message).await,
          None => peer.connection.close(7_u32.into(), b"bye"),
        }
        peer
      });
      let shared = shared_by(&login, &gateway);
      let (_stop, stopped) = watch::channel(false);
      let out = run(&shared, &bot(&login), stopped).await;
      assert!(out.fault.is_some_and(|fault| fault.starts_with(expected)));
      assert_eq!((out.joined.is_none(), shared.ready.load(Ordering::Relaxed)), (true, 1));
      gateway.end().await;
      login.end().await;
    }
  }

  fn walk() -> Walk {
    let (character, layer, rng) =
      (CharacterId(42), Layer { zone: ZoneId(1), number: 1 }, Rng::new(1));
    let world = World::default();
    Walk {
      character,
      layer,
      crosser: false,
      rng,
      world,
      body: None,
      heading: None,
      ticks: 0,
      last: None,
      lost: None,
    }
  }

  #[test]
  fn a_walk_that_lost_its_link_gives_the_time_without_a_snapshot_when_its_player_shows_again() {
    let mut walk = walk();
    let mut out = Outcome::default();
    walk.found(&mut out);
    assert_eq!((out.recoveries.len(), out.away), (0, false));
    assert_eq!(walk.hear(&snapshot(10, None, vec![entity(42)])).ok(), Some(true));
    walk.lose(&mut out);
    let first = walk.lost.map(|(at, _)| at);
    assert!(walk.body.is_none() && walk.world.entity(CharacterId(42)).is_none() && out.away);
    walk.lose(&mut out);
    assert_eq!(walk.lost.map(|(at, _)| at), first);
    assert_eq!(walk.hear(&snapshot(3, None, vec![entity(42)])).ok(), Some(true));
    walk.found(&mut out);
    assert_eq!((out.recoveries.len(), out.relocated, out.away), (1, 0, false));
    walk.lose(&mut out);
    walk.layer = Layer { zone: ZoneId(1), number: 2 };
    walk.found(&mut out);
    assert_eq!((out.recoveries.len(), out.relocated, out.away), (2, 1, false));
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_attaches_again_until_a_link_comes_or_the_measure_ends() {
    let login = login_service("Brom", 42);
    let gateway = Fake::start(hold);
    let mut shared = shared_by(&login, &gateway);
    let (stop, mut stopped) = watch::channel(false);
    assert!(again(&shared, Token([5; 32]), &mut stopped).await.is_some());
    shared.gateway = "127.0.0.1:0".to_owned();
    let ending = async {
      sleep(Duration::from_millis(500)).await;
      stop.send_replace(true);
    };
    let (found, ()) = tokio::join!(again(&shared, Token([5; 32]), &mut stopped), ending);
    assert!(found.is_none());
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_that_lost_its_link_attaches_again_and_measures_the_time_without_a_snapshot() {
    let login = login_service("Brom", 42);
    let (done, finished) = oneshot::channel();
    let gateway = Fake::start_twice(
      move |mut peer| async move {
        peer.read::<Attach>().await;
        peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 2 }).await;
        send(&peer, &snapshot(10, None, vec![entity(42)]));
        sleep(Duration::from_millis(200)).await;
        peer.connection.close(7_u32.into(), b"bye");
        peer
      },
      move |mut peer| async move {
        assert_eq!(peer.read::<Attach>().await, Attach { session: Token([5; 32]) });
        peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 2 }).await;
        send(&peer, &snapshot(2, None, vec![entity(42)]));
        sleep(Duration::from_millis(200)).await;
        done.send(()).expect("the test waits for the second link");
        peer
      },
    );
    let mut shared = shared_by(&login, &gateway);
    shared.reconnect = true;
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      let limit = Duration::from_secs(30);
      timeout(limit, finished).await.expect("a second link in time").expect("a second link");
      stop.send_replace(true);
    };
    let walker = bot(&login);
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    assert_eq!((out.fault, out.away, out.relocated, out.recoveries.len()), (None, false, 0, 1));
    assert!(out.recoveries.iter().all(|gap| *gap >= Duration::from_millis(100)));
    gateway.end().await;
    login.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_that_lost_its_link_and_finds_no_gateway_ends_away_with_no_fault() {
    let login = login_service("Brom", 42);
    let gateway = Fake::start(move |mut peer| async move {
      peer.read::<Attach>().await;
      peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 2 }).await;
      send(&peer, &snapshot(10, None, vec![entity(42)]));
      sleep(Duration::from_millis(200)).await;
      peer.connection.close(7_u32.into(), b"bye");
      peer
    });
    let mut shared = shared_by(&login, &gateway);
    shared.reconnect = true;
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      sleep(Duration::from_millis(900)).await;
      stop.send_replace(true);
    };
    let walker = bot(&login);
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    assert_eq!((out.fault, out.away, out.recoveries.len()), (None, true, 0));
    assert!(out.joined.is_some());
    gateway.end().await;
    login.end().await;
  }

  #[test]
  fn a_crosser_walks_east_and_a_walk_that_changes_layer_records_a_crossing() {
    let mut crosser = Walk { crosser: true, ..walk() };
    let mut out = Outcome::default();
    assert_eq!(crosser.hear(&snapshot(10, None, vec![entity(42)])).ok(), Some(true));
    for _ in 0..=TURN {
      crosser.tick();
      assert_eq!(crosser.heading, Some(Angle(0)));
    }
    let (from, next) = (crosser.layer, Layer { zone: ZoneId(2), number: 1 });
    crosser.moved(next, &mut out);
    assert_eq!(out.crossings, [(from, next)]);
    assert!(crosser.body.is_none() && crosser.world.entity(CharacterId(42)).is_none());
    crosser.moved(next, &mut out);
    assert_eq!(out.crossings.len(), 1);
    let mut fresh = Walk { layer: Layer { zone: ZoneId(1), number: 0 }, ..walk() };
    fresh.moved(Layer { zone: ZoneId(1), number: 2 }, &mut out);
    assert_eq!((out.crossings.len(), fresh.layer.number), (1, 2));
  }

  #[test]
  fn an_outcome_notes_each_character_of_another_realm_that_a_snapshot_gives() {
    let realms = HashMap::from([(42, 1), (43, 2), (44, 2)]);
    let mut out = Outcome { realm: 1, ..Outcome::default() };
    let seen = snapshot(10, None, vec![entity(42), entity(43), entity(45)]);
    out.note_strangers(&realms, &seen);
    out.note_strangers(&realms, &snapshot(11, None, vec![entity(43), entity(44)]));
    assert_eq!(out.strangers.len(), 2);
    assert!(out.strangers.contains(&43) && out.strangers.contains(&44));
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_that_does_not_cross_waits_for_the_crossers_and_ends_when_it_is_stopped() {
    let login = login_service("Brom", 42);
    let idle = Fake::start(hold);
    let mut shared = shared_by(&login, &idle);
    shared.crossers = 1;
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      sleep(Duration::from_millis(300)).await;
      stop.send_replace(true);
    };
    let walker = bot(&login);
    let ended = timeout(Duration::from_secs(10), async {
      tokio::join!(run(&shared, &walker, stopped), stopping)
    });
    let (out, ()) = ended.await.expect("a bot that waits for the crossers ends when it is stopped");
    assert_eq!((out.fault, out.queued, out.waited, out.joined), (None, false, None, None));
    assert_eq!(shared.ready.load(Ordering::Relaxed), 1);
  }

  #[test]
  fn a_walk_sends_nothing_before_it_sees_its_player() {
    let mut walk = walk();
    assert_eq!((walk.tick(), walk.ticks), (None, 0));
  }

  #[test]
  fn a_walk_keeps_its_heading_for_a_turn_and_picks_another_for_the_next() {
    let mut walk = walk();
    assert_eq!(walk.hear(&snapshot(10, None, vec![entity(42)])).ok(), Some(true));
    let headings: Vec<_> = (0..=TURN)
      .map(|_| {
        let intents = walk.tick().expect("the inputs of a tick");
        assert_eq!((intents.zone, intents.layer), (ZoneId(1), 1));
        walk.heading
      })
      .collect();
    let (next, kept) = headings.split_last().expect("one heading for each tick");
    assert_eq!(walk.ticks, TURN + 1);
    assert!(kept.iter().all(|heading| Some(heading) == kept.first()));
    assert_ne!(Some(next), kept.first());
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_ends_with_the_fault_of_a_gateway_that_it_cannot_reach() {
    let login = login_service("Brom", 42);
    let idle = Fake::start(hold);
    let mut shared = shared_by(&login, &idle);
    shared.gateway = "127.0.0.1:0".to_owned();
    let (_stop, stopped) = watch::channel(false);
    let out = run(&shared, &bot(&login), stopped).await;
    assert!(out.fault.is_some_and(|fault| fault.starts_with("the client cannot connect")));
    assert_eq!((out.waited.is_some(), out.joined.is_none()), (true, true));
    login.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_bot_that_waits_in_the_queue_when_the_measure_ends_says_so() {
    let (queued, wait) = oneshot::channel();
    let login = Fake::start(move |mut peer| async move {
      peer.read::<ToLogin>().await;
      let place = FromLogin::Queued { position: 7, length: 9, wait: 60, resume: Token([1; 32]) };
      peer.write(&place).await;
      // The bot reads the place before the test stops it.
      sleep(Duration::from_millis(300)).await;
      queued.send(()).expect("the test waits for the place");
      peer
    });
    let idle = Fake::start(hold);
    let shared = shared_by(&login, &idle);
    let (stop, stopped) = watch::channel(false);
    let stopping = async {
      wait.await.expect("a place");
      stop.send_replace(true);
    };
    let walker = bot(&login);
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    assert_eq!(
      (out.fault.as_deref(), out.queued, out.waited, shared.ready.load(Ordering::Relaxed)),
      (None, true, None, 1)
    );
    assert_eq!(out.places.len(), 1);
    assert_eq!(out.places.first().map(|(_, place)| place.position), Some(7));
    assert!(out.ended.is_some());
    login.end().await;
  }
}
