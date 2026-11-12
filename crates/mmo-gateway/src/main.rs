//! The gateway of mmo-archi. It ends the QUIC connection of each client, checks its session in
//! Redis, finds the zone of its player in the directory of zones, and relays each message between
//! the client and the writer of that zone.
//!
//! It keeps no state of a player. It holds only its open connections, and a link to each zone
//! writer that the directory names, which it can drop at any time and open again from Redis. So
//! any gateway serves any client, and a gateway that stops loses nothing: its clients attach to
//! another gateway with the same session. It reads these variables of its environment:
//!
//! - `GATEWAY_ADDRESS`: the address and the UDP port of the clients, as `0.0.0.0:4435`.
//! - `GATEWAY_CERTIFICATE` and `GATEWAY_KEY`: the PEM files of the certificate chain that the
//!   clients trust, and of its private key.
//! - `GATEWAY_LINK_CERTIFICATE` and `GATEWAY_LINK_KEY`: the PEM files of the certificate with
//!   which the gateway shows each zone writer that the operator runs it, and of its private key.
//! - `GATEWAY_AUTHORITY`: the PEM file of the certificates that sign the certificate of each zone
//!   writer. The gateway relays no intent to a peer whose certificate they do not sign.
//! - `REDIS_URL`: the Redis of the world, as `redis://redis:6379`.
//!
//! It runs until a fault stops it. Then it writes the fault on its standard error and exits with
//! the code 1, and the process manager starts it again.

mod client;
mod config;
mod gateway;
mod link;
#[cfg(test)]
mod world;
mod writer;

use std::process::ExitCode;
use std::sync::Arc;
use std::{env, io};

use quinn::Endpoint;

use crate::config::Config;
use crate::gateway::Gateway;

