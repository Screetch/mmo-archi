//! The servers that the tests of the bots script.

use std::fs;
use std::path::PathBuf;

use mmo_client::testing::Fake;
use mmo_protocol::{
  Angle, Character, CharacterId, Entity, FromLogin, Position, Snapshot, Tick, ToLogin, Token,
  Velocity, datagram,
};
use tempfile::TempDir;

use mmo_client::testing::Peer;

/// A file of PEM that holds the certificate of each fake, and the folder that holds the file.
pub(crate) fn authority(fakes: &[&Fake]) -> (TempDir, PathBuf) {
  let folder = TempDir::new().expect("a folder");
  let path = folder.path().join("authority.pem");
  fs::write(&path, fakes.iter().map(|fake| fake.pem.as_str()).collect::<String>()).expect("a file");
  (folder, path)
}

/// A login service that queues the account at place 3 of 9, and then gives the character `name`
/// with the id `id`, and a session.
pub(crate) fn login_service(name: &str, id: i64) -> Fake {
  let character = Character { id: CharacterId(id), name: name.to_owned() };
  Fake::start(move |mut peer| async move {
    peer.read::<ToLogin>().await;
    let place = FromLogin::Queued { position: 3, length: 9, wait: 1, resume: Token([1; 32]) };
    peer.write(&place).await;
    peer.write(&FromLogin::Characters(vec![character])).await;
    assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(id) });
    peer.write(&FromLogin::Session { token: Token([5; 32]) }).await;
    peer
  })
}

pub(crate) fn entity(id: i64) -> Entity {
  Entity {
    id: CharacterId(id),
    position: Position { x: 10_000, y: 0, z: 10_000 },
    velocity: Velocity { x: 0, y: 0, z: 0 },
    view: Angle(0),
    animation: 0,
    hit_points: 100,
  }
}

pub(crate) fn snapshot(tick: u32, base: Option<u32>, entered: Vec<Entity>) -> Snapshot {
  Snapshot { tick: Tick(tick), base: base.map(Tick), entered, changed: vec![], left: vec![] }
}

/// Sends `snapshot` to the client of `peer` in a datagram.
pub(crate) fn send(peer: &Peer, snapshot: &Snapshot) {
  let bytes = datagram(snapshot).expect("a datagram");
  peer.connection.send_datagram(bytes.into()).expect("sent");
}
