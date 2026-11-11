//! The check of a stack that runs on one machine: it makes an account and a character through the
//! login service, then links to a gateway and waits for the first snapshot that shows the player.
//!
//! The binary `mmo-check` runs [`check`], and the variables that it reads are these:
//!
//! - `MMO_CHECK_AUTHORITY`: the PEM file of the certificates that sign the login service and the
//!   gateway.
//! - `MMO_CHECK_LOGIN` and `MMO_CHECK_GATEWAY`: their addresses, as `host:port`. Each host is the
//!   name that its certificate must hold.
//! - `MMO_CHECK_NAME`, `MMO_CHECK_PASSWORD` and `MMO_CHECK_CHARACTER`: the new account and its
//!   character. The login service refuses a name that an account holds already.

use std::time::Duration;

use mmo_protocol::ToClient;
use tokio::time::timeout;

use crate::settings::Settings;
use crate::{Down, Failure, Play, enter};

/// The time that the check waits for the world: the queue admits an account within 1 s, and the
/// first snapshot comes within 50 ms of the join of the zone writer.
const LIMIT: Duration = Duration::from_secs(10);

/// Runs the check as the environment that `read` gives configures it, and prints what it proved.
pub async fn check(read: &dyn Fn(&str) -> Option<String>) -> Result<(), Failure> {
  check_within(read, LIMIT).await
}