/// Why the gateway stops.
#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Config(#[from] config::Error),
  #[error("Redis refuses the gateway: {0}")]
  Redis(#[from] redis::RedisError),
  #[error("the gateway cannot take clients on its address: {0}")]
  Address(#[from] io::Error),
  #[error("the endpoint of the clients closed")]
  Closed,
}

#[expect(clippy::disallowed_methods, reason = "main starts the runtime of the gateway once")]
#[tokio::main]
async fn main() -> ExitCode {
  eprintln!("the gateway stops: {}", run(open(|name| env::var(name).ok()).await).await);
  ExitCode::FAILURE
}

async fn open(var: impl Fn(&str) -> Option<String>) -> Result<(Endpoint, Arc<Gateway>), Error> {
  let config = Config::read(var)?;
  let mut endpoint = Endpoint::server(config.clients, config.address)?;
  endpoint.set_default_client_config(config.writers);
  let gateway = Gateway::open(config.redis, endpoint.clone(), config.address.is_ipv4()).await?;
  Ok((endpoint, gateway))
}

/// Serves each client of the gateway that `opened` gives in a task of its own until a fault stops
/// it, and gives that fault: why the gateway did not open, or the close of its endpoint.
async fn run(opened: Result<(Endpoint, Arc<Gateway>), Error>) -> Error {
  let (endpoint, gateway) = match opened {
    Ok(opened) => opened,
    Err(error) => return error,
  };
  while let Some(incoming) = endpoint.accept().await {
    tokio::spawn(client::serve(incoming, Arc::clone(&gateway)));
  }
  Error::Closed
}

#[cfg(test)]
mod tests {
  use std::io;
  use std::net::{SocketAddr, UdpSocket};
  use std::time::Duration;

  use mmo_protocol::{
    AccountId, Angle, CharacterId, DATAGRAM, Entity, FRAME, FromClient, Input, Intents, Link,
    PROTOCOL, Position, Relayed, Snapshot, Tick, ToClient, ToZone, Token, Velocity, ZoneId, encode,
    frame, relayed,
  };

  use tokio::time;

  use super::{Error, open, run};
  use crate::world::{ANY, Peer, World, Zone, var};

  const SAY: &str = "hello, all";
  const MALFORMED: &str = "the bytes are not a message of this version of the protocol";
  const STOPPED: &str = "the gateway cannot write to the stream: sending stopped by peer: error 7";
  const ENDED: &str = "the peer ended its stream";
  const LARGE: &str = "a frame holds 65536 bytes at most";
  const SHORT: &str =
    "a relayed datagram starts with a link of 8 bytes, and this one holds 3 bytes";

  #[expect(clippy::unnecessary_wraps, reason = "it stands beside the result of Peer::closed")]
  fn reason(text: &str) -> Result<(u64, String), String> {
    Ok((1, text.to_owned()))
  }

  fn hello(version: u16) -> Vec<u8> {
    let [low, high] = version.to_le_bytes();
    frame(&[low, high, 0, 0, 0, 0]).expect("a hello")
  }

  fn another_version() -> String {
    let theirs = PROTOCOL + 1;
    format!(
      "the peer speaks version {theirs} of the protocol, and this side speaks version {PROTOCOL}"
    )
  }

  fn intents(zone: i32) -> Intents {
    let input = Input { tick: Tick(8), heading: Some(Angle(0)), view: Angle(16384) };
    Intents { zone: ZoneId(zone), ack: Some(Tick(7)), inputs: [input; 3] }
  }

  fn line(text: &str) -> ToClient {
    ToClient::Line { channel: "world".to_owned(), from: "Ada".to_owned(), text: text.to_owned() }
  }

  /// The bytes of `message` and one byte more, which no message of the protocol is.
  fn longer(message: &impl bitcode::Encode) -> Vec<u8> {
    [encode(message), vec![0]].concat()
  }

  /// The link of `client` at the writer `zone`, as the intents of a datagram of the client name
  /// it.
  async fn link_at(client: &Peer, zone: &Peer) -> Link {
    client.send_datagram(&intents(1));
    zone.relayed_of::<Intents>().await.link
  }

  /// Attaches a client of the gateway at `address` for each character of `characters` in zone 1,
  /// takes the link of the gateway at `meadow`, the writer of zone 1, and gives that link and each
  /// client with its link, in the order of `characters`.
  async fn play(
    world: &World,
    address: SocketAddr,
    meadow: &Zone,
    characters: &[i64],
  ) -> (Peer, Vec<(Peer, Link)>) {
    let mut clients = Vec::new();
    for character in characters {
      clients.push(Some(world.attached(address, world.session(11, *character, 1).await).await));
    }
    let mut writer = meadow.accept().await;
    let mut played = Vec::new();
    for client in clients {
      let client = client.expect("a client");
      let link = link_at(&client, &writer).await;
      played.push((client, link));
    }
    let mut joins = Vec::new();
    for _ in characters {
      joins.push(writer.message::<ToZone>().await);
    }
    for ((_, link), character) in played.iter().zip(characters) {
      let join =
        ToZone::Join { link: *link, account: AccountId(11), character: CharacterId(*character) };
      assert!(joins.contains(&join), "{join:?} in {joins:?}");
    }
    (writer, played)
  }

  /// Tells `message` to `to` each 50 ms until `peer` closes, and gives how it closed. A stop of a
  /// stream of `peer` reaches the gateway at no known time, so the first message can pass it.
  async fn until_closed(
    peer: &Peer,
    to: &mut Peer,
    message: &impl bitcode::Encode,
  ) -> Result<(u64, String), String> {
    let mut each = time::interval(Duration::from_millis(50));
    loop {
      tokio::select! {
        closed = peer.closed() => return closed,
        _ = each.tick() => to.tell(message).await,
      }
    }
  }

  /// A full snapshot that takes more bytes than a datagram of a client holds.
  fn crowd() -> Snapshot {
    let mut snapshot =
      Snapshot { tick: Tick(7), base: None, entered: vec![], changed: vec![], left: vec![] };
    let mut id = 0;
    while encode(&snapshot).len() <= DATAGRAM {
      id += 1;
      snapshot.entered.push(Entity {
        id: CharacterId(id.into()),
        position: Position { x: id * 7919, y: -id * 104_729, z: id * 31 },
        velocity: Velocity { x: i16::try_from(id * 17).expect("a speed"), y: 3, z: -5 },
        view: Angle(u16::try_from(id * 97).expect("an angle")),
        animation: 2,
        hit_points: id * 3,
      });
    }
    snapshot
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_client_plays_through_the_gateway_with_the_writer_of_the_zone_of_its_player() {
    let world = World::new().await;
    let meadow = world.zone(1).await;
    let forest = world.zone(2).await;
    let token = world.session(11, 42, 1).await;
    let address = world.gateway().await;
    let (mut client, first) = world.announced(address, token).await;
    assert_eq!(first, ToClient::Moved { zone: ZoneId(1) });
    let mut one = meadow.accept().await;
    let link = link_at(&client, &one).await;
    let join = ToZone::Join { link, account: AccountId(11), character: CharacterId(42) };
    assert_eq!(one.message::<ToZone>().await, join);

    let snapshot =
      Snapshot { tick: Tick(7), base: None, entered: vec![], changed: vec![], left: vec![] };
    one.relay(&Relayed { link: Link(link.0 ^ 1), message: snapshot.clone() });
    one.relay(&Relayed { link, message: snapshot.clone() });
    assert_eq!(client.datagram_of::<Snapshot>().await, snapshot);
    one.datagram(&relayed(link, &[1, 2, 3]));
    assert_eq!(client.bytes_of_datagram().await, [1, 2, 3], "the gateway decodes no snapshot");
    client.send_datagram(&intents(1));
    assert_eq!(one.relayed_of::<Intents>().await, Relayed { link, message: intents(1) });
    let say = FromClient::Say { channel: "world".to_owned(), text: SAY.to_owned() };
    client.tell(&say).await;
    assert_eq!(one.message::<ToZone>().await, ToZone::Client(Relayed { link, message: say }));
    one.tell(&Relayed { link: Link(link.0 ^ 1), message: line("lost") }).await;
    one.tell(&Relayed { link, message: line(SAY) }).await;
    assert_eq!(client.message::<ToClient>().await, line(SAY));

    client.send_datagram(&intents(2));
    let mut two = forest.accept().await;
    assert_eq!(two.relayed_of::<Intents>().await, Relayed { link, message: intents(2) });
    let moved = ToClient::Moved { zone: ZoneId(2) };
    one.tell(&Relayed { link, message: moved.clone() }).await;
    assert_eq!(client.message::<ToClient>().await, moved);
    one.tell(&Relayed { link, message: line("from the meadow") }).await;
    two.tell(&Relayed { link, message: line("from the forest") }).await;
    assert_eq!(client.message::<ToClient>().await, line("from the forest"));
    client.tell(&FromClient::Logout).await;
    let logout = ToZone::Client(Relayed { link, message: FromClient::Logout });
    assert_eq!(two.message::<ToZone>().await, logout);
    client.close();
    assert_eq!(two.message::<ToZone>().await, ToZone::Leave { link });
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_client_that_breaks_the_protocol_before_its_attach_loses_its_connection() {
    let world = World::new().await;
    let address = world.gateway().await;
    let client = async || world.client(address).await.expect("the gateway is trusted");
    let untrusted = World::new().await.client(address).await.err();
    let refused = "the cryptographic handshake failed: error 51: invalid peer certificate: \
                   BadSignature";
    assert_eq!(untrusted.as_deref(), Some(refused));
    let quiet = client().await;
    quiet.close();
    let mut deaf = client().await;
    deaf.recv.stop(7u32.into()).expect("an open stream");
    deaf.write(&hello(PROTOCOL)).await;
    let mut short = client().await;
    short.write(&[5]).await;
    short.send.finish().expect("an open stream");
    let mut large = client().await;
    large.write(&[0x81, 0x80, 0x04]).await;
    let mut newer = client().await;
    newer.write(&hello(PROTOCOL + 1)).await;
    let mut silent = client().await;
    silent.greet().await;
    silent.send.finish().expect("an open stream");
    let mut garbled = client().await;
    garbled.greet().await;
    garbled.write(&frame(&[]).expect("a frame")).await;
    let mut stranger = world.attach(address, Token([0; 32])).await;
    assert_eq!(stranger.message::<ToClient>().await, ToClient::NoSession);
    let mut deaf_stranger = client().await;
    deaf_stranger.greet().await;
    deaf_stranger.recv.stop(7u32.into()).expect("an open stream");
    deaf_stranger.tell(&mmo_protocol::Attach { session: Token([0; 32]) }).await;
    let token = world.session(11, 42, 1).await;
    let mut deaf_player = client().await;
    deaf_player.greet().await;
    deaf_player.recv.stop(7u32.into()).expect("an open stream");
    deaf_player.tell(&mmo_protocol::Attach { session: token }).await;
    let mut reasons = Vec::new();
    for peer in [&quiet, &deaf, &short, &large, &newer, &silent, &garbled, &stranger] {
      reasons.push(peer.closed().await);
    }
    reasons.push(deaf_stranger.closed().await);
    reasons.push(deaf_player.closed().await);
    assert_eq!(
      reasons,
      [
        Err("closed".to_owned()),
        reason(STOPPED),
        reason(ENDED),
        reason(LARGE),
        reason(&another_version()),
        reason(ENDED),
        reason(MALFORMED),
        reason("no session has this token"),
        reason(STOPPED),
        reason(STOPPED),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_session_that_the_gateway_cannot_serve_ends_the_connection_of_its_client() {
    let world = World::new().await;
    let other = World::new().await;
    let strange = other.zone(1).await;
    let newer = world.zone(5).await;
    world.entry(3, "localhost", 1).await;
    world.entry(4, "[::1]:4434", 1).await;
    world.entry(6, "localhost:0", 1).await;
    world.entry(7, &strange.address(), 1).await;
    let address = world.gateway().await;
    let mut reasons = Vec::new();
    for zone in [9, 3, 4, 6] {
      let client = world.attached(address, world.session(11, 42, zone).await).await;
      reasons.push(client.closed().await);
    }
    let client = world.attached(address, world.session(11, 42, 7).await).await;
    reasons.push(Err(strange.refused().await));
    reasons.push(client.closed().await);
    let client = world.attached(address, world.session(11, 42, 5).await).await;
    let refused = newer.accept_with(PROTOCOL + 1).await;
    reasons.push(client.closed().await);
    drop(refused);
    for keys in ["~zone:*", "~session:*"] {
      let token = world.session(11, 42, 1).await;
      world.grant(keys, "+@all").await;
      reasons.push(world.attach(address, token).await.closed().await);
    }
    assert_eq!(
      reasons,
      [
        reason("the directory names no writer of zone 9"),
        reason("the gateway cannot resolve the address of a zone writer: invalid socket address"),
        reason("the address [::1]:4434 of a zone writer names no host that the gateway reaches"),
        reason("the gateway cannot link to a zone writer: invalid remote address: 127.0.0.1:0"),
        Err("aborted by peer: the cryptographic handshake failed: error 51: invalid peer certificate: \
             BadSignature"
          .to_owned()),
        reason(
          "the link fails: the cryptographic handshake failed: error 51: invalid peer certificate: BadSignature"
        ),
        reason(&another_version()),
        reason("Redis fails: NoPerm: No permissions to access a key"),
        reason("Redis fails: NoPerm: No permissions to access a key"),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn each_fault_of_a_playing_client_ends_its_connection_alone_with_its_reason() {
    let world = World::new().await;
    let meadow = world.zone(1).await;
    let address = world.gateway().await;
    let (writer, mut played) = play(&world, address, &meadow, &[1, 2, 3, 4, 5, 6]).await;
    played[0].0.write(&frame(&longer(&FromClient::Logout)).expect("a frame")).await;
    played[1].0.datagram(&longer(&intents(1)));
    played[2].0.send_datagram(&intents(9));
    played[3].0.send.finish().expect("an open stream");
    let say = |text: &str| FromClient::Say { channel: "world".to_owned(), text: text.to_owned() };
    let mut text = "a".repeat(FRAME);
    while encode(&say(&text)).len() > FRAME {
      text.pop();
    }
    played[4].0.tell(&say(&text)).await;
    let mut reasons = Vec::new();
    for (client, _) in &played[..5] {
      reasons.push(client.closed().await);
    }
    let (client, link) = &mut played[5];
    client.send_datagram(&intents(1));
    let relayed = writer.relayed_of::<Intents>().await;
    assert_eq!(relayed, Relayed { link: *link, message: intents(1) });
    assert_eq!(
      reasons,
      [
        reason(MALFORMED),
        reason(MALFORMED),
        reason("the directory names no writer of zone 9"),
        reason(ENDED),
        reason(LARGE),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_message_of_the_writer_that_a_client_cannot_take_ends_that_client_alone() {
    let world = World::new().await;
    let meadow = world.zone(1).await;
    let address = world.gateway().await;
    let (mut writer, mut played) = play(&world, address, &meadow, &[1, 2, 3, 4]).await;
    writer.tell(&Relayed { link: played[0].1, message: ToClient::Moved { zone: ZoneId(9) } }).await;
    writer.relay(&Relayed { link: played[2].1, message: crowd() });
    played[1].0.recv.stop(7u32.into()).expect("an open stream");
    let line = Relayed { link: played[1].1, message: line(SAY) };
    let reasons = [
      played[0].0.closed().await,
      until_closed(&played[1].0, &mut writer, &line).await,
      played[2].0.closed().await,
    ];
    writer.tell(&Relayed { link: played[3].1, message: self::line(SAY) }).await;
    assert_eq!(played[3].0.message::<ToClient>().await, self::line(SAY));
    let size = encode(&crowd()).len();
    assert_eq!(
      reasons,
      [
        reason("the directory names no writer of zone 9"),
        reason(STOPPED),
        reason(&format!("a datagram holds 1100 bytes at most, and this one holds {size}")),
      ]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn each_fault_of_a_zone_writer_ends_its_link_and_its_clients() {
    let world = World::new().await;
    let zones = [world.zone(1).await, world.zone(2).await, world.zone(3).await];
    let address = world.gateway().await;
    let mut clients = Vec::new();
    let mut writers = Vec::new();
    for (zone, writer) in (1..).zip(&zones) {
      clients.push(world.attached(address, world.session(11, 42, zone).await).await);
      writers.push(writer.accept().await);
    }
    writers[0].write(&frame(&[]).expect("a frame")).await;
    writers[1].datagram(&[1, 2, 3]);
    writers[2].recv.stop(7u32.into()).expect("an open stream");
    let say = FromClient::Say { channel: "world".to_owned(), text: SAY.to_owned() };
    let mut reasons = vec![
      writers[0].closed().await,
      writers[1].closed().await,
      until_closed(&writers[2], &mut clients[2], &say).await,
    ];
    for client in &clients {
      reasons.push(client.closed().await);
    }
    let lost = |zone: i32| reason(&format!("the link to the writer of zone {zone} ends: closed"));
    assert_eq!(
      reasons,
      [reason(MALFORMED), reason(SHORT), reason(STOPPED), lost(1), lost(2), lost(3)]
    );
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_change_of_the_directory_ends_the_link_to_the_old_writer_and_its_clients() {
    let world = World::new().await;
    let meadow = world.zone(1).await;
    let forest = world.zone(2).await;
    let address = world.gateway().await;
    let mut ada = world.attached(address, world.session(11, 42, 1).await).await;
    let mut one = meadow.accept().await;
    let link = link_at(&ada, &one).await;
    let bob = world.attached(address, world.session(12, 43, 2).await).await;
    let two = forest.accept().await;

    world.grant("~*", "-hmget").await;
    world.publish(1).await;
    let cy = world.attach(address, world.session(13, 44, 7).await).await;
    let denied = "Redis fails: NoPerm: User gateway has no permissions to run the 'hmget' command";
    assert_eq!(cy.closed().await, reason(denied));
    one.tell(&Relayed { link, message: line(SAY) }).await;
    assert_eq!(ada.message::<ToClient>().await, line(SAY));

    world.grant("~*", "+@all").await;
    world.entry(1, &meadow.address(), 2).await;
    world.publish(1).await;
    assert_eq!(ada.closed().await, reason("the link to the writer of zone 1 ends: closed"));
    assert_eq!(one.closed().await, reason("the directory names another writer of the zone"));
    let mut dee = world.attached(address, world.session(14, 45, 1).await).await;
    let mut again = meadow.accept().await;
    let link = link_at(&dee, &again).await;
    again.tell(&Relayed { link, message: line(SAY) }).await;
    assert_eq!(dee.message::<ToClient>().await, line(SAY));

    two.close();
    let lost = "the link to the writer of zone 2 ends: closed by peer: 0";
    assert_eq!(bob.closed().await, reason(lost));
    forest.close();
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_gateway_stops_on_a_variable_on_a_redis_and_on_an_address_that_it_cannot_use() {
    let world = World::new().await;
    let unset = run(open(var(Vec::new())).await).await.to_string();
    assert_eq!(unset, "the variable GATEWAY_ADDRESS is not set");
    let mut vars = world.vars(ANY);
    vars.retain(|(name, _)| *name != "REDIS_URL");
    vars.push(("REDIS_URL", "redis://127.0.0.1:1".to_owned()));
    let unreachable = run(open(var(vars)).await).await.to_string();
    assert!(unreachable.starts_with("Redis refuses the gateway: "), "{unreachable}");
    world.grant("~*", "-subscribe").await;
    let deaf = run(open(var(world.vars(ANY))).await).await.to_string();
    let denied = "NoPerm: User gateway has no permissions to run the 'subscribe' command";
    assert_eq!(deaf, format!("Redis refuses the gateway: {denied}"));
    world.grant("~*", "+@all").await;
    let taken = UdpSocket::bind("127.0.0.1:0").expect("a socket");
    let taken = taken.local_addr().expect("an address").to_string();
    let in_use = run(open(var(world.vars(&taken))).await).await;
    assert!(
      matches!(in_use, Error::Address(ref error) if error.kind() == io::ErrorKind::AddrInUse),
      "{in_use}"
    );
    let (endpoint, gateway) = open(var(world.vars(ANY))).await.expect("a gateway");
    endpoint.close(0u32.into(), b"");
    let closed = run(Ok((endpoint, gateway))).await.to_string();
    assert_eq!(closed, "the endpoint of the clients closed");
  }
}
