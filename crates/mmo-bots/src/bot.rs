//! One bot: it logs in, waits in the queue, attaches to a gateway, and walks as a player does.

use std::iter;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mmo_client::{Down, Fault, Gap, Place, Play, Predictor, Trust, Up, World, enter};
use mmo_collide::{Terrain, ground};
use mmo_protocol::{Angle, CharacterId, Intents, Snapshot, ToClient, ZoneId, encode};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval};

use crate::rng::Rng;
use crate::seed::Bot;

/// The time of one tick of the game.
const TICK: Duration = Duration::from_millis(50);

/// A bot picks a new walk each 20 ticks (1 s): one of 8 headings, or a stop.
const TURN: u64 = 20;

/// A bot says a line in the chat each 400 ticks (20 s), and at its first tick.
const SAY: u64 = 400;

/// What the bots of one run share.
pub(crate) struct Shared {
  pub(crate) trust: Trust,
  pub(crate) login: String,
  pub(crate) gateway: String,
  pub(crate) zone: ZoneId,
  pub(crate) seed: u64,
  /// The count of the bots that play, or that failed before they played.
  pub(crate) ready: AtomicUsize,
}

/// What one bot measured.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
  /// The place that the login service first gave in the queue.
  pub(crate) place: Option<Place>,
  /// The time from the start of the login to the session.
  pub(crate) waited: Option<Duration>,
  /// The time from the start of the login to the first snapshot that shows the player.
  pub(crate) entered: Option<Duration>,
  pub(crate) joined: Option<Instant>,
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

#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Fault(#[from] Fault),
  #[error("the gateway sends {0:?}")]
  Message(ToClient),
  #[error("the measure ended while the bot waited in the queue")]
  Queued,
}

/// The body of the player, and what it knows of the world.
struct Walk {
  character: CharacterId,
  zone: ZoneId,
  rng: Rng,
  world: World,
  body: Option<Predictor>,
  heading: Option<Angle>,
  ticks: u64,
}

#[expect(
  clippy::expect_used,
  reason = "the ground of a zone is valid, as the test of mmo-collide proves"
)]
fn terrain() -> Terrain {
  Terrain::new(&ground()).expect("the ground is valid")
}

