//! The link of the gateway to one layer writer, which the clients of that layer share.

use std::collections::BTreeMap;
use std::sync::Arc;

use mmo_protocol::{
  AccountId, CharacterId, DATAGRAM, FromClient, Layer, Link, Relayed, ShardId, ToClient, ToZone,
  decode, encode, frame, unrelayed,
};
use mmo_store::Serving;
use quinn::{Connection, SendStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::gateway::Gateway;
use crate::link::{FAULT, Fault, Stream, framed, greet};

/// The link to the writer of one layer. Each clone sends on the same link.
#[derive(Clone)]
pub(crate) struct Writer {
  pub(crate) shard: ShardId,
  pub(crate) layer: Layer,
  entry: Serving,
  connection: Connection,
  orders: UnboundedSender<Order>,
}

/// Where the task of the link relays what the writer sends to one client: each message to the
/// task of the client, and each snapshot straight to its connection, as the bytes that the writer
/// made, so a snapshot costs the gateway no decode, no encode and no wake of the task of the
/// client.
#[derive(Clone)]
pub(crate) struct Route {
  pub(crate) messages: UnboundedSender<ToClient>,
  pub(crate) connection: Connection,
}

/// What a client asks of the task of the link.
enum Order {
  /// Relays each message and each snapshot of the writer for `link` to `to`.
  Route { link: Link, to: Route },
  /// Relays no more message for the link.
  Unroute(Link),
  /// Writes a frame on the stream of the writer.
  Tell(Vec<u8>),
}

impl Writer {
  pub(crate) fn entry(&self) -> &Serving {
    &self.entry
  }

  pub(crate) fn id(&self) -> usize {
    self.connection.stable_id()
  }

  /// Ends the link, since the directory names another writer of the layer, or none.
  pub(crate) fn end(&self) {
    self.connection.close(FAULT, b"the directory names another writer of the layer");
  }

  /// Joins the character of a session to the layer on `link`, and relays the messages of the
  /// writer for that link to `to`.
  pub(crate) fn join(&self, link: Link, (account, character): (AccountId, CharacterId), to: Route) {
    self.order(Order::Route { link, to });
    self.order(Order::Tell(framed(&encode(&ToZone::Join { link, account, character }))));
  }

  /// Relays no more message of the writer for `link`, after a move of its player out of the
  /// layer.
  pub(crate) fn forsake(&self, link: Link) {
    self.order(Order::Unroute(link));
  }

  /// Tells the writer that the connection of `link` closed.
  pub(crate) fn leave(&self, link: Link) {
    self.order(Order::Tell(framed(&encode(&ToZone::Leave { link }))));
    self.order(Order::Unroute(link));
  }

  /// Relays a message of the client of `link` on the stream of the writer. The link makes the
  /// message larger, so a message that fills a frame of the client fills no frame of the writer.
  pub(crate) fn relay(&self, link: Link, message: FromClient) -> Result<(), Fault> {
    let bytes = frame(&encode(&ToZone::Client(Relayed { link, message })))?;
    self.order(Order::Tell(bytes));
    Ok(())
  }

  /// Sends a datagram of a client, which the gateway made, to the writer.
  pub(crate) fn intents(&self, datagram: Vec<u8>) -> Result<(), Fault> {
    self.connection.send_datagram(datagram.into()).map_err(Fault::from)
  }

  /// Ends when the link ends, with why.
  pub(crate) async fn lost(&self) -> Fault {
    Fault::Lost { layer: self.layer, why: self.connection.closed().await }
  }

  fn order(&self, order: Order) {
    // The send fails only when the task of the link ended, and then each client of the link sees
    // the end of the link and ends with it.
    drop(self.orders.send(order));
  }
}

/// Greets the writer of a layer of a shard on `connection`, and relays the link in a task of its
/// own until a fault ends it.
pub(crate) async fn open(
  gateway: Arc<Gateway>,
  (shard, layer): (ShardId, Layer),
  entry: Serving,
  connection: Connection,
) -> Result<Writer, Fault> {
  let (send, stream) = greet(connection.open_bi().await).await?;
  let (orders, heard) = unbounded_channel();
  let relay =
    Relay { connection: connection.clone(), send, stream, orders: heard, routes: BTreeMap::new() };
  tokio::spawn(relay.run(gateway, shard, layer));
  Ok(Writer { shard, layer, entry, connection, orders })
}

/// The task of a link: it writes the orders of the clients, and relays each message of the writer
/// to the client of its link.
struct Relay {
  connection: Connection,
  send: SendStream,
  stream: Stream,
  orders: UnboundedReceiver<Order>,
  routes: BTreeMap<u64, Route>,
}

impl Relay {
  async fn run(mut self, gateway: Arc<Gateway>, shard: ShardId, layer: Layer) {
    let fault = self.relay().await;
    eprintln!("the link to the writer of layer {}:{} ends: {fault}", layer.zone.0, layer.number);
    // A close after the close of the writer would hide why the writer closed from the clients.
    if self.connection.close_reason().is_none() {
      self.connection.close(FAULT, fault.to_string().as_bytes());
    }
    gateway.forget(shard, layer, self.connection.stable_id()).await;
  }

  async fn relay(&mut self) -> Fault {
    loop {
      let done = tokio::select! {
        biased;
        Some(order) = self.orders.recv() => self.order(order).await,
        payload = self.stream.next() => payload.and_then(|payload| {
          let Relayed { link, message } = decode(&payload)?;
          self.deliver(link, message);
          Ok(())
        }),
        datagram = self.connection.read_datagram() => datagram.map_err(Fault::from).and_then(|bytes| {
          let (link, snapshot) = unrelayed(&bytes)?;
          if let Some(route) = self.routes.get(&link.0) {
            let size = snapshot.len();
            let sent = if size > DATAGRAM {
              Err(Fault::Codec(mmo_protocol::Error::Datagram { size }))
            } else {
              route.connection.send_datagram(bytes.slice(bytes.len() - size..)).map_err(Fault::from)
            };
            // The fault of one client ends that client alone, and its leave comes next.
            if let Err(fault) = sent {
              route.connection.close(FAULT, fault.to_string().as_bytes());
            }
          }
          Ok(())
        }),
      };
      if let Err(fault) = done {
        return fault;
      }
    }
  }

  async fn order(&mut self, order: Order) -> Result<(), Fault> {
    match order {
      Order::Route { link, to } => {
        self.routes.insert(link.0, to);
      }
      Order::Unroute(link) => {
        self.routes.remove(&link.0);
      }
      Order::Tell(bytes) => self.send.write_all(&bytes).await?,
    }
    Ok(())
  }

  /// Gives `message` to the client of `link`. The writer can send for a link that left, until it
  /// reads the leave.
  fn deliver(&self, link: Link, message: ToClient) {
    if let Some(to) = self.routes.get(&link.0) {
      // The send fails only when the client ended, and its leave comes next.
      drop(to.messages.send(message));
    }
  }
}
