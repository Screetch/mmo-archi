use std::sync::LazyLock;

use mmo_protocol::{AccountId, Population, Realm, RealmId, Rules, Token};
use mmo_store::keys;
use redis::Script;
use redis::aio::ConnectionManager;

use crate::Error;

/// The most accounts that one realm admits in one second, over all the copies of the login
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

/// The name of `rules` in the configuration and in the entry of the realm in Redis.
const fn named(rules: Rules) -> &'static str {
  match rules {
    Rules::Normal => "normal",
    Rules::PlayerAgainstPlayer => "player-against-player",
    Rules::RolePlay => "role-play",
  }
}

/// The type of rules whose name is `name`, as [`named`] gives it.
pub(crate) fn ruled(name: &str) -> Option<Rules> {
  [Rules::Normal, Rules::PlayerAgainstPlayer, Rules::RolePlay]
    .into_iter()
    .find(|&rules| named(rules) == name)
}

/// The entry of the realm that a copy of the login service serves, as its configuration gives it:
/// the identity of the realm, its three attributes, the address of its login service, and its
/// caps. It is the one home of the entry: each copy writes it in Redis at each admission, and the
/// list of the realms reads it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
  /// The id of the realm, as the keys of `mmo_store::keys` name it.
  pub id: u32,
  /// The name of the realm, which no other realm holds.
  pub name: String,
  /// The region of the realm.
  pub region: String,
  /// The language of the realm.
  pub language: String,
  /// The type of rules of the realm, a label that no rule of the game reads.
  pub rules: Rules,
  /// The address of the login service of the realm that a client reaches, as `host:port`.
  pub login: String,
  /// The most accounts that hold a slot in the realm at one time.
  pub cap: u32,
  /// The most players of each layer of the realm, 5 at least.
  pub layer_cap: u32,
}

/// Where an account stands in the queue of its realm.
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
    /// The estimate in seconds, or `u32::MAX` when the realm admitted no account in the window.
    wait: u32,
  },
}

impl Entry {
  /// The id of the realm, whose characters alone its login service lists, makes and places.
  pub(crate) const fn realm(&self) -> RealmId {
    RealmId(self.id)
  }

  /// Writes the entry and the caps of the realm, renews or ends each slot that ended, then admits
  /// the head of the queue into the free slots.
  pub(crate) async fn admit(&self, redis: &mut ConnectionManager) -> Result<u32, Error> {
    let rules = named(self.rules);
    let admitted = ADMIT
      .key(keys::queue(self.id))
      .key(keys::admitted(self.id))
      .key(keys::admissions(self.id))
      .key(keys::realm(self.realm()))
      .key(keys::REALMS)
      .arg(self.cap)
      .arg(BURST)
      .arg(SLOT)
      .arg(EXTENSION)
      .arg(WINDOW * 1000)
      .arg(SLOT / 1000)
      .arg(keys::PRESENCE)
      .arg(keys::QUEUED)
      .arg(keys::ADMIT)
      .arg(self.id)
      .arg(&[(keys::NAME, &self.name), (keys::REGION, &self.region)])
      .arg(&[(keys::LANGUAGE, &self.language), (keys::LOGIN, &self.login)])
      .arg(&[(keys::RULES, rules)])
      .arg(&[(keys::CAP, self.cap), (keys::LAYER_CAP, self.layer_cap)])
      .invoke_async(redis)
      .await?;
    Ok(admitted)
  }

  /// The level of population of the realm, from the accounts that its admitted set holds.
  pub(crate) async fn population(
    &self,
    redis: &mut ConnectionManager,
  ) -> Result<Population, Error> {
    let admitted: u32 = redis::cmd("ZCARD").arg(keys::admitted(self.id)).query_async(redis).await?;
    Ok(Population::of(admitted, self.cap))
  }

  /// The place of `account`, which renews its marks. With `enter`, an account with no place
  /// enters at the end of the queue, and an account with a place keeps it.
  pub(crate) async fn place(
    &self,
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

/// Each realm whose entry Redis holds, by its id, with its level of population and the length of
/// its queue. A realm whose entry lacks a field, or names a type of rules that this version does
/// not know, stays out of the list, since a client could not show it.
pub(crate) async fn realms(redis: &mut ConnectionManager) -> Result<Vec<Realm>, Error> {
  let mut ids: Vec<u32> = redis::cmd("SMEMBERS").arg(keys::REALMS).query_async(redis).await?;
  ids.sort_unstable();
  let mut pipe = redis::pipe();
  for &id in &ids {
    let fields = [keys::NAME, keys::REGION, keys::LANGUAGE, keys::RULES, keys::LOGIN, keys::CAP];
    pipe
      .cmd("HMGET")
      .arg(keys::realm(RealmId(id)))
      .arg(&fields)
      .zcard(keys::admitted(id))
      .zcard(keys::queue(id));
  }
  let found: Vec<(Vec<Option<String>>, u32, u32)> = pipe.query_async(redis).await?;
  Ok(
    ids
      .into_iter()
      .zip(found)
      .filter_map(|(id, (entry, admitted, queue))| listed(id, &entry, admitted, queue))
      .collect(),
  )
}

/// The realm `id` as the list gives it, from the fields of its entry, its count of admitted
/// accounts and the length of its queue.
fn listed(id: u32, entry: &[Option<String>], admitted: u32, queue: u32) -> Option<Realm> {
  let [Some(name), Some(region), Some(language), Some(rules), Some(login), Some(cap)] = entry
  else {
    return None;
  };
  let rules = ruled(rules)?;
  let population = Population::of(admitted, cap.parse().ok()?);
  Some(Realm {
    id: RealmId(id),
    name: name.clone(),
    region: region.clone(),
    language: language.clone(),
    rules,
    population,
    queue,
    login: login.clone(),
  })
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
