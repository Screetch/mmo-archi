//! The connection of one client, from its attach to its end.

use std::convert::Infallible;
use std::sync::Arc;

use mmo_protocol::{
  Attach, FromClient, Intents, Layer, Link, ShardId, ToClient, decode, encode, relayed,
};
use quinn::{Connection, ConnectionError, Incoming, SendStream};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::gateway::Gateway;
use crate::link::{FAULT, Fault, Stream, framed, greet};
use crate::writer::{Route, Writer};

/// Serves the client of `incoming` until a fault ends its connection, and closes it with that
/// fault as its reason.
pub(crate) async fn serve(incoming: Incoming, gateway: Arc<Gateway>) {
  let fault = match incoming.await {
    Ok(connection) => {
      let Err(fault) = attach(&connection, &gateway).await;
      connection.close(FAULT, fault.to_string().as_bytes());
      fault
    }
    Err(error) => Fault::Link(error),
  };
  eprintln!("the connection of a client ends: {fault}");
}

/// Takes the stream that the client opens. Each side writes its hello first, then the client
/// sends its [`Attach`], and the gateway tells it the zone and the layer of its session as
/// [`ToClient::Moved`], then joins the character of the session to that layer.
async fn attach(connection: &Connection, gateway: &Arc<Gateway>) -> Result<Infallible, Fault> {
  let (mut send, mut stream) = greet(connection.accept_bi().await).await?;
  let Attach { session } = decode(&stream.next().await?)?;
  let Some(session) = gateway.session(&session).await? else {
    return Err(refuse(send).await);
  };
  let link = random();
  let first = ToClient::Moved { zone: session.layer.zone, layer: session.layer.number };
  send.write_all(&framed(&encode(&first))).await?;
  let shard = session.shard;
  let writer = gateway.writer(shard, session.layer).await?;
  let (messages, down) = unbounded_channel();
  let to = Route { messages, connection: connection.clone() };
  writer.join(link, (session.account, session.character), to.clone());
  let mut client = Client { connection, gateway, shard, link, writer, to, send, stream, down };
  let fault = client.play().await;
  client.writer.leave(link);
  Err(fault)
}

/// Tells the client that no session has its token, and waits until the message reaches it.
#[expect(clippy::expect_used, reason = "the gateway neither finished nor reset the stream")]
async fn refuse(mut send: SendStream) -> Fault {
  if let Err(fault) = send.write_all(&framed(&encode(&ToClient::NoSession))).await {
    return fault.into();
  }
  send.finish().expect("the stream is open");
  // The client got the message, stopped the stream or closed the link: each ends the wait.
  drop(send.stopped().await);
  Fault::NoSession
}

/// A client that plays in a shard, and the link to the writer of the layer of its player.
struct Client<'a> {
  connection: &'a Connection,
  gateway: &'a Arc<Gateway>,
  shard: ShardId,
  link: Link,
  writer: Writer,
  to: Route,
  send: SendStream,
  stream: Stream,
  down: UnboundedReceiver<ToClient>,
}

impl Client<'_> {
  /// Relays each message of the client and of the writer until a fault. When the link closes, its
  /// datagrams fail first, so each close ends here in one way.
  async fn play(&mut self) -> Fault {
    loop {
      let done = tokio::select! {
        biased;
        datagram = self.connection.read_datagram() => self.intents(datagram).await,
        payload = self.stream.next() => self.relay(payload),
        Some(message) = self.down.recv() => self.deliver(message).await,
        fault = self.writer.lost() => Err(fault),
      };
      if let Err(fault) = done {
        return fault;
      }
    }
  }

  fn relay(&self, payload: Result<Vec<u8>, Fault>) -> Result<(), Fault> {
    let message: FromClient = decode(&payload?)?;
    self.writer.relay(self.link, message)
  }

  /// Relays the intents of a datagram to the writer of the layer that they name, in the shard of
  /// the session.
  async fn intents(
    &self,
    datagram: Result<impl AsRef<[u8]>, ConnectionError>,
  ) -> Result<(), Fault> {
    let datagram = datagram?;
    let message: Intents = decode(datagram.as_ref())?;
    let bytes = relayed(self.link, datagram.as_ref());
    let layer = Layer { zone: message.zone, number: message.layer };
    if layer == self.writer.layer {
      return self.writer.intents(bytes);
    }
    self.gateway.writer(self.shard, layer).await?.intents(bytes)
  }

  async fn deliver(&mut self, message: ToClient) -> Result<(), Fault> {
    if let ToClient::Moved { zone, layer } = message {
      let next = self.gateway.writer(self.shard, Layer { zone, number: layer }).await?;
      next.follow(self.link, self.to.clone());
      self.writer.forsake(self.link);
      self.writer = next;
    }
    Ok(self.send.write_all(&framed(&encode(&message))).await?)
  }
}

#[expect(clippy::expect_used, reason = "a server whose system gives no random bytes cannot run")]
fn random() -> Link {
  Link(getrandom::u64().expect("the random source of the system gives 8 bytes"))
}
