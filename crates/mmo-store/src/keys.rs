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

use mmo_protocol::{AccountId, Layer, ShardId, Token, ZoneId};

/// The channel of the directory of the layers. Each script of
/// [`Directory`](crate::Directory) names on it each layer that it opened, closed, put in drain or
/// gave a writer, as [`layer`] names it.
pub const DIRECTORY: &str = "directory";

/// The fields of [`shard`], which the login service writes: the name of the shard, the most
/// accounts that it admits, and the most players of each of its layers. The scripts of the
/// directory read [`LAYER_CAP`] by this name.
pub const NAME: &str = "name";
pub const CAP: &str = "cap";
pub const LAYER_CAP: &str = "layer_cap";

/// The fields of [`zone`] and of [`writer`], which a writer writes and a gateway reads: the
/// address that the gateways dial, the epoch of the writer, and the count of its players. The
/// scripts of the directory write [`ADDRESS`] and [`EPOCH`] by these names.
pub const ADDRESS: &str = "address";
pub const EPOCH: &str = "epoch";
pub const PLAYERS: &str = "players";

/// The fields of [`presence`], besides [`EPOCH`]: the character of the account, its shard, its
/// zone and its layer, and its party. The scripts of the directory read them by these names.
pub const CHARACTER: &str = "character";
pub const SHARD: &str = "shard";
pub const ZONE: &str = "zone";
pub const LAYER: &str = "layer";
pub const PARTY: &str = "party";

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

/// The caps of `shard`, a hash of its [`NAME`], its [`CAP`] and its [`LAYER_CAP`].
pub fn shard(shard: ShardId) -> String {
  format!("shard:{}", shard.0)
}

/// The layers of `shard`, a hash from the [`field`] of each layer to its count of players.
pub fn layers(shard: ShardId) -> String {
  format!("layers:{}", shard.0)
}

/// The layers of `shard` that drain, a set of the [`field`] of each.
pub fn draining(shard: ShardId) -> String {
  format!("draining:{}", shard.0)
}

/// The field of `layer` in [`layers`] and [`draining`]: its zone and its number.
pub fn field(layer: Layer) -> String {
  format!("{}:{}", layer.zone.0, layer.number)
}

/// The name of `layer` of `shard` in the keys of a layer and on the channel [`DIRECTORY`].
pub fn layer(shard: ShardId, layer: Layer) -> String {
  format!("{}:{}", shard.0, field(layer))
}

/// The prefix of the [`writer`] of each layer of `shard`, before the [`field`] of the layer.
pub fn writers(shard: ShardId) -> String {
  format!("writer:{}:", shard.0)
}

/// The lease of the writer of `layer` of `shard`, a hash of its [`ADDRESS`] and its [`EPOCH`].
/// A layer whose lease ended has no writer.
pub fn writer(shard: ShardId, layer: Layer) -> String {
  format!("{}{}", writers(shard), field(layer))
}

/// The prefix of each party of `shard`, before the id of the party.
pub fn parties(shard: ShardId) -> String {
  format!("party:{}:", shard.0)
}

/// The entry of `zone` in the directory of zones, a hash of the address, the epoch and the count
/// of players of its writer.
pub fn zone(zone: ZoneId) -> String {
  format!("zone:{}", zone.0)
}

/// The ticks of the writer of `zone`, a hash from the number of a bucket to the count of ticks in
/// it. Bucket `n` holds each tick that took from `n` times [`TICK_BUCKET`] up to the next bucket,
/// and the last bucket holds each longer tick. The writer adds to it and never resets it, so a
/// reader that wants the ticks of a run reads the hash before and after, and takes the difference.
pub fn ticks(zone: ZoneId) -> String {
  format!("ticks:{}", zone.0)
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

  use mmo_protocol::{AccountId, Layer, ShardId, Token, ZoneId};

  use super::{
    DIRECTORY, TICK_BUCKETS, admissions, admit, admitted, chat, created, draining, failed, field,
    guessed, layer, layers, parties, presence, queue, queued, resume, session, shard, tick_bucket,
    ticks, writer, writers, zone,
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
        ticks(ZoneId(3)),
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
        "zone:-3".to_owned(),
        "ticks:3".to_owned(),
        "world:1:queue".to_owned(),
        "world:1:admitted".to_owned(),
        "world:4294967295:admissions".to_owned(),
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
  fn each_key_of_the_directory_of_a_shard_has_the_name_that_section_3_gives() {
    let far = Layer { zone: ZoneId(-3), number: u32::MAX };
    assert_eq!(
      [
        shard(ShardId(2)),
        layers(ShardId(2)),
        draining(ShardId(2)),
        field(far),
        layer(ShardId(2), far),
        writers(ShardId(2)),
        writer(ShardId(u32::MAX), far),
        parties(ShardId(2)),
      ],
      [
        "shard:2",
        "layers:2",
        "draining:2",
        "-3:4294967295",
        "2:-3:4294967295",
        "writer:2:",
        "writer:4294967295:-3:4294967295",
        "party:2:",
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
