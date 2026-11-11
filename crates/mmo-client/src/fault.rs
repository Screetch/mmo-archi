use std::io;

use mmo_protocol::Refusal;
use quinn::{ConnectError, ConnectionError, ReadError, SendDatagramError, WriteError};
use rustls::client::VerifierBuilderError;

/// Why the client cannot reach the world, or loses it.
#[derive(Debug, thiserror::Error)]
pub enum Fault {
  /// The file of PEM that names the authority of the servers cannot be read.
  #[error("the certificate cannot be read: {0}")]
  Pem(#[from] rustls::pki_types::pem::Error),
  /// TLS refuses a certificate of the authority.
  #[error("TLS refuses the certificate: {0}")]
  Tls(#[from] rustls::Error),
  /// TLS cannot check the certificates of the authority.
  #[error("TLS cannot use the authority: {0}")]
  Authority(#[from] VerifierBuilderError),
  /// The name of a server does not resolve.
  #[error("the client cannot resolve {address}: {source}")]
  Resolve {
    /// The address, as `host:port`.
    address: String,
    /// Why the name does not resolve.
    source: io::Error,
  },
  /// The name of a server resolves to no address.
  #[error("the address {0} names no host that the client reaches")]
  Unresolved(String),
  /// The socket of the client cannot open.
  #[error("the client cannot open a socket: {0}")]
  Socket(#[source] io::Error),
  /// The connection cannot start.
  #[error("the client cannot connect: {0}")]
  Connect(#[from] ConnectError),
  /// The connection ends or fails.
  #[error("the link fails: {0}")]
  Link(#[from] ConnectionError),
  /// The stream cannot be read.
  #[error("the stream fails: {0}")]
  Read(#[from] ReadError),
  /// The stream cannot be written.
  #[error("the client cannot write to the stream: {0}")]
  Write(#[from] WriteError),
  /// A datagram cannot be sent.
  #[error("the client cannot send a datagram: {0}")]
  Datagram(#[from] SendDatagramError),
  /// The server sends bytes that are no frame or no message of this version of the protocol.
  #[error(transparent)]
  Codec(#[from] mmo_protocol::Error),
  /// The server ended its stream.
  #[error("the server ended its stream")]
  Ended,
  /// The login service refuses the client.
  #[error("the login service refuses: {0:?}")]
  Refused(Refusal),
  /// The account holds no character with this name, and a new one did not give it.
  #[error("the account has no character named {0}")]
  NoCharacter(String),
  /// The game closed the link.
  #[error("the game closed the link")]
  Closed,
}
