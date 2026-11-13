//! The data of mmo-archi that more than one process reads: the accounts, the zones and the
//! characters in PostgreSQL, and the name and the format of each key of Redis in [`keys`].
//!
//! [`Store`] is the one home of the SQL of the game. The keeper opens it with
//! [`Store::migrate`], which runs the migrations of `migrations/`, and each other process with
//! [`Store::open`], which refuses a database that lacks a migration of this build.
//!
//! - The login service: [`Store::create_account`], [`Store::sign_in`],
//!   [`Store::create_character`] and [`Store::characters`].
//! - A zone writer: [`Store::load`], when it takes a zone or a player.
//! - The login service and each layer writer: the [`Directory`] of the layers of a shard in
//!   Redis, with its scripts.
//! - The keeper: [`Store::add_zone`], [`Store::claim`], which gives a writer the next
//!   [`Epoch`](mmo_protocol::Epoch) of its zone, and [`Store::save`], which writes the
//!   [`Batch`](mmo_protocol::Batch) of a writer.

mod accounts;
mod characters;
mod directory;
pub mod keys;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod zones;

use mmo_protocol::{Epoch, KeeperRefusal, ZoneId};
use sqlx::PgPool;
use sqlx::migrate::Migrator;

pub use characters::Saved;
pub use directory::{Directory, LEASE, PARTY, PartyId, Placed};

static MIGRATOR: Migrator = sqlx::migrate!();

/// The code of PostgreSQL for a table that does not exist: a database where no keeper ran.
const UNDEFINED_TABLE: &str = "42P01";

/// The count of characters that one account holds at most. The table refuses a ninth character of
/// an account, whatever the count of clients that make one together.
pub const CHARACTERS: usize = 8;

/// A refusal of the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The database lacks a migration of this build, which the keeper runs when it starts.
  #[error("the database lacks the migration {version}, which the keeper runs")]
  Behind {
    /// The first migration that the database lacks.
    version: i64,
  },
  /// An account or a character holds the name already.
  #[error("the name {name} is taken")]
  Taken {
    /// The name, as the table holds it.
    name: String,
  },
  /// The account holds [`CHARACTERS`] characters already.
  #[error("the account {} holds {CHARACTERS} characters already", account.0)]
  Full {
    /// The account that holds the most.
    account: mmo_protocol::AccountId,
  },
  /// The table `zones` holds no zone of this id.
  #[error("no zone has the id {}", zone.0)]
  NoZone {
    /// The id that no zone has.
    zone: ZoneId,
  },
  /// A newer writer claimed the zone after the writer that sent the batch.
  #[error("zone {} is at epoch {}, and the batch is of epoch {}", zone.0, held.0, given.0)]
  Epoch {
    /// The zone of the batch.
    zone: ZoneId,
    /// The epoch of the last writer that claimed the zone.
    held: Epoch,
    /// The epoch of the batch.
    given: Epoch,
  },
  /// The table holds a password hash that argon2 cannot check.
  #[error("a password hash fails: {0}")]
  Hash(argon2::password_hash::Error),
  /// A migration fails.
  #[error(transparent)]
  Migrate(#[from] sqlx::migrate::MigrateError),
  /// The database refuses a query, or cannot be reached.
  #[error(transparent)]
  Database(#[from] sqlx::Error),
}

impl Error {
  /// The refusal that the keeper answers to a zone writer for this error: a zone that the table
  /// does not hold, a batch of an old epoch, or else a fault of PostgreSQL, after which the writer
  /// sends its batch again.
  #[must_use]
  pub const fn refusal(&self) -> KeeperRefusal {
    match *self {
      Self::NoZone { .. } => KeeperRefusal::NoZone,
      Self::Epoch { held, .. } => KeeperRefusal::Stale { held },
      _ => KeeperRefusal::Unavailable,
    }
  }
}

/// The connections to the database of the game.
#[derive(Clone, Debug)]
pub struct Store {
  pool: PgPool,
}

impl Store {
  /// Runs each migration that the database lacks, under the lock of sqlx, so two keepers never
  /// run them together.
  pub async fn migrate(pool: PgPool) -> Result<Self, Error> {
    MIGRATOR.run(&pool).await?;
    Ok(Self { pool })
  }

  /// Opens a database that holds each migration of this build, and changes nothing in it.
  pub async fn open(pool: PgPool) -> Result<Self, Error> {
    let applied: Vec<i64> =
      match sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
        .fetch_all(&pool)
        .await
      {
        Err(sqlx::Error::Database(found)) if found.code().as_deref() == Some(UNDEFINED_TABLE) => {
          Vec::new()
        }
        applied => applied?,
      };
    match MIGRATOR.iter().find(|migration| !applied.contains(&migration.version)) {
      Some(migration) => Err(Error::Behind { version: migration.version }),
      None => Ok(Self { pool }),
    }
  }
}

fn taken(error: sqlx::Error, name: &str) -> Error {
  match error {
    sqlx::Error::Database(found) if found.is_unique_violation() => {
      Error::Taken { name: name.to_owned() }
    }
    error => Error::Database(error),
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    AccountId, Angle, Batch, CharacterId, Epoch, KeeperRefusal, Password, Position, State, ZoneId,
  };
  use sqlx::PgPool;

  use super::{Error, Store};

  #[sqlx::test(migrations = false)]
  async fn a_process_opens_only_a_database_that_the_keeper_migrated(
    pool: PgPool,
  ) -> Result<(), Error> {
    let refused = Store::open(pool.clone()).await.err().map(|error| error.to_string());
    assert_eq!(
      refused.as_deref(),
      Some("the database lacks the migration 1, which the keeper runs")
    );
    Store::migrate(pool.clone()).await?;
    Store::open(pool).await?;
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn each_call_on_a_closed_pool_gives_the_error_of_the_pool(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool.clone()).await?;
    pool.close().await;
    let state = State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 1 };
    let batch = Batch { zone: ZoneId(1), epoch: Epoch(1), saves: Vec::new() };
    let refusals = [
      Store::migrate(pool.clone()).await.err(),
      Store::open(pool).await.err(),
      store.create_account("alice", Password("one".to_owned())).await.err(),
      store.sign_in("alice", Password("one".to_owned())).await.err(),
      store.create_character(AccountId(1), "Brom", ZoneId(1), state).await.err(),
      store.characters(AccountId(1)).await.err(),
      store.load(&[CharacterId(1)]).await.err(),
      store.add_zone(ZoneId(1), "meadow").await.err(),
      store.claim(ZoneId(1)).await.err(),
      store.save(&batch).await.err(),
    ];
    let closed = "attempted to acquire a connection on a closed pool";
    let mut expected = vec![Some(closed.to_owned()); 10];
    expected[0] = Some(format!("while executing migrations: {closed}"));
    let unavailable =
      refusals.iter().flatten().all(|refused| refused.refusal() == KeeperRefusal::Unavailable);
    assert_eq!(refusals.map(|refused| refused.map(|error| error.to_string())).to_vec(), expected);
    assert!(unavailable, "a closed pool is a fault of PostgreSQL");
    Ok(())
  }

  #[test]
  fn the_keeper_refuses_a_lost_zone_and_an_old_epoch_as_such() {
    let epoch = Error::Epoch { zone: ZoneId(1), held: Epoch(2), given: Epoch(1) };
    let refusals = [Error::NoZone { zone: ZoneId(1) }, epoch].map(|error| error.refusal());
    assert_eq!(refusals, [KeeperRefusal::NoZone, KeeperRefusal::Stale { held: Epoch(2) }]);
  }
}
