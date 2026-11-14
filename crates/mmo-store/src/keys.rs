//! The name and the format of each key of Redis, which the processes of the game share.
//!
//! Redis holds the short shared state, which the game can lose and make again. Each key is made
//! here alone, so a writer and a reader of a key never disagree on its name. Section 3 of
//! `docs/architecture.md` gives the type, the writer and the life of each key.
//!
//! A Lua script that Redis runs makes the key of an account from its prefix, as
//! [`QUEUED`] and the id of the account, so each prefix that a script needs is public.

use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;

use mmo_protocol::{AccountId, Layer, RealmId, Token};

/// The channel of the directory of the layers. Each script of
/// [`Directory`](crate::Directory) names on it each layer that it opened, closed, put in drain or
/// gave a writer, as [`layer`] names it.
pub const DIRECTORY: &str = "directory";

/// The fields of [`realm`], which the login service writes: the name of the realm, the most
/// accounts that it admits, and the most players of each of its layers. The scripts of the
/// directory read [`LAYER_CAP`] by this name.
pub const NAME: &str = "name";
pub const CAP: &str = "cap";
pub const LAYER_CAP: &str = "layer_cap";

/// The fields of [`writer`], which a writer writes and a gateway reads: the address that the
/// gateways dial, and the epoch of the writer. The scripts of the directory write them by these
/// names.
pub const ADDRESS: &str = "address";
pub const EPOCH: &str = "epoch";

/// The field of [`session`] that holds the account, besides the fields of [`presence`] but
/// [`EPOCH`].
pub const ACCOUNT: &str = "account";

/// The fields of [`presence`], besides [`EPOCH`]: the character of the account, its realm, its
/// zone and its layer. The scripts of the directory read them by these names.
pub const CHARACTER: &str = "character";
pub const REALM: &str = "realm";
pub const ZONE: &str = "zone";
pub const LAYER: &str = "layer";

/// The prefix of [`presence`].
pub const PRESENCE: &str = "presence:";

/// The prefix of [`queued`].
pub const QUEUED: &str = "queued:";

/// The prefix of [`admit`].
pub const ADMIT: &str = "admit:";

/// The session of `token`, a hash of the account, the character, the realm, the zone and the
/// layer.
pub fn session(token: &Token) -> String {
  format!("session:{}", hex(token))
}

/// The presence of `account`, a hash of the character, the zone and the epoch of its writer.
pub fn presence(account: AccountId) -> String {
  format!("{PRESENCE}{}", account.0)
}

/// The caps of `realm`, a hash of its [`NAME`], its [`CAP`] and its [`LAYER_CAP`].
pub fn realm(realm: RealmId) -> String {
  format!("realm:{}", realm.0)
}

/// The layers of `realm`, a hash from the [`field`] of each layer to its count of players.
pub fn layers(realm: RealmId) -> String {
  format!("layers:{}", realm.0)
}

/// The layers of `realm` that drain, a set of the [`field`] of each.
pub fn draining(realm: RealmId) -> String {
  format!("draining:{}", realm.0)
}

/// The field of `layer` in [`layers`] and [`draining`]: its zone and its number.
pub fn field(layer: Layer) -> String {
  format!("{}:{}", layer.zone.0, layer.number)
}

/// The name of `layer` of `realm` in the keys of a layer and on the channel [`DIRECTORY`].
pub fn layer(realm: RealmId, layer: Layer) -> String {
  format!("{}:{}", realm.0, field(layer))
}

/// The prefix of the [`writer`] of each layer of `realm`, before the [`field`] of the layer.
pub fn writers(realm: RealmId) -> String {
  format!("writer:{}:", realm.0)
}

/// The lease of the writer of `layer` of `realm`, a hash of its [`ADDRESS`] and its [`EPOCH`].
/// A layer whose lease ended has no writer.
pub fn writer(realm: RealmId, layer: Layer) -> String {
  format!("{}{}", writers(realm), field(layer))
}

/// The prefix of the [`placed`] of each layer of `realm`, before the [`field`] of the layer.
pub fn places(realm: RealmId) -> String {
  format!("placed:{}:", realm.0)
}

/// The places that placements gave in `layer` of `realm`, a sorted set of the accounts by the
/// time in milliseconds when each place ends. The writer of the layer takes a place away when it
/// takes its player.
pub fn placed(realm: RealmId, layer: Layer) -> String {
  format!("{}{}", places(realm), field(layer))
}

/// The ticks of the writer of `layer` of `realm`, a hash from the number of a bucket to the
/// count of ticks in it. Bucket `n` holds each tick that took from `n` times [`TICK_BUCKET`] up to
/// the next bucket, and the last bucket holds each longer tick. The writer adds to it and never
/// resets it, so a reader that wants the ticks of a run reads the hash before and after, and takes
/// the difference.
pub fn ticks(realm: RealmId, layer: Layer) -> String {
  format!("ticks:{}", self::layer(realm, layer))
}

/// The time that one bucket of [`ticks`] spans.
pub const TICK_BUCKET: Duration = Duration::from_micros(250);

