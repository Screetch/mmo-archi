//! The messages between a client and the servers of mmo-archi, their codec and their frames.
//!
//! Each link is a connection of QUIC. Each side first sends a [`Hello`] in a [`frame`], whose
//! layout no version of the protocol changes. Then each message goes on the wire as [`encode`]
//! makes it: on a stream in a [`frame`], which [`Frames`] reads, and alone in a datagram, as
//! [`datagram`] makes it.
//!
//! - A client and the login service: [`ToLogin`] and [`FromLogin`], on a stream.
//! - A client and a gateway: [`Attach`], [`FromClient`] and [`ToClient`] on stream 0, and
//!   [`Intents`] and [`Snapshot`] in datagrams.
//! - A gateway and a zone writer: [`ToZone`], and a [`Relayed`] for each message that the gateway
//!   relays between one link and the writer.
//! - Two zone writers: [`ToNeighbour`], on a stream.
//!
//! The state of the world goes on the wire in integers: [`Position`], [`Offset`], [`Velocity`]
//! and [`Angle`].

mod codec;
mod messages;
mod values;

pub use codec::{DATAGRAM, Error, FRAME, Frames, Hello, datagram, decode, encode, frame};
pub use messages::{
  Attach, Change, Character, Entity, FromClient, FromLogin, Input, Intents, Refusal, Relayed,
  Snapshot, ToClient, ToLogin, ToNeighbour, ToZone,
};
pub use values::{
  AccountId, Angle, CharacterId, Epoch, Link, Offset, Password, Position, Tick, Token, Velocity,
  ZoneId,
};

/// The version of the protocol, which each [`Hello`] carries.
///
/// A change of a message, of the codec, of the version of bitcode or of the step of `mmo-collide`
/// raises it. A new version needs a restart of the world, since a side accepts only its own.
pub const PROTOCOL: u16 = 2;
