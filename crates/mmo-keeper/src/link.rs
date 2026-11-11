use std::time::Duration;

use mmo_protocol::{Frames, FromKeeper, Hello, KeeperRefusal, ToKeeper, decode, encode, frame};
use mmo_store::Store;
use quinn::{
  ConnectionError, Endpoint, Incoming, ReadError, RecvStream, SendStream, VarInt, WriteError,
};
use tokio_util::task::TaskTracker;

use crate::Error;

/// The build in the hello of the keeper: the version of the workspace, as 1002003 for 1.2.3.
pub(crate) const BUILD: u32 = {
  let parts = [
    env!("CARGO_PKG_VERSION_MAJOR"),
    env!("CARGO_PKG_VERSION_MINOR"),
    env!("CARGO_PKG_VERSION_PATCH"),
  ];
  let mut build = 0;
  let mut at = 0;
  while at < parts.len() {
    let Ok(part) = u32::from_str_radix(parts[at], 10) else {
      panic!("cargo gives each part of the version as a number");
    };
    build = build * 1000 + part;
    at += 1;
  }
  build
};

/// The code with which the keeper closes the link of a zone writer after a fault. The reason of the
/// close holds the text of the fault.
const FAULT: VarInt = VarInt::from_u32(1);

/// The most time that the keeper waits, after a fault, for the writer to read its last bytes and
/// close the link: the idle timeout of QUIC.
const GRACE: Duration = Duration::from_secs(3);

