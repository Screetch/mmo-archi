//! The link of the layer writer to a peer that answers each message in order: the keeper, and the
//! writer of the layer that a player crosses to.

use std::io;

use mmo_protocol::{Frames, Hello, frame};
use quinn::{
  ClientConfig, ConnectError, Connection, ConnectionError, Endpoint, ReadError, RecvStream,
  SendStream, WriteError,
};
use tokio::net::lookup_host;

use crate::link::BUILD;

/// Why a peer takes no message of the layer writer. `peer` names it: the keeper or a neighbour.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
  #[error("the layer writer cannot find the {peer} at {address}: {source}")]
  Resolve { peer: &'static str, address: String, source: io::Error },
  #[error(
    "the layer writer finds no address of the {peer} at {address} that its endpoint can dial"
  )]
  Unresolved { peer: &'static str, address: String },
  #[error("the layer writer cannot dial the {peer}: {source}")]
  Dial { peer: &'static str, source: ConnectError },
  #[error("the link to the {peer} fails: {source}")]
  Link { peer: &'static str, source: ConnectionError },
  #[error("the stream of the {peer} fails: {source}")]
  Read { peer: &'static str, source: ReadError },
  #[error("the layer writer cannot write to the {peer}: {source}")]
  Write { peer: &'static str, source: WriteError },
  #[error("the {peer} ended its stream")]
  Ended { peer: &'static str },
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
}

/// What a layer writer needs to dial a peer: its endpoint, its certificate, and the family of the
/// address of the endpoint.
#[derive(Clone)]
pub(crate) struct Dial {
  pub(crate) endpoint: Endpoint,
  pub(crate) client: ClientConfig,
  /// True when the endpoint binds an address of IPv4, which dials only an address of IPv4.
  pub(crate) ipv4: bool,
}

impl Dial {
  /// The link to the peer `peer` at `address`, as `host:port`. The name of the host is the name
  /// that the certificate of the peer must hold.
  pub(crate) fn to(&self, address: String, peer: &'static str) -> Remote {
    Remote::new(self.endpoint.clone(), self.client.clone(), address, self.ipv4, peer)
  }
}

/// The link to a peer. It dials the peer at the first message, and again at the next message
/// after a fault ends the link.
pub(crate) struct Remote {
  endpoint: Endpoint,
  client: ClientConfig,
  address: String,
  ipv4: bool,
  peer: &'static str,
  stream: Option<Stream>,
}

impl Remote {
  pub(crate) fn new(
    endpoint: Endpoint,
    client: ClientConfig,
    address: String,
    ipv4: bool,
    peer: &'static str,
  ) -> Self {
    Self { endpoint, client, address, ipv4, peer, stream: None }
  }

  /// Sends `payload` in a frame and gives the answer that `read` makes of the payload of the next
  /// frame. A fault of the link, and an answer that `read` refuses, drop the link.
  pub(crate) async fn ask<A>(
    &mut self,
    payload: &[u8],
    read: fn(&[u8]) -> Result<A, mmo_protocol::Error>,
  ) -> Result<A, Error> {
    let mut stream = match self.stream.take() {
      Some(stream) => stream,
      None => self.dial().await?,
    };
    let answer = read(&stream.exchange(payload).await?)?;
    self.stream = Some(stream);
    Ok(answer)
  }

  /// Opens a link and its stream to the peer. Each side writes its hello first.
  async fn dial(&self) -> Result<Stream, Error> {
    let (address, peer) = (&self.address, self.peer);
    let host = address.rsplit_once(':').map_or(address.as_str(), |(host, _)| host);
    let found = lookup_host(address)
      .await
      .map_err(|source| Error::Resolve { peer, address: address.clone(), source })?
      .find(|found| found.is_ipv4() == self.ipv4)
      .ok_or_else(|| Error::Unresolved { peer, address: address.clone() })?;
    let link = self
      .endpoint
      .connect_with(self.client.clone(), found, host.trim_matches(['[', ']']))
      .map_err(|source| Error::Dial { peer, source })?;
    // A failed handshake and a failed stream end the dial alike.
    let opened = match link.await {
      Ok(connection) => connection.open_bi().await.map(|(send, recv)| (connection, send, recv)),
      Err(fault) => Err(fault),
    };
    let (connection, send, recv) = opened.map_err(|source| Error::Link { peer, source })?;
    let mut stream =
      Stream { peer, _connection: connection, send, recv, frames: Frames::default() };
    Hello::decode(&stream.exchange(&Hello { build: BUILD }.encode()).await?)?;
    Ok(stream)
  }
}

struct Stream {
  peer: &'static str,
  /// The link of the stream, which closes when the stream drops.
  _connection: Connection,
  send: SendStream,
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// Writes `payload` in a frame, and gives the payload of the next frame of the peer.
  #[expect(clippy::expect_used, reason = "a message of the writer holds less than a frame")]
  async fn exchange(&mut self, payload: &[u8]) -> Result<Vec<u8>, Error> {
    let peer = self.peer;
    let framed = frame(payload).expect("a message of the writer holds less than a frame");
    self.send.write_all(&framed).await.map_err(|source| Error::Write { peer, source })?;
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(payload);
      }
      let size = self
        .recv
        .read(&mut chunk)
        .await
        .map_err(|source| Error::Read { peer, source })?
        .ok_or(Error::Ended { peer })?;
      self.frames.push(&chunk[..size]);
    }
  }
}
