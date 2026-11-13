//! The start of the game client: its command line, its login and its link to a gateway.

use tokio::runtime::Runtime;
use tokio::sync::mpsc::unbounded_channel;

use crate::settings::Settings;
use crate::{Failure, Game, Place, Play, Wire, enter};

/// What stands behind a game, and must live as long as it does.
pub(crate) enum Keep {
  /// The reactor that runs the link to the gateway.
  Server { _reactor: Runtime },
  /// The far end of a game with no server.
  Demo { _wire: Wire },
}

/// A game that is ready to show.
pub(crate) struct Launch {
  pub(crate) game: Game,
  /// The count of frames after which the window closes, or `None` to run until the player quits.
  pub(crate) frames: Option<u32>,
  pub(crate) keep: Keep,
}

struct Arguments {
  demo: bool,
  frames: Option<u32>,
}

fn parse(args: &[String]) -> Result<Arguments, Failure> {
  let mut parsed = Arguments { demo: false, frames: None };
  let mut rest = args.iter();
  while let Some(arg) = rest.next() {
    match arg.as_str() {
      "--demo" => parsed.demo = true,
      "--frames" => {
        let count = rest.next().and_then(|count| count.parse().ok());
        parsed.frames = Some(count.ok_or_else(|| {
          Failure::Usage("--frames needs a count of frames, as --frames 60".to_owned())
        })?);
      }
      other => {
        return Err(Failure::Usage(format!(
          "{other} is no option of the game: use --demo, or --frames <count>"
        )));
      }
    }
  }
  Ok(parsed)
}

/// Reads the command line and the environment, and makes the game: the demo with `--demo`, or the
/// game of the character that the variables `MMO_PLAY_*` name, linked to its gateway. The thread
/// of the window runs the game, so the link runs on a reactor of its own, which the launch keeps.
#[expect(clippy::expect_used, reason = "a process that cannot start a reactor cannot play")]
#[expect(
  clippy::disallowed_methods,
  reason = "the thread of the window waits once for the login, before the window opens"
)]
pub(crate) fn prepare(
  args: &[String],
  read: &dyn Fn(&str) -> Option<String>,
) -> Result<Launch, Failure> {
  let Arguments { demo, frames } = parse(args)?;
  if demo {
    let (game, wire) = Game::demo();
    return Ok(Launch { game, frames, keep: Keep::Demo { _wire: wire } });
  }
  let runtime = Runtime::new().expect("the reactor starts");
  let game = runtime.block_on(connect(read))?;
  Ok(Launch { game, frames, keep: Keep::Server { _reactor: runtime } })
}

