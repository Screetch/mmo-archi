use bitcode::{DecodeOwned, Encode};

use crate::{Link, PROTOCOL};

/// The most bytes that a frame holds, apart from its length.
pub const FRAME: usize = 64 * 1024;

/// The most bytes that a datagram holds. QUIC needs a path of 1200 bytes at least, so a datagram
/// of this size and its headers always fit.
pub const DATAGRAM: usize = 1100;

/// A refusal of the codec.
///
/// After a refusal on a stream, the reader closes the stream, since it cannot find where the next
/// frame starts.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The hello of the peer carries another version of the protocol.
  #[error("the peer speaks version {theirs} of the protocol, and this side speaks version {ours}")]
  Version {
    /// The version of the peer.
    theirs: u16,
    /// The version of this side, [`PROTOCOL`].
    ours: u16,
  },
  /// A hello of this version does not hold 6 bytes.
  #[error("a hello holds 6 bytes, and this one holds {size}")]
  Hello {
    /// The bytes that the hello holds.
    size: usize,
  },
  /// A frame holds more than [`FRAME`] bytes.
  #[error("a frame holds {max} bytes at most", max = FRAME)]
  Frame,
  /// A message for a datagram takes more than [`DATAGRAM`] bytes.
  #[error("a datagram holds {max} bytes at most, and this one holds {size}", max = DATAGRAM)]
  Datagram {
    /// The bytes of the message.
    size: usize,
  },
  /// A datagram between a gateway and a layer writer holds no link, which takes 8 bytes.
  #[error("a relayed datagram starts with a link of 8 bytes, and this one holds {size} bytes")]
  Relayed {
    /// The bytes of the datagram.
    size: usize,
  },
  /// The bytes do not hold exactly one message of the type that the reader expects.
  #[error("the bytes are not a message of this version of the protocol")]
  Malformed(#[source] bitcode::Error),
}

/// The first message of each side of a link, before any other.
///
/// Its layout is the same in each version of the protocol, so each side reads the version of the
/// other: the version as a `u16`, then the build as a `u32`, both little-endian, with no bitcode.
/// A side that reads a hello of another version answers with its own hello, which carries the
/// version that it needs, and closes the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
  /// The build of the program that sends the hello, for the logs of the operator.
  pub build: u32,
}

impl Hello {
  /// The bytes of the hello, with the version [`PROTOCOL`].
  pub fn encode(self) -> [u8; 6] {
    let mut bytes = [0; 6];
    let (version, build) = bytes.split_at_mut(2);
    version.copy_from_slice(&PROTOCOL.to_le_bytes());
    build.copy_from_slice(&self.build.to_le_bytes());
    bytes
  }

  /// Reads a hello. It refuses a hello of another version before it reads the rest, since a hello
  /// of another version can hold more bytes.
  pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
    let Some((&version, rest)) = bytes.split_first_chunk::<2>() else {
      return Err(Error::Hello { size: bytes.len() });
    };
    let theirs = u16::from_le_bytes(version);
    if theirs != PROTOCOL {
      return Err(Error::Version { theirs, ours: PROTOCOL });
    }
    match rest.first_chunk::<4>() {
      Some(&build) if rest.len() == 4 => Ok(Self { build: u32::from_le_bytes(build) }),
      _ => Err(Error::Hello { size: bytes.len() }),
    }
  }
}

/// The bytes of a message, with bitcode, for a [`frame`] or a datagram.
pub fn encode<M: Encode>(message: &M) -> Vec<u8> {
  bitcode::encode(message)
}

/// Reads a message of type `M`, and refuses bytes that do not hold exactly one.
pub fn decode<M: DecodeOwned>(bytes: &[u8]) -> Result<M, Error> {
  bitcode::decode(bytes).map_err(Error::Malformed)
}

/// The bytes of a message for a datagram, which holds one message and no length. It refuses a
/// message of more than [`DATAGRAM`] bytes.
pub fn datagram<M: Encode>(message: &M) -> Result<Vec<u8>, Error> {
  let bytes = encode(message);
  if bytes.len() > DATAGRAM {
    return Err(Error::Datagram { size: bytes.len() });
  }
  Ok(bytes)
}

