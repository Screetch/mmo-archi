//! The play of the client: its link to a gateway.

use mmo_protocol::{Attach, FromClient, Intents, Snapshot, ToClient, Token, datagram, decode};
use quinn::VarInt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::Fault;
use crate::connect::{Link, Trust, open};

/// The code with which the client closes the link when it leaves.
const LEAVE: VarInt = VarInt::from_u32(0);

/// What the gateway gives to the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Down {
  /// The entities that the player sees, from a datagram.
  Snapshot(Snapshot),
  /// A message on stream 0. [`ToClient::NoSession`] says that the session of the token ended, so
  /// the client logs in again.
  Message(ToClient),
}

/// What the client gives to the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Up {
  /// The inputs of the player, in a datagram.
  Intents(Intents),
  /// A line of chat, on stream 0.
  Say {
    /// The name of the channel.
    channel: String,
    /// The line.
    text: String,
  },
  /// The end of the play of the character, on stream 0.
  Logout,
}

/// The link of the client to a gateway, after its attach.
pub struct Play {
  link: Link,
}

impl Play {
  /// Connects to the gateway at `address`, as `host:port`, and attaches the character of the
  /// session `token`. The gateway answers a session that it does not know with
  /// [`ToClient::NoSession`].
  pub async fn attach(trust: &Trust, address: &str, token: Token) -> Result<Self, Fault> {
    let mut link = open(trust, address).await?;
    let written = link.write(&Attach { session: token }).await;
    written.map(|()| Self { link })
  }

  /// The next snapshot or message of the gateway.
  pub async fn next(&mut self) -> Result<Down, Fault> {
    let connection = self.link.connection.clone();
    tokio::select! {
      biased;
      datagram = connection.read_datagram() => datagram
        .map_err(Fault::from)
        .and_then(|bytes| decode(&bytes).map_err(Fault::from))
        .map(Down::Snapshot),
      message = self.link.read() => message.map(Down::Message),
    }
  }

