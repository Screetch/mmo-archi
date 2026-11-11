//! The connection of the client to a server of the game.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use bitcode::{DecodeOwned, Encode};
use mmo_protocol::{FRAME, Frames, Hello, decode, encode, frame};
use quinn::{ClientConfig, Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::net::lookup_host;

use crate::Fault;

/// The build in the hello of the client, for the logs of the operator.
const BUILD: u32 = 0;

/// The authority that signed the certificates of the servers: the client trusts a server only
/// when the chain of its certificate ends there.
#[derive(Clone)]
pub struct Trust(ClientConfig);

impl Trust {
  /// Trusts the servers whose certificate `authority` signed.
  pub fn new(authority: Vec<CertificateDer<'static>>) -> Result<Self, Fault> {
    let mut roots = RootCertStore::empty();
    for certificate in authority {
      roots.add(certificate)?;
    }
    Ok(Self(ClientConfig::with_root_certificates(Arc::new(roots))?))
  }

  /// Trusts the authority that a file of PEM holds.
  pub fn from_pem(path: &Path) -> Result<Self, Fault> {
    Self::new(CertificateDer::pem_file_iter(path)?.collect::<Result<_, _>>()?)
  }
}

/// The reader of the frames of a stream of a server.
struct Stream {
  recv: RecvStream,
  frames: Frames,
}

impl Stream {
  /// The payload of the next frame of the server.
  async fn next(&mut self) -> Result<Vec<u8>, Fault> {
    loop {
      if let Some(payload) = self.frames.pop()? {
        return Ok(payload);
      }
      let chunk = self.recv.read_chunk(FRAME, true).await?.ok_or(Fault::Ended)?;
      self.frames.push(&chunk.bytes);
    }
  }
}

/// A connection to a server, after the hello of each side, with the stream that the client
/// opened.
pub(crate) struct Link {
  _endpoint: Endpoint,
  pub(crate) connection: Connection,
  send: SendStream,
  stream: Stream,
}

impl Link {
  /// Writes a message on the stream.
  pub(crate) async fn write<M: Encode>(&mut self, message: &M) -> Result<(), Fault> {
    self.write_payload(&encode(message)).await
  }

  /// Reads the next message of the server on the stream. It is safe to drop the future: the
  /// bytes that came stay for the next call.
  pub(crate) async fn read<M: DecodeOwned>(&mut self) -> Result<M, Fault> {
    self.read_payload().await.and_then(|payload| decode(&payload).map_err(Fault::from))
  }

  async fn write_payload(&mut self, payload: &[u8]) -> Result<(), Fault> {
    let bytes = frame(payload)?;
    Ok(self.send.write_all(&bytes).await?)
  }

  async fn read_payload(&mut self) -> Result<Vec<u8>, Fault> {
    self.stream.next().await
  }
}

/// Connects to the server at `address`, as `host:port`, with the host as the name that its
/// certificate must hold, and swaps the hello of each side. The client writes its hello first,
/// since a server sees a stream only after a first byte. The hello of the server must speak this
/// version of the protocol. When the name gives more than one address, the client takes the first
/// address of IPv4, or else the first address.
pub(crate) async fn open(trust: &Trust, address: &str) -> Result<Link, Fault> {
  let resolve = |source| Fault::Resolve { address: address.to_owned(), source };
  let found = lookup_host(address).await.map_err(resolve)?;
  let to = found.min_by_key(SocketAddr::is_ipv6).ok_or(Fault::Unresolved(address.to_owned()));
  let any = [IpAddr::V4(Ipv4Addr::UNSPECIFIED), IpAddr::V6(Ipv6Addr::UNSPECIFIED)];
  let ip = any[usize::from(to.as_ref().is_ok_and(SocketAddr::is_ipv6))];
  let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
  dial(Endpoint::client(SocketAddr::new(ip, 0)), trust, to, host).await
}

/// Connects the endpoint that `socket` gives to `to`, as the server named `host`.
async fn dial(
  socket: io::Result<Endpoint>,
  trust: &Trust,
  to: Result<SocketAddr, Fault>,
  host: &str,
) -> Result<Link, Fault> {
  let connecting = socket.map_err(Fault::Socket).and_then(|mut endpoint| {
    endpoint.set_default_client_config(trust.0.clone());
    to.and_then(|to| {
      let name = host.trim_matches(['[', ']']);
      endpoint.connect(to, name).map(|connecting| (endpoint, connecting)).map_err(Fault::from)
    })
  });
  let (endpoint, connecting) = connecting?;
  let connection = connecting.await?;
  let greeted = greet(connection.open_bi().await).await;
  greeted.map(|(send, stream)| Link { _endpoint: endpoint, connection, send, stream })
}

/// Writes the hello of the client on the stream that `opened` gives, and reads the hello of the
/// server. It reads even when the write fails, since a link that fails shows the fault of the
/// read too, and `Fault` gives the first.
async fn greet(
  opened: Result<(SendStream, RecvStream), ConnectionError>,
) -> Result<(SendStream, Stream), Fault> {
  let (mut send, recv) = opened?;
  let mut stream = Stream { recv, frames: Frames::default() };
  let written = send.write_all(&framed(&Hello { build: BUILD }.encode())).await;
  written
    .map_err(Fault::from)
    .and(stream.next().await)
    .and_then(|payload| Hello::decode(&payload).map_err(Fault::from))
    .map(|_| (send, stream))
}

/// `payload` in its frame, for a payload that a frame holds, as a hello does.
#[expect(clippy::expect_used, reason = "a hello of 6 bytes fits a frame")]
fn framed(payload: &[u8]) -> Vec<u8> {
  frame(payload).expect("the payload fits a frame")
}

#[cfg(test)]
mod tests {
  use std::io;
  use std::path::PathBuf;

  use mmo_protocol::{PROTOCOL, frame};
  use quinn::{ConnectionError, Endpoint};
  use rustls::pki_types::CertificateDer;

  use super::{Trust, dial, greet, open};
  use crate::Fault;
  use crate::testing::{Fake, hold};

  fn fault<T>(result: Result<T, Fault>) -> Result<(), String> {
    result.map(drop).map_err(|fault| fault.to_string())
  }

  /// A folder of the test alone, with `files` in it.
  fn folder(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("mmo-client-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("a folder of the test");
    for (file, text) in files {
      std::fs::write(folder.join(file), text).expect("a file");
    }
    folder
  }

  #[test]
  fn the_trust_reads_an_authority_and_names_what_it_refuses() {
    let broken = "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n";
    let folder = folder("trust", &[("broken.pem", broken)]);
    let read = |file: &str| fault(Trust::from_pem(&folder.join(file)));
    assert!(
      read("nowhere.pem").expect_err("no file").starts_with("the certificate cannot be read: ")
    );
    assert_eq!(
      read("broken.pem"),
      Err("the certificate cannot be read: base64 decode error: InvalidCharacter(33)".to_owned())
    );
    let garbled = fault(Trust::new(vec![CertificateDer::from(vec![1, 2, 3])]));
    assert!(garbled.expect_err("a bad certificate").starts_with("TLS refuses the certificate: "));
    let empty = fault(Trust::new(vec![]));
    assert!(empty.expect_err("no authority").starts_with("TLS cannot use the authority: "));
    std::fs::remove_dir_all(&folder).expect("the folder goes");
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_connects_to_the_server_that_its_authority_signed_and_to_no_other() {
    let fake = Fake::start(hold);
    let folder = folder("open", &[("authority.pem", &fake.pem)]);
    let trust = Trust::from_pem(&folder.join("authority.pem")).expect("a trust");
    assert_eq!(fault(open(&trust, &fake.address).await), Ok(()));
    fake.end().await;
    let stranger = Fake::start(hold);
    let outcome = fault(open(&trust, &stranger.address).await).expect_err("an untrusted server");
    assert!(outcome.starts_with("the link fails: the cryptographic handshake failed"), "{outcome}");
    stranger.end().await;
    let nowhere = fault(open(&trust, "nowhere.invalid:1").await).expect_err("no host");
    assert!(nowhere.starts_with("the client cannot resolve nowhere.invalid:1: "), "{nowhere}");
    let port_0 = fault(open(&trust, "127.0.0.1:0").await).expect_err("no port");
    assert!(port_0.starts_with("the client cannot connect: "), "{port_0}");
    std::fs::remove_dir_all(&folder).expect("the folder goes");
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_hello_of_the_server_is_refused_when_it_is_not_a_hello_of_this_version() {
    let version = [(PROTOCOL + 1).to_le_bytes().as_slice(), &[7, 0, 0, 0]].concat();
    let other = Fake::raw(|mut peer| async move {
      peer.payload().await;
      peer.write_raw(&frame(&version).expect("a frame")).await;
      peer
    });
    let theirs = format!(
      "the peer speaks version {} of the protocol, and this side speaks version {PROTOCOL}",
      PROTOCOL + 1
    );
    assert_eq!(fault(open(&other.trust, &other.address).await), Err(theirs));
    other.end().await;
    let large = Fake::raw(|mut peer| async move {
      peer.payload().await;
      peer.write_raw(&[0x81, 0x80, 0x04]).await;
      peer
    });
    let too_large = Err("a frame holds 65536 bytes at most".to_owned());
    assert_eq!(fault(open(&large.trust, &large.address).await), too_large);
    large.end().await;
    let finished = Fake::raw(|mut peer| async move {
      peer.payload().await;
      peer.finish();
      peer
    });
    let ended = Err("the server ended its stream".to_owned());
    assert_eq!(fault(open(&finished.trust, &finished.address).await), ended);
    finished.end().await;
    let closing = Fake::raw(|mut peer| async move {
      peer.payload().await;
      peer.connection.close(7_u32.into(), b"bye");
      peer
    });
    let lost = Err("the stream fails: connection lost".to_owned());
    assert_eq!(fault(open(&closing.trust, &closing.address).await), lost);
    closing.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_socket_that_does_not_open_and_a_stream_that_does_not_open_end_the_connect() {
    let fake = Fake::start(hold);
    let socket = Err(io::Error::other("no socket"));
    let to = Ok(([127, 0, 0, 1], 1).into());
    let refused = fault(dial(socket, &fake.trust, to, "localhost").await);
    assert_eq!(refused, Err("the client cannot open a socket: no socket".to_owned()));
    let socket = Endpoint::client(([127, 0, 0, 1], 0).into());
    let none = Err(Fault::Unresolved("nowhere:1".to_owned()));
    let refused = fault(dial(socket, &fake.trust, none, "nowhere").await);
    assert_eq!(
      refused,
      Err("the address nowhere:1 names no host that the client reaches".to_owned())
    );
    let closed = fault(greet(Err(ConnectionError::LocallyClosed)).await);
    assert_eq!(closed, Err(format!("the link fails: {}", ConnectionError::LocallyClosed)));
    assert_eq!(fault(open(&fake.trust, &fake.address).await), Ok(()));
    fake.end().await;
  }
}