impl Walk {
  /// Takes a snapshot. It gives true when the snapshot shows the player. A snapshot that starts
  /// from one that the world does not hold empties the world, so the zone sends a full snapshot.
  fn hear(&mut self, snapshot: &Snapshot) -> Result<bool, Gap> {
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

  /// Runs one tick of the walk, and gives the datagram of the inputs. A bot sends nothing before
  /// the first snapshot that shows its player.
  fn tick(&mut self) -> Option<Intents> {
    let body = self.body.as_mut()?;
    if self.ticks.is_multiple_of(TURN) {
      self.heading = u16::try_from(self.rng.below(9) * 8192).ok().map(Angle);
    }
    self.ticks += 1;
    body.step(self.heading, self.heading.unwrap_or(Angle(0)));
    body.recent().map(|inputs| Intents { zone: self.zone, ack: self.world.ack(), inputs })
  }
}

fn say() -> Up {
  Up::Say { channel: "world".to_owned(), text: "hello".to_owned() }
}

async fn play(
  shared: &Shared,
  bot: &Bot,
  mut stop: watch::Receiver<bool>,
  out: &mut Outcome,
) -> Result<(), Error> {
  let started = Instant::now();
  let queued = &mut |place| {
    out.place.get_or_insert(place);
  };
  let token = tokio::select! {
    session = enter(&shared.trust, &shared.login, &bot.account, queued) => session?.token,
    _ = stop.changed() => return Err(Error::Queued),
  };
  out.waited = Some(started.elapsed());
  let mut link = Play::attach(&shared.trust, &shared.gateway, token).await?;
  let rng = Rng::new(shared.seed.wrapping_mul(0x9e37_79b9) ^ u64::from(bot.index));
  let (zone, character) = (shared.zone, bot.character);
  let mut walk =
    Walk { character, zone, rng, world: World::default(), body: None, heading: None, ticks: 0 };
  let mut clock = interval(TICK);
  clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
  loop {
    tokio::select! {
      _ = clock.tick() => {
        if let Some(intents) = walk.tick() {
          out.up += encode(&intents).len();
          let chat = (walk.ticks % SAY == 1).then(say);
          for up in iter::once(Up::Intents(intents)).chain(chat) {
            // A dead link ends `link.next()` at once, so a send that fails ends nothing here.
            drop(link.send(up).await);
          }
        }
      }
      down = link.next() => match down? {
        Down::Snapshot(snapshot) => {
          let size = encode(&snapshot).len();
          (out.snapshots, out.down, out.largest) = (out.snapshots + 1, out.down + size, out.largest.max(size));
          match walk.hear(&snapshot) {
            Ok(true) if out.joined.is_none() => {
              (out.entered, out.joined) = (Some(started.elapsed()), Some(Instant::now()));
              shared.ready.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(_) => out.gaps += 1,
          }
        }
        Down::Message(ToClient::Line { .. }) => {}
        Down::Message(other) => return Err(Error::Message(other)),
      },
      _ = stop.changed() => return Ok(()),
    }
  }
}

/// Runs one bot until `stop` changes, and gives what it measured. A fault ends the bot, and the
/// outcome holds it. A bot leaves as a player who closes the game does: it closes its link, and the
/// zone writer keeps its player for 30 s.
pub(crate) async fn run(shared: &Shared, bot: &Bot, stop: watch::Receiver<bool>) -> Outcome {
  let mut out = Outcome::default();
  let ended = play(shared, bot, stop, &mut out).await;
  out.fault = ended.err().map(|error| error.to_string());
  out.played = out.joined.map(|at| at.elapsed()).unwrap_or_default();
  if out.joined.is_none() {
    shared.ready.fetch_add(1, Ordering::Relaxed);
  }
  out
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Duration;

  use mmo_client::World;
  use mmo_client::testing::{Fake, hold};
  use mmo_client::{Account, Place, Trust};
  use mmo_protocol::{
    Attach, CharacterId, FromClient, FromLogin, Password, Refusal, Snapshot, ToClient, ToLogin,
    Token, ZoneId, encode,
  };
  use tokio::sync::{oneshot, watch};
  use tokio::time::sleep;

  use super::{Shared, TURN, Walk, run};
  use crate::fixture::{authority, entity, login_service, send, snapshot};
  use crate::rng::Rng;
  use crate::seed::Bot;

  fn bot() -> Bot {
    let account = Account {
      name: "ada".to_owned(),
      password: Password("a long password".to_owned()),
      character: "Brom".to_owned(),
      create: false,
    };
    Bot { index: 0, account, character: CharacterId(42) }
  }

  fn shared_by(login: &Fake, gateway: &Fake) -> Shared {
    let (_folder, path) = authority(&[login, gateway]);
    Shared {
      trust: Trust::from_pem(&path).expect("a trust"),
      login: login.address.clone(),
      gateway: gateway.address.clone(),
      zone: ZoneId(1),
      seed: 1,
      ready: AtomicUsize::new(0),
    }
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
    let walker = bot();
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    let sizes: Vec<usize> = sent().iter().map(|one| encode(one).len()).collect();
    assert_eq!(out.fault, None);
    assert_eq!(out.place, Some(Place { position: 3, length: 9, wait: 1 }));
    assert_eq!((out.snapshots, out.gaps), (5, 2));
    assert_eq!(
      (out.down, out.largest),
      (sizes.iter().sum(), sizes.iter().copied().max().unwrap_or(0))
    );
    assert!(out.up > 0 && out.waited.is_some() && out.entered.is_some() && out.joined.is_some());
    assert_eq!(shared.ready.load(Ordering::Relaxed), 1);
    gateway.end().await;
    login.end().await;
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
    let out = run(&shared, &bot(), stopped).await;
    let refused = (out.fault.as_deref(), out.place, out.joined.is_none());
    assert_eq!(refused, (Some("the login service refuses: Password"), None, true));
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
      let out = run(&shared, &bot(), stopped).await;
      assert!(out.fault.is_some_and(|fault| fault.starts_with(expected)));
      assert_eq!((out.joined.is_none(), shared.ready.load(Ordering::Relaxed)), (true, 1));
      gateway.end().await;
      login.end().await;
    }
  }

  fn walk() -> Walk {
    let (character, zone, rng) = (CharacterId(42), ZoneId(1), Rng::new(1));
    Walk { character, zone, rng, world: World::default(), body: None, heading: None, ticks: 0 }
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
        assert_eq!(intents.zone, ZoneId(1));
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
    let out = run(&shared, &bot(), stopped).await;
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
    let walker = bot();
    let (out, ()) = tokio::join!(run(&shared, &walker, stopped), stopping);
    let fault = "the measure ended while the bot waited in the queue";
    assert_eq!(
      (out.fault.as_deref(), out.waited, shared.ready.load(Ordering::Relaxed)),
      (Some(fault), None, 1)
    );
    login.end().await;
  }
}
