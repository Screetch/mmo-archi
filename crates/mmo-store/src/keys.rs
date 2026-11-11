//! The name and the format of each key of Redis, which the processes of the game share.
//!
//! Redis holds the short shared state, which the game can lose and make again. Each key is made
//! here alone, so a writer and a reader of a key never disagree on its name. Section 3 of
//! `docs/architecture.md` gives the type, the writer and the life of each key.
//!
//! A Lua script that Redis runs makes the key of an account from its prefix, as
//! [`QUEUED`] and the id of the account, so each prefix that a script needs is public.

use std::net::{IpAddr, Ipv6Addr};

use mmo_protocol::{AccountId, Token, ZoneId};

/// The channel on which a zone writer names each zone that changed its writer.
pub const DIRECTORY: &str = "directory";

/// The fields of [`zone`], which a zone writer writes and a gateway reads: the address that the
/// gateways dial, the epoch of the writer, and the count of its players.
pub const ADDRESS: &str = "address";
pub const EPOCH: &str = "epoch";
pub const PLAYERS: &str = "players";

/// The fields of [`presence`], besides [`EPOCH`]: the character and the zone of the account.
pub const CHARACTER: &str = "character";
pub const ZONE: &str = "zone";

/// The prefix of [`presence`].
pub const PRESENCE: &str = "presence:";

/// The prefix of [`queued`].
pub const QUEUED: &str = "queued:";

/// The prefix of [`admit`].
pub const ADMIT: &str = "admit:";

/// The session of `token`, a hash of the account, the character and the zone.
pub fn session(token: &Token) -> String {
  format!("session:{}", hex(token))
}

/// The presence of `account`, a hash of the character, the zone and the epoch of its writer.
pub fn presence(account: AccountId) -> String {
  format!("{PRESENCE}{}", account.0)
}

/// The entry of `zone` in the directory of zones, a hash of the address, the epoch and the count
/// of players of its writer.
pub fn zone(zone: ZoneId) -> String {
  format!("zone:{}", zone.0)
}

/// The queue of `world`, a sorted set of the queued accounts by the time in milliseconds when each
/// entered.
pub fn queue(world: u32) -> String {
  format!("world:{world}:queue")
}

/// The admitted accounts of `world`, a sorted set by the time in milliseconds when the slot of
/// each ends.
pub fn admitted(world: u32) -> String {
  format!("world:{world}:admitted")
}

/// The admissions of `world`, a sorted set of the accounts by the time in milliseconds when each
/// was admitted, from which a queued client gets an estimate of its wait.
pub fn admissions(world: u32) -> String {
  format!("world:{world}:admissions")
}

/// The mark that `account` still waits in a queue.
pub fn queued(account: AccountId) -> String {
  format!("{QUEUED}{}", account.0)
}

/// The mark that `account` has a slot in its world.
pub fn admit(account: AccountId) -> String {
  format!("{ADMIT}{}", account.0)
}

/// The account of a queued client that can come back with `token`.
pub fn resume(token: &Token) -> String {
  format!("resume:{}", hex(token))
}

/// The count of the failed logins of the last minute for the name `name`, in lower case.
pub fn failed(name: &str) -> String {
  format!("failed:{}", name.to_lowercase())
}

/// The count of the new accounts of the last hour from `address`. An address of IPv6 counts by its
/// network of /64, since one host commonly holds the whole network, and an address of IPv4 in
/// IPv6 counts as its address of IPv4.
pub fn created(address: IpAddr) -> String {
  match address.to_canonical() {
    IpAddr::V4(address) => format!("created:{address}"),
    IpAddr::V6(address) => {
      format!("created:{}/64", Ipv6Addr::from(u128::from(address) >> 64 << 64))
    }
  }
}

/// The lines of the chat `channel`, a stream, and the channel of Redis on which each line goes.
pub fn chat(channel: &str) -> String {
  format!("chat:{channel}")
}

fn hex(token: &Token) -> String {
  const DIGITS: &[u8; 16] = b"0123456789abcdef";
  let digits = |byte: &u8| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 0xf)]];
  token.0.iter().flat_map(digits).map(char::from).collect()
}

#[cfg(test)]
mod tests {
  use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

  use mmo_protocol::{AccountId, Token, ZoneId};

  use super::{
    DIRECTORY, admissions, admit, admitted, chat, created, failed, presence, queue, queued, resume,
    session, zone,
  };

  #[test]
  fn each_key_has_the_name_that_section_3_gives() {
    let mut bytes = [0; 32];
    bytes[0] = 0x0a;
    bytes[31] = 0xff;
    let token = Token(bytes);
    let account = AccountId(i64::MAX);
    let zeros = "00".repeat(30);
    assert_eq!(
      [
        session(&token),
        resume(&token),
        presence(account),
        queued(account),
        admit(account),
        zone(ZoneId(-3)),
        queue(1),
        admitted(1),
        admissions(u32::MAX),
        failed("AlIce"),
        created(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
        created(IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped())),
        created(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 3, 4, 5, 6))),
        chat("trade"),
        DIRECTORY.to_owned(),
      ],
      [
        format!("session:0a{zeros}ff"),
        format!("resume:0a{zeros}ff"),
        "presence:9223372036854775807".to_owned(),
        "queued:9223372036854775807".to_owned(),
        "admit:9223372036854775807".to_owned(),
        "zone:-3".to_owned(),
        "world:1:queue".to_owned(),
        "world:1:admitted".to_owned(),
        "world:4294967295:admissions".to_owned(),
        "failed:alice".to_owned(),
        "created:192.0.2.1".to_owned(),
        "created:192.0.2.1".to_owned(),
        "created:2001:db8:1:2::/64".to_owned(),
        "chat:trade".to_owned(),
        "directory".to_owned(),
      ]
    );
  }
}
