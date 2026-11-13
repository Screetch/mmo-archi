use std::sync::LazyLock;

use mmo_protocol::{AccountId, ShardId, Token};
use mmo_store::keys;
use redis::Script;
use redis::aio::ConnectionManager;

use crate::Error;

/// The most accounts that one world admits in one second, over all the copies of the login
/// service, so the loads of the characters spread over time.
const BURST: u32 = 100;

/// The slot of an admitted account in milliseconds: the time that its client has to reach a
/// gateway.
const SLOT: u64 = 60_000;

/// The extension in milliseconds of each slot that ends while the presence of its account stands.
const EXTENSION: u64 = 30_000;

/// The window of the admissions from which a queued client gets an estimate of its wait, in
/// seconds.
const WINDOW: u64 = 600;

/// The life in seconds of the marks of a queued client, which each wait of the client renews.
const MARK: u64 = 120;

static ADMIT: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("admit.lua")));
static PLACE: LazyLock<Script> = LazyLock::new(|| Script::new(include_str!("place.lua")));

/// A world of the game: its id in the keys of Redis, the most accounts that it admits at one
/// time, and the most players of each of its layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
  /// The id of the world, as the keys of `mmo_store::keys` name it.
  pub id: u32,
  /// The most accounts that hold a slot in the world at one time.
  pub cap: u32,
  /// The most players of each layer of the world, which a party of
  /// [`PARTY`](mmo_store::PARTY) players always fits in.
  pub layer_cap: u32,
}

/// Where an account stands in the queue of its world.
pub(crate) enum Place {
  /// The account has a slot.
  Admitted,
  /// The presence of the account stands: it plays already.
  Online,
  /// The account has no place: it never entered, or its place ended.
  Gone,
  /// The account waits.
  Queued {
    /// The first account of the queue is at 0.
    position: u32,
    length: u32,
    /// The estimate in seconds, or `u32::MAX` when the world admitted no account in the window.
    wait: u32,
  },
}

impl World {
  /// Writes the caps of the world, renews or ends each slot that ended, then admits the head of the
  /// queue into the free slots.
  pub(crate) async fn admit(self, redis: &mut ConnectionManager) -> Result<u32, Error> {
    let admitted = ADMIT
      .key(keys::queue(self.id))
      .key(keys::admitted(self.id))
      .key(keys::admissions(self.id))
      .key(keys::shard(ShardId(self.id)))
      .arg(self.cap)
      .arg(BURST)
      .arg(SLOT)
      .arg(EXTENSION)
      .arg(WINDOW * 1000)
      .arg(SLOT / 1000)
      .arg(keys::PRESENCE)
      .arg(keys::QUEUED)
      .arg(keys::ADMIT)
      .arg(self.layer_cap)
      .invoke_async(redis)
      .await?;
    Ok(admitted)
  }

  /// The place of `account`, which renews its marks. With `enter`, an account with no place
  /// enters at the end of the queue, and an account with a place keeps it.
  pub(crate) async fn place(
    self,
    redis: &mut ConnectionManager,
    account: AccountId,
    resume: &Token,
    enter: bool,
  ) -> Result<Place, Error> {
    let found: Vec<i64> = PLACE
      .key(keys::queue(self.id))
      .key(keys::admissions(self.id))
      .key(keys::queued(account))
      .key(keys::admit(account))
      .key(keys::resume(resume))
      .key(keys::presence(account))
      .arg(account.0)
      .arg(u8::from(enter))
      .arg(MARK)
      .arg(WINDOW * 1000)
      .invoke_async(redis)
      .await?;
    Ok(match found[..] {
      [-1] => Place::Admitted,
      [-3] => Place::Online,
      [rank, length, admissions] => Place::Queued {
        position: saturated(rank),
        length: saturated(length),
        wait: estimate(rank, admissions),
      },
      _ => Place::Gone,
    })
  }
}

/// The seconds until the admission of the account at `rank`, at the pace of the `admissions` of
/// the window.
fn estimate(rank: i64, admissions: i64) -> u32 {
  if admissions <= 0 {
    return u32::MAX;
  }
  let pace = (rank + 1).saturating_mul(WINDOW.cast_signed());
  saturated(pace.saturating_add(admissions - 1) / admissions)
}

fn saturated(count: i64) -> u32 {
  u32::try_from(count).unwrap_or(u32::MAX)
}
