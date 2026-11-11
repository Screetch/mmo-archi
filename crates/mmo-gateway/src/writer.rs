//! The link of the gateway to one zone writer, which the clients of that zone share.

use std::collections::BTreeMap;
use std::sync::Arc;

use mmo_protocol::{
  AccountId, CharacterId, FromClient, Link, Relayed, Snapshot, ToClient, ToZone, ZoneId, decode,
  encode, frame,
};
use quinn::{Connection, SendStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::gateway::Gateway;
use crate::link::{FAULT, Fault, Stream, framed, greet};

/// The entry of a zone in the directory: the address of its writer, as `host:port`, and the epoch
/// of its claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
  pub(crate) address: String,
  pub(crate) epoch: i64,
}

/// A message of the zone writer for one client.
pub(crate) enum Down {
  Message(ToClient),
  Snapshot(Snapshot),
}

/// The link to the writer of one zone. Each clone sends on the same link.
#[derive(Clone)]
pub(crate) struct Writer {
  pub(crate) zone: ZoneId,
  entry: Entry,
  connection: Connection,
  orders: UnboundedSender<Order>,
}

/// What a client asks of the task of the link.
enum Order {
  /// Relays each message of the writer for `link` to `to`.
  Route { link: Link, to: UnboundedSender<Down> },
  /// Relays no more message for the link.
  Unroute(Link),
  /// Writes a frame on the stream of the writer.
  Tell(Vec<u8>),
}

impl Writer {
  pub(crate) fn entry(&self) -> &Entry {
    &self.entry
  }

  pub(crate) fn id(&self) -> usize {
    self.connection.stable_id()
  }

  /// Ends the link, since the directory names another writer of the zone, or none.
  pub(crate) fn end(&self) {
    self.connection.close(FAULT, b"the directory names another writer of the zone");
  }

  /// Joins the character of a session to the zone on `link`, and relays the messages of the
  /// writer for that link to `to`.
  pub(crate) fn join(
    &self,
    link: Link,
    (account, character): (AccountId, CharacterId),
    to: UnboundedSender<Down>,
  ) {
    self.order(Order::Route { link, to });
    self.order(Order::Tell(framed(&encode(&ToZone::Join { link, account, character }))));
  }

  /// Relays the messages of the writer for `link` to `to`, after a move of its player into this
  /// zone.
  pub(crate) fn follow(&self, link: Link, to: UnboundedSender<Down>) {
    self.order(Order::Route { link, to });
  }

  /// Relays no more message of the writer for `link`, after a move of its player out of the zone.
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
    Fault::Lost { zone: self.zone, why: self.connection.closed().await }
  }

  fn order(&self, order: Order) {
    // The send fails only when the task of the link ended, and then each client of the link sees
    // the end of the link and ends with it.
    drop(self.orders.send(order));
  }
}

/// Greets the writer of `zone` on `connection`, and relays the link in a task of its own until a
/// fault ends it.
pub(crate) async fn open(
  gateway: Arc<Gateway>,
  zone: ZoneId,
  entry: Entry,
  connection: Connection,
) -> Result<Writer, Fault> {
  let (send, stream) = greet(connection.open_bi().await).await?;
  let (orders, heard) = unbounded_channel();
  let relay =
    Relay { connection: connection.clone(), send, stream, orders: heard, routes: BTreeMap::new() };
  tokio::spawn(relay.run(gateway, zone));
  Ok(Writer { zone, entry, connection, orders })
}

/// The task of a link: it writes the orders of the clients, and relays each message of the writer
/// to the client of its link.
struct Relay {
  connection: Connection,
  send: SendStream,
  stream: Stream,
  orders: UnboundedReceiver<Order>,
  routes: BTreeMap<u64, UnboundedSender<Down>>,
}

impl Relay {
  async fn run(mut self, gateway: Arc<Gateway>, zone: ZoneId) {
    let fault = self.relay().await;
    eprintln!("the link to the writer of zone {} ends: {fault}", zone.0);
    // A close after the close of the writer would hide why the writer closed from the clients.
    if self.connection.close_reason().is_none() {
      self.connection.close(FAULT, fault.to_string().as_bytes());
    }
    gateway.forget(zone, self.connection.stable_id()).await;
  }

  async fn relay(&mut self) -> Fault {
    loop {
      let done = tokio::select! {
        biased;
        Some(order) = self.orders.recv() => self.order(order).await,
        payload = self.stream.next() => payload.and_then(|payload| {
          let Relayed { link, message } = decode(&payload)?;
          self.deliver(link, Down::Message(message));
          Ok(())
        }),
        datagram = self.connection.read_datagram() => datagram.map_err(Fault::from).and_then(|bytes| {
          let Relayed { link, message } = decode(&bytes)?;
          self.deliver(link, Down::Snapshot(message));
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

  /// Gives `down` to the client of `link`. The writer can send for a link that left, until it
  /// reads the leave.
  fn deliver(&self, link: Link, down: Down) {
    if let Some(to) = self.routes.get(&link.0) {
      // The send fails only when the client ended, and its leave comes next.
      drop(to.send(down));
    }
  }
}
