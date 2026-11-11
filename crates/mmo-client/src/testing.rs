//! A server of the game that a test scripts, to prove each step of the client.

use std::future::Future;
use std::pin::Pin;

use bitcode::{DecodeOwned, Encode};
use mmo_protocol::{FRAME, Frames, Hello, decode, encode, frame};
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::task::JoinHandle;

use crate::Trust;

/// A script, boxed so that the server is one function for every script.
type Script = Box<dyn FnOnce(Peer) -> Pin<Box<dyn Future<Output = Peer> + Send>> + Send>;

/// The side of the server of one connection that a script plays.
pub(crate) struct Peer {
  pub(crate) connection: Connection,
  send: SendStream,
  recv: RecvStream,
  frames: Frames,
}

impl Peer {
  /// The payload of the next frame of the client.
  pub(crate) async fn payload(&mut self) -> Vec<u8> {
    loop {
      if let Some(payload) = self.frames.pop().expect("a frame") {
        return payload;
      }
      let chunk = self.recv.read_chunk(FRAME, true).await.expect("the stream reads");
      self.frames.push(&chunk.expect("the client keeps its stream").bytes);
    }
  }

  /// The next message of the client on its stream.
  pub(crate) async fn read<M: DecodeOwned>(&mut self) -> M {
    decode(&self.payload().await).expect("a message of this version")
  }

  /// Sends a message on the stream.
  pub(crate) async fn write<M: Encode>(&mut self, message: &M) {
    self.write_raw(&frame(&encode(message)).expect("a frame")).await;
  }

  /// Sends bytes on the stream as they are.
  pub(crate) async fn write_raw(&mut self, bytes: &[u8]) {
    self.send.write_all(bytes).await.expect("the stream takes the bytes");
  }

  /// Ends the stream of the server.
  pub(crate) fn finish(&mut self) {
    self.send.finish().expect("the stream is open");
  }
}

/// A server on a local address, with a certificate that [`Fake::trust`] trusts. It serves one
/// connection with its script, and holds the connection until the client closes it.
pub(crate) struct Fake {
  /// The address of the server, as `host:port` with a host that its certificate holds.
  pub(crate) address: String,
  /// The trust that holds the certificate of the server.
  pub(crate) trust: Trust,
  /// The certificate of the server, as a file of PEM holds it.
  pub(crate) pem: String,
  task: JoinHandle<()>,
  _endpoint: Endpoint,
}

impl Fake {
  /// A server whose hello is done when its script starts.
  pub(crate) fn start<S, F>(script: S) -> Self
  where
    S: FnOnce(Peer) -> F + Send + 'static,
    F: Future<Output = Peer> + Send + 'static,
  {
    Self::serve(true, Box::new(move |peer| Box::pin(script(peer))))
  }

  /// A server whose script starts at the stream of the client, with the hello of each side
  /// still to do.
  pub(crate) fn raw<S, F>(script: S) -> Self
  where
    S: FnOnce(Peer) -> F + Send + 'static,
    F: Future<Output = Peer> + Send + 'static,
  {
    Self::serve(false, Box::new(move |peer| Box::pin(script(peer))))
  }

  fn serve(greeting: bool, script: Script) -> Self {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("a key");
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()));
    let tls = ServerConfig::with_single_cert(vec![made.cert.der().clone()], key).expect("a server");
    let endpoint = Endpoint::server(tls, ([127, 0, 0, 1], 0).into()).expect("an endpoint");
    let port = endpoint.local_addr().expect("a bound address").port();
    let serving = endpoint.clone();
    let task = tokio::spawn(async move {
      let Ok(connection) = serving.accept().await.expect("a client").await else {
        return;
      };
      let (send, recv) = connection.accept_bi().await.expect("a stream");
      let mut peer = Peer { connection, send, recv, frames: Frames::default() };
      if greeting {
        Hello::decode(&peer.payload().await).expect("a hello of this version");
        let hello = Hello { build: 7 }.encode();
        peer.write_raw(&frame(&hello).expect("a frame")).await;
      }
      let peer = script(peer).await;
      peer.connection.closed().await;
    });
    Self {
      address: format!("localhost:{port}"),
      trust: Trust::new(vec![made.cert.der().clone()]).expect("a trust"),
      pem: made.cert.pem(),
      task,
      _endpoint: endpoint,
    }
  }

  /// Waits until the script and the connection end, so a failed assertion of the script fails the
  /// test. The client must close the connection first.
  pub(crate) async fn end(self) {
    self.task.await.expect("the script ends with no failed assertion");
  }
}

/// The script of a server that says nothing after its hello.
pub(crate) async fn hold(peer: Peer) -> Peer {
  peer
}