/// The bytes of a datagram between a gateway and a layer writer: the link of the client, as a
/// `u64` in little-endian, then `datagram`, the bytes of a datagram of that client as they go on
/// its link. So a gateway relays the intents of a client, and each snapshot for it, with no
/// decode and no encode.
pub fn relayed(link: Link, datagram: &[u8]) -> Vec<u8> {
  [link.0.to_le_bytes().as_slice(), datagram].concat()
}

/// The link and the datagram of the client in the bytes that [`relayed`] made.
pub fn unrelayed(bytes: &[u8]) -> Result<(Link, &[u8]), Error> {
  let (link, datagram) = bytes.split_first_chunk().ok_or(Error::Relayed { size: bytes.len() })?;
  Ok((Link(u64::from_le_bytes(*link)), datagram))
}

/// Puts a payload in a frame for a stream: its length as an unsigned LEB128 number, then its
/// bytes. It refuses a payload of more than [`FRAME`] bytes.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, Error> {
  if payload.len() > FRAME {
    return Err(Error::Frame);
  }
  let mut bytes = Vec::with_capacity(payload.len() + 3);
  let mut rest = payload.len();
  while rest > 0x7f {
    let [low, ..] = rest.to_le_bytes();
    bytes.push(low | 0x80);
    rest >>= 7;
  }
  let [last, ..] = rest.to_le_bytes();
  bytes.push(last);
  bytes.extend_from_slice(payload);
  Ok(bytes)
}

/// The reader of the frames of a stream: it takes the bytes as they come, and gives each payload
/// when its frame is whole.
#[derive(Debug, Default)]
pub struct Frames {
  held: Vec<u8>,
}

impl Frames {
  /// Adds the bytes that the stream gave.
  pub fn push(&mut self, bytes: &[u8]) {
    self.held.extend_from_slice(bytes);
  }

  /// The payload of the next frame, or `None` while that frame is not whole. It refuses a length
  /// of more than [`FRAME`] as soon as it reads it, before the payload comes.
  pub fn pop(&mut self) -> Result<Option<Vec<u8>>, Error> {
    let mut size = 0;
    for (at, &byte) in self.held.iter().enumerate() {
      size |= usize::from(byte & 0x7f) << (7 * at);
      let more = byte & 0x80 != 0;
      if size > FRAME || more && at == 2 {
        return Err(Error::Frame);
      }
      if !more {
        let end = at + 1 + size;
        if self.held.len() < end {
          return Ok(None);
        }
        return Ok(Some(self.held.drain(..end).skip(at + 1).collect()));
      }
    }
    Ok(None)
  }
}

#[cfg(test)]
mod tests {
  use super::{
    DATAGRAM, Error, FRAME, Frames, Hello, PROTOCOL, datagram, frame, relayed, unrelayed,
  };
  use crate::Link;

  fn version(number: u16, rest: &[u8]) -> Vec<u8> {
    [number.to_le_bytes().as_slice(), rest].concat()
  }

  fn refused<T>(result: Result<T, Error>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
  }

  #[test]
  fn a_hello_holds_the_version_then_the_build_in_little_endian() {
    let hello = Hello { build: 0x0102_0304 };
    let [low, high] = PROTOCOL.to_le_bytes();
    assert_eq!(hello.encode(), [low, high, 4, 3, 2, 1]);
    assert_eq!(refused(Hello::decode(&hello.encode())), Ok(hello));
  }

  #[test]
  fn a_hello_of_another_version_is_refused_with_both_versions() {
    let theirs = format!(
      "the peer speaks version {} of the protocol, and this side speaks version {PROTOCOL}",
      PROTOCOL + 1
    );
    let other = |rest: &[u8]| refused(Hello::decode(&version(PROTOCOL + 1, rest)));
    assert_eq!(other(&[4, 3, 2, 1]), Err(theirs.clone()));
    assert_eq!(other(&[9; 7]), Err(theirs));
  }