/// Why the keeper ends the link of a zone writer.
#[derive(Debug, thiserror::Error)]
enum Fault {
  #[error("the link fails: {0}")]
  Link(#[from] ConnectionError),
  #[error("the stream of the writer fails: {0}")]
  Read(#[from] ReadError),
  #[error("the keeper cannot write to the writer: {0}")]
  Write(#[from] WriteError),
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
}

/// Takes the links of the zone writers, each in a task of its own, until the endpoint closes. Then
/// it waits for the end of each link.
pub(crate) async fn serve(endpoint: Endpoint, store: Store) -> Error {
  let links = TaskTracker::new();
  while let Some(incoming) = endpoint.accept().await {
    links.spawn(link(incoming, store.clone()));
  }
  links.close();
  links.wait().await;
  Error::Closed
}

async fn link(incoming: Incoming, store: Store) {
  let connection = match incoming.await {
    Ok(connection) => connection,
    Err(error) => return ended(&Fault::Link(error)),
  };
  let fault = match connection.accept_bi().await {
    Ok((send, recv)) => {
      let mut stream = Stream { send, recv, frames: Frames::default() };
      match talk(&mut stream, &store).await {
        Ok(()) => return,
        Err(fault) => {
          // A close drops each byte that the writer did not read yet, as the hello that names the
          // version of the keeper. So the keeper ends its stream and lets the writer close the
          // link first, for GRACE at most. Each result of the two ends the wait alike.
          stream.send.finish().ok();
          tokio::time::timeout(GRACE, connection.closed()).await.ok();
          fault
        }
      }
    }
    Err(error) => Fault::Link(error),
  };
  connection.close(FAULT, fault.to_string().as_bytes());
  ended(&fault);
}

fn ended(fault: &Fault) {
  eprintln!("a link of a zone writer ends: {fault}");
}

/// Answers the zone writer on the stream that it opened. Each side writes its hello first, then
/// the keeper answers each message of the writer in order, until the writer ends the stream.
async fn talk(stream: &mut Stream, store: &Store) -> Result<(), Fault> {
  let hello = stream.exchange(&framed(&Hello { build: BUILD }.encode())).await?;
  Hello::decode(&hello.unwrap_or_default())?;
  let mut answer = Vec::new();
  while let Some(payload) = stream.exchange(&answer).await? {
    answer = framed(&encode(&answered(store, decode(&payload)?).await));
  }
  Ok(())
}

struct Stream {
  send: SendStream,
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// Writes `bytes`, then gives the payload of the next frame of the writer, or `None` when the
  /// writer ended the stream.
  async fn exchange(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, Fault> {
    self.send.write_all(bytes).await?;
    let mut chunk = [0; 4096];
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(Some(payload));
      }
      match self.recv.read(&mut chunk).await? {
        Some(size) => self.frames.push(&chunk[..size]),
        None => return Ok(None),
      }
    }
  }
}

#[expect(clippy::expect_used, reason = "a hello or an answer holds a few bytes")]
fn framed(payload: &[u8]) -> Vec<u8> {
  frame(payload).expect("a hello or an answer of the keeper holds less than a frame")
}

async fn answered(store: &Store, message: ToKeeper) -> FromKeeper {
  let done = match message {
    ToKeeper::Claim { zone } => store.claim(zone).await.map(|epoch| FromKeeper::Claimed { epoch }),
    ToKeeper::Save(batch) => store.save(&batch).await.map(|written| FromKeeper::Saved { written }),
  };
  done.unwrap_or_else(|error| FromKeeper::Refused(refusal(error)))
}

fn refusal(error: mmo_store::Error) -> KeeperRefusal {
  match error {
    mmo_store::Error::NoZone { .. } => KeeperRefusal::NoZone,
    mmo_store::Error::Epoch { held, .. } => KeeperRefusal::Stale { held },
    error => {
      eprintln!("the keeper cannot write in PostgreSQL: {error}");
      KeeperRefusal::Unavailable
    }
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{Batch, Epoch, FromKeeper, Hello, KeeperRefusal, ToKeeper, ZoneId, frame};
  use mmo_store::Store;
  use quinn::{ConnectionError, Endpoint, TransportErrorCode};
  use sqlx::PgPool;

  use super::{BUILD, answered, serve};
  use crate::config::Config;
  use crate::peer::{World, Writer, var};

  #[sqlx::test(migrations = false)]
  async fn each_fault_of_a_writer_ends_its_link_alone_with_its_reason(
    pool: PgPool,
  ) -> Result<(), mmo_store::Error> {
    let store = Store::migrate(pool).await?;
    let world = World::new();
    let vars = world.vars("127.0.0.1:0", "postgres://keeper@localhost/game");
    let config = Config::read(var(&vars)).expect("a config");
    let endpoint = Endpoint::server(config.server, config.address).expect("an endpoint");
    let address = endpoint.local_addr().expect("an address");
    let keeper = tokio::spawn(serve(endpoint.clone(), store));
    let writers = world.writer(true);
    let idle = Writer::link(&writers, address).await;
    let hello = |version: u8| frame(&[version, 0, 0, 0, 0, 0]).expect("a hello");

    let mut other = Writer::link(&writers, address).await;
    other.write(&hello(1)).await;
    let theirs = other.read().await;
    let mut large = Writer::link(&writers, address).await;
    large.write(&[0x81, 0x80, 0x04]).await;
    let mut wrong = Writer::link(&writers, address).await;
    wrong.greet().await;
    wrong.write(&frame(&[]).expect("a frame")).await;
    let mut deaf = Writer::link(&writers, address).await;
    deaf.recv.stop(7u32.into()).expect("an open stream");
    deaf.write(&hello(2)).await;
    let mut gone = Writer::link(&writers, address).await;
    gone.greet().await;
    let refused = gone.ask(&ToKeeper::Claim { zone: ZoneId(9) }).await;
    gone.send.reset(7u32.into()).expect("an open stream");
    let mut reasons = Vec::new();
    for writer in [other, large, wrong, deaf, gone] {
      reasons.push(writer.closed().await);
    }
    let reason = |text: &str| Ok((1, text.to_owned()));
    assert_eq!(theirs, Hello { build: BUILD }.encode());
    assert_eq!(refused, FromKeeper::Refused(KeeperRefusal::NoZone));
    assert_eq!(
      reasons,
      [
        reason("the peer speaks version 1 of the protocol, and this side speaks version 2"),
        reason("a frame holds 65536 bytes at most"),
        reason("the bytes are not a message of this version of the protocol"),
        reason("the keeper cannot write to the writer: sending stopped by peer: error 7"),
        reason("the stream of the writer fails: stream reset by peer: error 7"),
      ]
    );

    let stranger = Writer::link(&world.writer(false), address).await.closed().await;
    assert!(
      matches!(
        stranger,
        Err(ConnectionError::ConnectionClosed(ref close))
          if close.error_code == TransportErrorCode::crypto(116)
      ),
      "{stranger:?}"
    );
    endpoint.close(0u32.into(), b"");
    assert_eq!(keeper.await.expect("serve ends").to_string(), "the endpoint of the links closed");
    drop(idle);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn the_keeper_answers_that_it_cannot_write_when_postgresql_fails(
    pool: PgPool,
  ) -> Result<(), mmo_store::Error> {
    let store = Store::migrate(pool.clone()).await?;
    pool.close().await;
    let batch = Batch { zone: ZoneId(1), epoch: Epoch(1), saves: vec![] };
    let claim = answered(&store, ToKeeper::Claim { zone: ZoneId(1) }).await;
    let save = answered(&store, ToKeeper::Save(batch)).await;
    assert_eq!([claim, save], [FromKeeper::Refused(KeeperRefusal::Unavailable); 2]);
    Ok(())
  }
}