/// The count of buckets of [`ticks`], so the last one starts at 99.75 ms: a tick of 50 ms or more
/// is late, and a tick of twice that or more is not told apart from a longer one.
pub const TICK_BUCKETS: u32 = 400;

/// The bucket of [`ticks`] for a tick that took `elapsed`.
pub fn tick_bucket(elapsed: Duration) -> u32 {
  let bucket = u32::try_from(elapsed.as_micros() / TICK_BUCKET.as_micros());
  bucket.map_or(TICK_BUCKETS - 1, |bucket| bucket.min(TICK_BUCKETS - 1))
}

/// The queue of `realm`, a sorted set of the queued accounts by the time in milliseconds when each
/// entered.
pub fn queue(realm: u32) -> String {
  format!("realm:{realm}:queue")
}

/// The admitted accounts of `realm`, a sorted set by the time in milliseconds when the slot of
/// each ends.
pub fn admitted(realm: u32) -> String {
  format!("realm:{realm}:admitted")
}

/// The admissions of `realm`, a sorted set of the accounts by the time in milliseconds when each
/// was admitted, from which a queued client gets an estimate of its wait.
pub fn admissions(realm: u32) -> String {
  format!("realm:{realm}:admissions")
}

/// The mark that `account` still waits in a queue.
pub fn queued(account: AccountId) -> String {
  format!("{QUEUED}{}", account.0)
}

/// The mark that `account` has a slot in its realm.
pub fn admit(account: AccountId) -> String {
  format!("{ADMIT}{}", account.0)
}

/// The account of a queued client that can come back with `token`.
pub fn resume(token: &Token) -> String {
  format!("resume:{}", hex(token))
}

/// The count of the failed logins of the last minute for the name `name`, in lower case, from
/// `address`, which counts as in [`created`].
pub fn failed(address: IpAddr, name: &str) -> String {
  format!("failed:{}:{}", host(address), name.to_lowercase())
}

/// The count of the failed logins of the last 10 minutes from `address` over all names, which
/// counts as in [`created`].
pub fn guessed(address: IpAddr) -> String {
  format!("guessed:{}", host(address))
}

/// The count of the new accounts of the last hour from `address`. An address of IPv6 counts by its
/// network of /64, since one host commonly holds the whole network, and an address of IPv4 in
/// IPv6 counts as its address of IPv4.
pub fn created(address: IpAddr) -> String {
  format!("created:{}", host(address))
}

fn host(address: IpAddr) -> String {
  match address.to_canonical() {
    IpAddr::V4(address) => address.to_string(),
    IpAddr::V6(address) => format!("{}/64", Ipv6Addr::from(u128::from(address) >> 64 << 64)),
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
  use std::time::Duration;

  use mmo_protocol::{AccountId, Layer, RealmId, Token, ZoneId};

  use super::{
    DIRECTORY, TICK_BUCKETS, admissions, admit, admitted, chat, created, draining, failed, field,
    guessed, layer, layers, placed, places, presence, queue, queued, realm, resume, session,
    tick_bucket, ticks, writer, writers,
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
        queue(1),
        admitted(1),
        admissions(u32::MAX),
        failed(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), "AlIce"),
        failed(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 3, 4, 5, 6)), "bob"),
        guessed(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
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
        "realm:1:queue".to_owned(),
        "realm:1:admitted".to_owned(),
        "realm:4294967295:admissions".to_owned(),
        "failed:192.0.2.1:alice".to_owned(),
        "failed:2001:db8:1:2::/64:bob".to_owned(),
        "guessed:192.0.2.1".to_owned(),
        "created:192.0.2.1".to_owned(),
        "created:192.0.2.1".to_owned(),
        "created:2001:db8:1:2::/64".to_owned(),
        "chat:trade".to_owned(),
        "directory".to_owned(),
      ]
    );
  }

  #[test]
  fn each_key_of_the_directory_of_a_realm_has_the_name_that_section_3_gives() {
    let far = Layer { zone: ZoneId(-3), number: u32::MAX };
    assert_eq!(
      [
        realm(RealmId(2)),
        layers(RealmId(2)),
        draining(RealmId(2)),
        field(far),
        layer(RealmId(2), far),
        writers(RealmId(2)),
        writer(RealmId(u32::MAX), far),
        ticks(RealmId(2), far),
        places(RealmId(2)),
        placed(RealmId(2), far),
      ],
      [
        "realm:2",
        "layers:2",
        "draining:2",
        "-3:4294967295",
        "2:-3:4294967295",
        "writer:2:",
        "writer:4294967295:-3:4294967295",
        "ticks:2:-3:4294967295",
        "placed:2:",
        "placed:2:-3:4294967295",
      ]
    );
  }

  #[test]
  fn a_tick_falls_in_the_bucket_of_its_quarter_of_a_millisecond_and_a_long_one_in_the_last() {
    let micros = |micros| tick_bucket(Duration::from_micros(micros));
    assert_eq!(
      [micros(0), micros(249), micros(250), micros(50_000), micros(99_749), micros(99_750)],
      [0, 0, 1, 200, 398, 399]
    );
    assert_eq!([micros(10_000_000), tick_bucket(Duration::MAX)], [TICK_BUCKETS - 1; 2]);
  }
}