  #[test]
  fn a_hello_of_this_version_with_another_size_is_refused() {
    let holds = |size: usize| Err(format!("a hello holds 6 bytes, and this one holds {size}"));
    assert_eq!(refused(Hello::decode(&[])), holds(0));
    assert_eq!(refused(Hello::decode(&version(PROTOCOL, &[])[..1])), holds(1));
    assert_eq!(refused(Hello::decode(&version(PROTOCOL, &[4, 3, 2]))), holds(5));
    assert_eq!(refused(Hello::decode(&version(PROTOCOL, &[4, 3, 2, 1, 0]))), holds(7));
  }

  #[test]
  fn a_frame_starts_with_its_length_in_leb128() {
    assert_eq!(refused(frame(&[])), Ok(vec![0]));
    assert_eq!(refused(frame(&[7; 127])), Ok([vec![127], vec![7; 127]].concat()));
    assert_eq!(refused(frame(&[7; 200])), Ok([vec![0xc8, 0x01], vec![7; 200]].concat()));
    let largest = [vec![0x80, 0x80, 0x04], vec![7; FRAME]].concat();
    assert_eq!(refused(frame(&vec![7; FRAME])), Ok(largest));
    let too_large = Err("a frame holds 65536 bytes at most".to_owned());
    assert_eq!(refused(frame(&vec![7; FRAME + 1])), too_large);
  }

  #[test]
  fn the_reader_gives_each_whole_frame_in_order() {
    let mut frames = Frames::default();
    assert_eq!(refused(frames.pop()), Ok(None));
    frames.push(&[vec![3, 1, 2, 3, 0, 0xc8, 0x01], vec![9; 200]].concat());
    assert_eq!(refused(frames.pop()), Ok(Some(vec![1, 2, 3])));
    assert_eq!(refused(frames.pop()), Ok(Some(vec![])));
    assert_eq!(refused(frames.pop()), Ok(Some(vec![9; 200])));
    assert_eq!(refused(frames.pop()), Ok(None));
  }

  #[test]
  fn the_reader_waits_for_the_last_byte_of_a_frame() {
    let mut frames = Frames::default();
    let bytes = [vec![0xac, 0x02], vec![5; 300]].concat();
    for byte in &bytes[..bytes.len() - 1] {
      frames.push(&[*byte]);
      assert_eq!(refused(frames.pop()), Ok(None));
    }
    frames.push(&bytes[bytes.len() - 1..]);
    assert_eq!(refused(frames.pop()), Ok(Some(vec![5; 300])));
  }

  #[test]
  fn the_reader_refuses_a_length_past_the_largest_frame_before_its_payload() {
    let mut frames = Frames::default();
    frames.push(&[0x80, 0x80, 0x04]);
    assert_eq!(refused(frames.pop()), Ok(None));
    let too_large = Err("a frame holds 65536 bytes at most".to_owned());
    for length in [[0x81, 0x80, 0x04], [0xff, 0xff, 0x7f], [0x80, 0x80, 0x80]] {
      let mut frames = Frames::default();
      frames.push(&length);
      assert_eq!(refused(frames.pop()), too_large);
    }
  }

  #[test]
  fn a_relayed_datagram_holds_the_link_in_little_endian_then_the_datagram_of_the_client() {
    let bytes = relayed(Link(0x0102_0304_0506_0708), &[9, 9]);
    assert_eq!(bytes, [8, 7, 6, 5, 4, 3, 2, 1, 9, 9]);
    assert_eq!(refused(unrelayed(&bytes)), Ok((Link(0x0102_0304_0506_0708), &[9, 9][..])));
    assert_eq!(refused(unrelayed(&bytes[..8])), Ok((Link(0x0102_0304_0506_0708), &[][..])));
    let short = "a relayed datagram starts with a link of 8 bytes, and this one holds 7 bytes";
    assert_eq!(refused(unrelayed(&bytes[..7])), Err(short.to_owned()));
  }

  #[test]
  fn a_datagram_holds_one_message_of_its_largest_size_and_no_more() {
    let largest: Vec<u8> = (0..=u8::MAX).cycle().take(DATAGRAM - 5).collect();
    assert_eq!(refused(datagram(&largest)).map(|bytes| bytes.len()), Ok(DATAGRAM));
    let past = [largest, vec![0]].concat();
    assert_eq!(
      refused(datagram(&past)),
      Err("a datagram holds 1100 bytes at most, and this one holds 1101".to_owned())
    );
  }
}