/// Logs in as the variables `MMO_PLAY_*` say, with the same meaning as the variables `MMO_CHECK_*`
/// of [`check`](crate::check), and `MMO_PLAY_CREATE` set to make the account first. It prints the
/// place in the queue of the world, since no window shows it yet. Then it links to the gateway and
/// runs that link on the reactor that calls it.
async fn connect(read: &dyn Fn(&str) -> Option<String>) -> Result<Game, Failure> {
  let create = read("MMO_PLAY_CREATE").is_some();
  let Settings { trust, login, gateway, account } = Settings::read("MMO_PLAY", read, create)?;
  let queued = &mut |place: Place| {
    eprintln!(
      "the queue of the world: place {} of {}, about {} s",
      place.position + 1,
      place.length,
      place.wait
    );
  };
  let session = enter(&trust, &login, &account, queued).await?;
  let link = Play::attach(&trust, &gateway, session.token).await?;
  let (up, from) = unbounded_channel();
  let (to, down) = unbounded_channel();
  tokio::spawn(async move {
    let fault = link.run(from, to).await;
    eprintln!("the link to the gateway ends: {fault}");
  });
  Ok(Game::new(session.character, up, down))
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;
  use std::time::Duration;

  use mmo_protocol::{
    Angle, Attach, Character, CharacterId, Entity, FromLogin, Intents, Password, Position, Refusal,
    Snapshot, Tick, ToClient, ToLogin, Token, Velocity, ZoneId, datagram, decode,
  };
  use tokio::runtime::Runtime;
  use tokio::time::{sleep, timeout};

  use super::{parse, prepare};
  use crate::testing::{Fake, Peer};

  fn arguments(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
  }

  #[test]
  fn the_command_line_takes_the_demo_and_a_count_of_frames() {
    let parsed = parse(&arguments(&["--demo", "--frames", "60"])).expect("a command line");
    assert!(parsed.demo);
    assert_eq!(parsed.frames, Some(60));
    let parsed = parse(&[]).expect("an empty command line");
    assert!(!parsed.demo);
    assert_eq!(parsed.frames, None);
  }

  #[test]
  fn the_command_line_names_what_it_does_not_take() {
    let said = |words: &[&str]| parse(&arguments(words)).err().map(|failure| failure.to_string());
    let frames = "--frames needs a count of frames, as --frames 60";
    assert_eq!(said(&["--frames"]).as_deref(), Some(frames));
    assert_eq!(said(&["--frames", "many"]).as_deref(), Some(frames));
    let other = "--fast is no option of the game: use --demo, or --frames <count>";
    assert_eq!(said(&["--fast"]).as_deref(), Some(other));
  }

  #[test]
  fn the_demo_needs_no_server_and_keeps_its_wire() {
    let launch = prepare(&arguments(&["--demo", "--frames", "3"]), &|_| None).expect("a demo");
    assert_eq!(launch.frames, Some(3));
    assert!(!launch.game.over());
  }

  #[test]
  fn a_launch_without_the_variables_names_the_first_that_it_lacks() {
    let failure = prepare(&[], &|_| None).err().map(|failure| failure.to_string());
    assert_eq!(failure.as_deref(), Some("the variable MMO_PLAY_AUTHORITY is not set"));
    let option = arguments(&["--fast"]);
    assert!(prepare(&option, &|_| None).is_err());
  }

  fn snapshot(tick: u32) -> Snapshot {
    let player = Entity {
      id: CharacterId(42),
      position: Position { x: 10_000, y: 0, z: 10_000 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    };
    Snapshot { tick: Tick(tick), base: None, entered: vec![player], changed: vec![], left: vec![] }
  }

  /// The login service of a test: it queues the account once, then gives the session of Brom.
  async fn login(mut peer: Peer) -> Peer {
    let name = "ada".to_owned();
    let asked = ToLogin::Login { name, password: Password("a long password".to_owned()) };
    assert_eq!(peer.read::<ToLogin>().await, asked);
    let place = FromLogin::Queued { position: 0, length: 2, wait: 3, resume: Token([1; 32]) };
    peer.write(&place).await;
    let brom = Character { id: CharacterId(42), name: "Brom".to_owned() };
    peer.write(&FromLogin::Characters(vec![brom])).await;
    assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(42) });
    peer.write(&FromLogin::Session { token: Token([5; 32]) }).await;
    peer
  }

  /// The gateway of a test: it gives the zone, sends a snapshot each 20 ms from a task of its own
  /// until the client sends its intents, checks them, and closes the link. A snapshot can pass the
  /// zone, so the task sends more, and no bound in time decides which path of the test runs.
  async fn gateway(mut peer: Peer) -> Peer {
    assert_eq!(peer.read::<Attach>().await, Attach { session: Token([5; 32]) });
    peer.write(&ToClient::Moved { zone: ZoneId(1), layer: 1 }).await;
    let sender = peer.connection.clone();
    let snapshots = tokio::spawn(async move {
      let mut tick = 12;
      loop {
        let bytes = datagram(&snapshot(tick)).expect("a datagram");
        sender.send_datagram(bytes.into()).expect("sent");
        tick += 1;
        sleep(Duration::from_millis(20)).await;
      }
    });
    let sent = peer.connection.read_datagram().await.expect("a datagram");
    snapshots.abort();
    assert!(snapshots.await.is_err(), "the snapshots stop before the link closes");
    let intents: Intents = decode(&sent).expect("intents");
    assert_eq!(intents.zone, ZoneId(1));
    assert!(intents.ack.is_some());
    peer.connection.close(7_u32.into(), b"bye");
    peer
  }

  /// The variables of a launch against the servers at `login` and `gateway`, and the file of the
  /// authority that holds `pem`.
  fn variables(pem: &str, login: &str, gateway: &str) -> (PathBuf, Vec<(&'static str, String)>) {
    let name = format!("mmo-play-{}-{}.pem", std::process::id(), gateway.replace(':', "-"));
    let file = std::env::temp_dir().join(name);
    std::fs::write(&file, pem).expect("a file");
    let vars = vec![
      ("MMO_PLAY_AUTHORITY", file.display().to_string()),
      ("MMO_PLAY_LOGIN", login.to_owned()),
      ("MMO_PLAY_GATEWAY", gateway.to_owned()),
      ("MMO_PLAY_NAME", "ada".to_owned()),
      ("MMO_PLAY_PASSWORD", "a long password".to_owned()),
      ("MMO_PLAY_CHARACTER", "brom".to_owned()),
    ];
    (file, vars)
  }

  fn launched(vars: &[(&'static str, String)]) -> Result<super::Launch, String> {
    let read = |name: &str| vars.iter().find(|(key, _)| *key == name).map(|(_, v)| v.clone());
    prepare(&[], &read).map_err(|failure| failure.to_string())
  }

  #[expect(clippy::disallowed_methods, reason = "the test waits for a game that runs on a reactor")]
  #[test]
  fn the_game_logs_in_queues_attaches_and_sends_its_intents_until_the_link_ends() {
    let host = Runtime::new().expect("a reactor");
    let guard = host.enter();
    let (login, gateway) = (Fake::start(login), Fake::start(gateway));
    let pem = format!("{}{}", login.pem, gateway.pem);
    let (file, vars) = variables(&pem, &login.address, &gateway.address);
    let mut launch = launched(&vars).expect("the launch reaches the gateway");
    let game = &mut launch.game;
    let ended = host.block_on(timeout(Duration::from_secs(20), async {
      while !game.over() {
        game.update(0.06);
        sleep(Duration::from_millis(10)).await;
      }
    }));
    assert!(ended.is_ok(), "the game ends with the link");
    std::fs::remove_file(file).expect("the file goes");
    host.block_on(login.end());
    host.block_on(gateway.end());
    drop(guard);
  }

  #[expect(clippy::disallowed_methods, reason = "the test waits for fake servers on a reactor")]
  #[test]
  fn a_launch_names_what_stops_it_in_the_login_and_at_the_gateway() {
    let host = Runtime::new().expect("a reactor");
    let guard = host.enter();
    let refusing = Fake::start(|mut peer: Peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Refused(Refusal::Password)).await;
      peer
    });
    let (file, vars) = variables(&refusing.pem, &refusing.address, "127.0.0.1:0");
    let refused = launched(&vars).err();
    assert_eq!(refused.as_deref(), Some("the login service refuses: Password"));
    std::fs::remove_file(file).expect("the file goes");
    host.block_on(refusing.end());
    let serving = Fake::start(login);
    let (file, vars) = variables(&serving.pem, &serving.address, "127.0.0.1:0");
    let unreachable = launched(&vars).err().unwrap_or_default();
    assert!(unreachable.starts_with("the client cannot connect: "), "{unreachable}");
    std::fs::remove_file(file).expect("the file goes");
    host.block_on(serving.end());
    drop(guard);
  }
}