async fn check_within(
  read: &dyn Fn(&str) -> Option<String>,
  limit: Duration,
) -> Result<(), Failure> {
  let Settings { trust, login, gateway, account } = Settings::read("MMO_CHECK", read, true)?;
  let seen = async {
    let session = enter(&trust, &login, &account, &mut |_| ()).await?;
    println!(
      "the login service made {} with {} and gave a session",
      account.name, account.character
    );
    let mut play = Play::attach(&trust, &gateway, session.token).await?;
    loop {
      match play.next().await? {
        Down::Snapshot(snapshot) if !snapshot.entered.is_empty() => {
          let (tick, seen) = (snapshot.tick.0, snapshot.entered.len());
          println!("the zone shows {seen} entities in the snapshot of tick {tick}");
          return Ok(());
        }
        Down::Snapshot(_) | Down::Message(ToClient::Moved { .. }) => {}
        Down::Message(message) => return Err(Failure::Message(message)),
      }
    }
  };
  timeout(limit, seen).await.map_err(|_elapsed| Failure::Slow(limit))?
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;
  use std::time::Duration;

  use mmo_protocol::{
    Angle, Attach, Character, CharacterId, Entity, FromLogin, Password, Position, Refusal,
    Snapshot, Tick, ToClient, ToLogin, Token, Velocity, ZoneId, datagram,
  };

  use super::{check, check_within};
  use crate::testing::{Fake, Peer, hold};

  fn snapshot(tick: u32, entered: Vec<Entity>) -> Snapshot {
    Snapshot { tick: Tick(tick), base: None, entered, changed: vec![], left: vec![] }
  }

  fn entity() -> Entity {
    Entity {
      id: CharacterId(42),
      position: Position { x: 10_000, y: 0, z: 10_000 },
      velocity: Velocity { x: 0, y: 0, z: 0 },
      view: Angle(0),
      animation: 0,
      hit_points: 100,
    }
  }

  /// The login service of a test: it queues the account once, then makes it with the character
  /// "Brom" and gives a session.
  async fn login(mut peer: Peer) -> Peer {
    let account = ToLogin::NewAccount {
      name: "checker".to_owned(),
      password: Password("a long password".to_owned()),
    };
    assert_eq!(peer.read::<ToLogin>().await, account);
    let place = FromLogin::Queued { position: 0, length: 1, wait: 1, resume: Token([1; 32]) };
    peer.write(&place).await;
    peer.write(&FromLogin::Characters(vec![])).await;
    assert_eq!(peer.read::<ToLogin>().await, ToLogin::NewCharacter { name: "Brom".to_owned() });
    let brom = Character { id: CharacterId(42), name: "Brom".to_owned() };
    peer.write(&FromLogin::Characters(vec![brom])).await;
    assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(42) });
    peer.write(&FromLogin::Session { token: Token([5; 32]) }).await;
    peer
  }

  /// A login service that refuses the password.
  async fn refusing(mut peer: Peer) -> Peer {
    peer.read::<ToLogin>().await;
    peer.write(&FromLogin::Refused(Refusal::Password)).await;
    peer
  }

  /// What the gateway of a test sends after the attach.
  #[derive(Clone, Copy)]
  enum Gateway {
    /// Two snapshots: one that shows nobody, and one that shows the player.
    Shows,
    /// A message that is no snapshot.
    Says,
    /// A datagram that is no message of the protocol.
    Garbles,
  }

  async fn gateway(mut peer: Peer, what: Gateway) -> Peer {
    assert_eq!(peer.read::<Attach>().await, Attach { session: Token([5; 32]) });
    peer.write(&ToClient::Moved { zone: ZoneId(1) }).await;
    match what {
      Gateway::Shows => {
        for shown in [vec![], vec![entity()]] {
          let bytes = datagram(&snapshot(7, shown)).expect("a datagram");
          peer.connection.send_datagram(bytes.into()).expect("sent");
        }
      }
      Gateway::Says => peer.write(&ToClient::Full { zone: ZoneId(1) }).await,
      Gateway::Garbles => peer.connection.send_datagram(vec![0xff; 4].into()).expect("sent"),
    }
    peer
  }

  /// The variables of a check against `login` and `gateway`, with the file of the authority that
  /// holds the certificates of both.
  fn variables(login: &Fake, gateway: &Fake, name: &str) -> (PathBuf, Vec<(&'static str, String)>) {
    let file = std::env::temp_dir().join(format!("mmo-check-{}-{name}.pem", std::process::id()));
    std::fs::write(&file, format!("{}{}", login.pem, gateway.pem)).expect("a file");
    let vars = vec![
      ("MMO_CHECK_AUTHORITY", file.display().to_string()),
      ("MMO_CHECK_LOGIN", login.address.clone()),
      ("MMO_CHECK_GATEWAY", gateway.address.clone()),
      ("MMO_CHECK_NAME", "checker".to_owned()),
      ("MMO_CHECK_PASSWORD", "a long password".to_owned()),
      ("MMO_CHECK_CHARACTER", "Brom".to_owned()),
    ];
    (file, vars)
  }

  fn var(vars: &[(&'static str, String)]) -> impl Fn(&str) -> Option<String> {
    let vars = vars.to_vec();
    move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
  }

  /// The words of the failure of a check that must fail.
  async fn failed(vars: &[(&'static str, String)]) -> String {
    check_within(&var(vars), Duration::from_millis(500)).await.expect_err("a failure").to_string()
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_check_makes_an_account_and_waits_for_a_snapshot_that_shows_the_player() {
    let (login, gateway) = (Fake::start(login), Fake::start(|peer| gateway(peer, Gateway::Shows)));
    let (file, vars) = variables(&login, &gateway, "shown");
    check(&var(&vars)).await.expect("the check passes");
    std::fs::remove_file(file).expect("the file goes");
    login.end().await;
    gateway.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_check_names_each_variable_that_it_lacks_and_the_authority_that_it_cannot_read() {
    let (login, gateway) = (Fake::start(hold), Fake::start(hold));
    let (file, vars) = variables(&login, &gateway, "unset");
    let mut found = Vec::new();
    for (missing, _) in &vars {
      let rest: Vec<_> = vars.iter().filter(|(key, _)| key != missing).cloned().collect();
      found.push(failed(&rest).await);
    }
    let nowhere = [("MMO_CHECK_AUTHORITY", "/nowhere.pem".to_owned())];
    found.push(failed(&[&nowhere[..], &vars[1..]].concat()).await);
    std::fs::remove_file(file).expect("the file goes");
    let unset = |name: &str| format!("the variable {name} is not set");
    let expected: Vec<_> = vars.iter().map(|(name, _)| unset(name)).collect();
    assert_eq!(found[..6], expected[..]);
    assert!(found[6].starts_with("the certificate cannot be read: "));
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_check_names_what_stops_it_in_the_login_and_in_the_gateway() {
    let mut found = Vec::new();
    for (name, script, what) in [
      ("refused", 0, Gateway::Shows),
      ("attach", 1, Gateway::Shows),
      ("message", 1, Gateway::Says),
      ("garbled", 1, Gateway::Garbles),
    ] {
      let login = if script == 0 { Fake::start(refusing) } else { Fake::start(login) };
      let gateway = Fake::start(move |peer| gateway(peer, what));
      let (file, mut vars) = variables(&login, &gateway, name);
      if name == "attach" {
        vars[2].1 = "127.0.0.1:0".to_owned();
      }
      found.push(failed(&vars).await);
      std::fs::remove_file(file).expect("the file goes");
      login.end().await;
      if name != "attach" && name != "refused" {
        gateway.end().await;
      }
    }
    assert_eq!(found[0], "the login service refuses: Password");
    assert!(found[1].starts_with("the client cannot connect: "));
    assert_eq!(found[2], "the gateway sends Full { zone: ZoneId(1) } in place of a snapshot");
    assert_eq!(found[3], "the bytes are not a message of this version of the protocol");
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_check_ends_when_the_gateway_shows_nothing_in_time() {
    let (login, gateway) = (Fake::start(login), Fake::start(|peer| async move { peer }));
    let (file, vars) = variables(&login, &gateway, "slow");
    assert_eq!(failed(&vars).await, "the check does not end within 500ms");
    std::fs::remove_file(file).expect("the file goes");
    login.end().await;
    gateway.end().await;
  }
}
