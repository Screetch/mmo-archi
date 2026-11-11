//! What the links of the gateway share: the hello, the reader of the frames of a stream, and the
//! faults that end a link.

use mmo_protocol::{Frames, Hello, ZoneId, frame};
use quinn::{
  ConnectError, ConnectionError, ReadError, RecvStream, SendDatagramError, SendStream, VarInt,
  WriteError,
};

/// The code with which the gateway closes a link after a fault. The reason of the close holds the
/// text of the fault.
pub(crate) const FAULT: VarInt = VarInt::from_u32(1);

/// The build in the hello of the gateway, for the logs of the operator.
const BUILD: u32 = 0;

/// Why the gateway ends the connection of a client or its link to a zone writer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Fault {
  #[error("the link fails: {0}")]
  Link(#[from] ConnectionError),
  #[error("the stream fails: {0}")]
  Read(#[from] ReadError),
  #[error("the gateway cannot write to the stream: {0}")]
  Write(#[from] WriteError),
  #[error("the gateway cannot send a datagram: {0}")]
  Datagram(#[from] SendDatagramError),
  #[error("the peer ended its stream")]
  Ended,
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
  #[error("Redis fails: {0}")]
  Redis(#[from] redis::RedisError),
  #[error("no session has this token")]
  NoSession,
  #[error("the directory names no writer of zone {}", .0.0)]
  NoWriter(ZoneId),
  #[error("the address {0} of a zone writer names no host that the gateway reaches")]
  Unresolved(String),
  #[error("the gateway cannot resolve the address of a zone writer: {0}")]
  Resolve(#[source] std::io::Error),
  #[error("the gateway cannot link to a zone writer: {0}")]
  Connect(#[from] ConnectError),
  #[error("the link to the writer of zone {} ends: {}", .zone.0, .why)]
  Lost { zone: ZoneId, why: ConnectionError },
}

/// Writes the hello of the gateway on the stream that `opened` gives, then reads the hello of the
/// peer, which must speak this version of the protocol. A client and a zone writer each greet so.
pub(crate) async fn greet(
  opened: Result<(SendStream, RecvStream), ConnectionError>,
) -> Result<(SendStream, Stream), Fault> {
  let (mut send, recv) = opened?;
  let mut stream = Stream { recv, frames: Frames::default() };
  send.write_all(&framed(&Hello { build: BUILD }.encode())).await?;
  Hello::decode(&stream.next().await?)?;
  Ok((send, stream))
}

/// A stream of a peer, and the frames that it gave.
pub(crate) struct Stream {
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// The payload of the next frame of the peer.
  pub(crate) async fn next(&mut self) -> Result<Vec<u8>, Fault> {
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(payload);
      }
      let size = self.recv.read(&mut chunk).await?.ok_or(Fault::Ended)?;
      self.frames.push(&chunk[..size]);
    }
  }
}

/// `payload` in its frame, for a payload that a frame holds: a hello, a message of a few integers,
/// or a message that the gateway took out of a frame of a zone writer.
#[expect(clippy::expect_used, reason = "each caller gives a payload that a frame holds")]
pub(crate) fn framed(payload: &[u8]) -> Vec<u8> {
  frame(payload).expect("the payload fits a frame")
}