  /// Sends `up` to the gateway.
  #[expect(
    clippy::expect_used,
    reason = "the inputs of 3 ticks take 30 bytes, and a datagram holds 1100"
  )]
  pub async fn send(&mut self, up: Up) -> Result<(), Fault> {
    match up {
      Up::Intents(intents) => {
        let bytes = datagram(&intents).expect("an intent datagram fits a datagram");
        Ok(self.link.connection.send_datagram(bytes.into())?)
      }
      Up::Say { channel, text } => self.link.write(&FromClient::Say { channel, text }).await,
      Up::Logout => self.link.write(&FromClient::Logout).await,
    }
  }

  /// Gives each message of the gateway to `down`, and sends each message of `up`, until a fault
  /// ends the link: the fault is the answer. It ends with [`Fault::Closed`] when the game drops
  /// the other side of a channel, and closes the link when it ends.
  pub async fn run(mut self, mut up: UnboundedReceiver<Up>, down: UnboundedSender<Down>) -> Fault {
    let fault = loop {
      let done = tokio::select! {
        got = self.next() => match got {
          Ok(message) => down.send(message).map_err(|_unheard| Fault::Closed),
          Err(fault) => Err(fault),
        },
        got = up.recv() => match got {
          Some(message) => self.send(message).await,
          None => Err(Fault::Closed),
        },
      };
      if let Err(fault) = done {
        break fault;
      }
    };
    self.link.connection.close(LEAVE, b"the client leaves");
    fault
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    Angle, Attach, CharacterId, FromClient, Input, Intents, Snapshot, Tick, ToClient, Token,
    ZoneId, datagram, decode,
  };
  use tokio::sync::mpsc::unbounded_channel;

  use super::{Down, Play, Up};
  use crate::testing::Fake;

  const TOKEN: Token = Token([3; 32]);

  fn intents() -> Intents {
    let input = |tick| Input { tick: Tick(tick), heading: Some(Angle(0)), view: Angle(16384) };
    Intents {
      zone: ZoneId(1),
      layer: 1,
      ack: Some(Tick(9)),
      inputs: [input(10), input(11), input(12)],
    }
  }

  fn snapshot() -> Snapshot {
    Snapshot {
      tick: Tick(12),
      base: None,
      entered: vec![],
      changed: vec![],
      left: vec![CharacterId(5)],
    }
  }

  fn say() -> Up {
    Up::Say { channel: "world".to_owned(), text: "hello".to_owned() }
  }

  fn fault<T>(result: Result<T, crate::Fault>) -> Result<(), String> {
    result.map(drop).map_err(|fault| fault.to_string())
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_attaches_and_exchanges_streams_and_datagrams_with_the_gateway() {
    let fake = Fake::start(|mut peer| async move {
      assert_eq!(peer.read::<Attach>().await, Attach { session: TOKEN });
      peer.write(&ToClient::Moved { zone: ZoneId(2), layer: 1 }).await;
      let sent = peer.connection.read_datagram().await.expect("a datagram");
      assert_eq!(decode::<Intents>(&sent).ok(), Some(intents()));
      let bytes = datagram(&snapshot()).expect("a datagram");
      peer.connection.send_datagram(bytes.into()).expect("sent");
      let line = FromClient::Say { channel: "world".to_owned(), text: "hello".to_owned() };
      assert_eq!(peer.read::<FromClient>().await, line);
      assert_eq!(peer.read::<FromClient>().await, FromClient::Logout);
      peer.write(&ToClient::NoSession).await;
      peer
    });
    let mut play = Play::attach(&fake.trust, &fake.address, TOKEN).await.expect("attached");
    assert_eq!(
      play.next().await.ok(),
      Some(Down::Message(ToClient::Moved { zone: ZoneId(2), layer: 1 }))
    );
    play.send(Up::Intents(intents())).await.expect("sent");
    assert_eq!(play.next().await.ok(), Some(Down::Snapshot(snapshot())));
    play.send(say()).await.expect("sent");
    play.send(Up::Logout).await.expect("sent");
    assert_eq!(play.next().await.ok(), Some(Down::Message(ToClient::NoSession)));
    let long = Up::Say { channel: "world".to_owned(), text: "x".repeat(70_000) };
    let refused = fault(play.send(long).await);
    assert_eq!(refused, Err("a frame holds 65536 bytes at most".to_owned()));
    drop(play);
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_names_what_ends_the_stream_or_the_datagrams_of_the_gateway() {
    let fake = Fake::start(|mut peer| async move {
      peer.read::<Attach>().await;
      peer.connection.send_datagram(vec![0xff; 4].into()).expect("sent");
      peer.write(&ToClient::Full { zone: ZoneId(4) }).await;
      peer.finish();
      peer
    });
    let mut play = Play::attach(&fake.trust, &fake.address, TOKEN).await.expect("attached");
    let garbled = "the bytes are not a message of this version of the protocol";
    assert_eq!(fault(play.next().await), Err(garbled.to_owned()));
    assert_eq!(play.next().await.ok(), Some(Down::Message(ToClient::Full { zone: ZoneId(4) })));
    assert_eq!(fault(play.next().await), Err("the server ended its stream".to_owned()));
    drop(play);
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_link_that_the_gateway_closed_refuses_each_message_and_the_attach_refuses_a_bad_address()
   {
    let fake = Fake::start(|mut peer| async move {
      peer.read::<Attach>().await;
      peer.connection.close(7_u32.into(), b"bye");
      peer
    });
    let mut play = Play::attach(&fake.trust, &fake.address, TOKEN).await.expect("attached");
    play.link.connection.closed().await;
    let closed = "connection lost";
    let sent = fault(play.send(Up::Intents(intents())).await);
    assert_eq!(sent, Err(format!("the client cannot send a datagram: {closed}")));
    let sent = fault(play.send(Up::Logout).await);
    assert_eq!(sent, Err(format!("the client cannot write to the stream: {closed}")));
    let refused = fault(Play::attach(&fake.trust, "127.0.0.1:0", TOKEN).await);
    assert!(refused.expect_err("no port").starts_with("the client cannot connect: "));
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_run_carries_each_message_both_ways_until_the_game_leaves() {
    let fake = Fake::start(|mut peer| async move {
      assert_eq!(peer.read::<Attach>().await, Attach { session: TOKEN });
      assert_eq!(peer.read::<FromClient>().await, FromClient::Logout);
      peer.write(&ToClient::Full { zone: ZoneId(4) }).await;
      peer
    });
    let play = Play::attach(&fake.trust, &fake.address, TOKEN).await.expect("attached");
    let (up, from) = unbounded_channel();
    let (to, mut heard) = unbounded_channel();
    let running = tokio::spawn(play.run(from, to));
    up.send(Up::Logout).expect("the run listens");
    assert_eq!(heard.recv().await, Some(Down::Message(ToClient::Full { zone: ZoneId(4) })));
    drop(up);
    let ended = running.await.expect("the run ends");
    assert_eq!(ended.to_string(), "the game closed the link");
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_run_ends_when_the_game_stops_listening_or_the_link_fails() {
    let talking = Fake::start(|mut peer| async move {
      peer.write(&ToClient::Full { zone: ZoneId(4) }).await;
      peer
    });
    let play = Play::attach(&talking.trust, &talking.address, TOKEN).await.expect("attached");
    let (_up, from) = unbounded_channel();
    let (to, heard) = unbounded_channel();
    drop(heard);
    assert_eq!(play.run(from, to).await.to_string(), "the game closed the link");
    talking.end().await;

    let closing = Fake::start(|mut peer| async move {
      peer.read::<Attach>().await;
      peer.connection.close(7_u32.into(), b"bye");
      peer
    });
    let play = Play::attach(&closing.trust, &closing.address, TOKEN).await.expect("attached");
    let (_up, from) = unbounded_channel();
    let (to, _heard) = unbounded_channel();
    let ended = play.run(from, to).await.to_string();
    assert_eq!(ended, "the link fails: closed by peer: bye (code 7)");
    closing.end().await;
  }
}
