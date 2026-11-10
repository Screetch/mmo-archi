use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{self, PasswordHasher, PasswordVerifier};
use mmo_protocol::{AccountId, Password};
use tokio::task::spawn_blocking;

use crate::{Error, Store, taken};

/// The hash that the sign in of an unknown name checks, so that it takes the time of a known name,
/// and the time tells no one which names exist.
static DECOY: LazyLock<String> = LazyLock::new(|| hash(&Password(String::new())));

impl Store {
  /// Makes an account whose name is `name` in lower case.
  ///
  /// The table holds the argon2id hash of the password in the PHC format, with 19 MiB of memory,
  /// 2 passes, 1 lane and a random salt. The hash takes about 10 ms of one core, on a thread apart
  /// from the reactor.
  pub async fn create_account(&self, name: &str, password: Password) -> Result<AccountId, Error> {
    let hash = off_reactor(move || hash(&password)).await;
    let name = name.to_lowercase();
    sqlx::query_scalar("INSERT INTO accounts (name, password_hash) VALUES ($1, $2) RETURNING id")
      .bind(&name)
      .bind(hash)
      .fetch_one(&self.pool)
      .await
      .map(AccountId)
      .map_err(|error| taken(error, &name))
  }

  /// The account of `name`, in any case, when `password` is its password, and else `None`.
  ///
  /// An unknown name and a wrong password take the same work, so the time of a refusal does not
  /// tell which names exist.
  pub async fn sign_in(&self, name: &str, password: Password) -> Result<Option<AccountId>, Error> {
    let found: Option<(i64, String)> =
      sqlx::query_as("SELECT id, password_hash FROM accounts WHERE name = $1")
        .bind(name.to_lowercase())
        .fetch_optional(&self.pool)
        .await?;
    let (account, hash) =
      found.map_or_else(|| (None, DECOY.clone()), |(id, hash)| (Some(AccountId(id)), hash));
    let right = off_reactor(move || verify(&password, &hash)).await;
    right.map(|right| account.filter(|_| right))
  }
}

#[expect(clippy::expect_used, reason = "the work of a password does not panic")]
async fn off_reactor<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
  spawn_blocking(work).await.expect("the work of a password does not panic")
}

#[expect(clippy::expect_used, reason = "argon2 hashes each password that a frame can hold")]
fn hash(password: &Password) -> String {
  Argon2::default()
    .hash_password_with_salt(password.0.as_bytes(), &password_hash::generate_salt())
    .expect("the default params and a salt of 16 bytes hash each password of less than 4 GiB")
    .to_string()
}

fn verify(password: &Password, hash: &str) -> Result<bool, Error> {
  let hash = PasswordHash::new(hash).map_err(|error| Error::Hash(error.into()))?;
  match Argon2::default().verify_password(password.0.as_bytes(), &hash) {
    Ok(()) => Ok(true),
    Err(password_hash::Error::PasswordInvalid) => Ok(false),
    Err(error) => Err(Error::Hash(error)),
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::Password;
  use sqlx::PgPool;

  use crate::{Error, Store};

  fn secret(text: &str) -> Password {
    Password(text.to_owned())
  }

  #[sqlx::test(migrations = false)]
  async fn an_account_signs_in_with_its_password_alone(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let account = store.create_account("Alice", secret("a long passphrase")).await?;
    assert_eq!(store.sign_in("ALICE", secret("a long passphrase")).await?, Some(account));
    assert_eq!(store.sign_in("alice", secret("a long passphrasE")).await?, None);
    assert_eq!(store.sign_in("bob", secret("a long passphrase")).await?, None);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn the_table_holds_a_salted_argon2id_hash_of_19_mib_2_passes_and_1_lane(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool.clone()).await?;
    store.create_account("alice", secret("the same")).await?;
    store.create_account("bob", secret("the same")).await?;
    let hashes: Vec<String> =
      sqlx::query_scalar("SELECT password_hash FROM accounts ORDER BY id").fetch_all(&pool).await?;
    let params: Vec<&str> = hashes.iter().map(|hash| &hash[..31]).collect();
    assert_eq!(params, ["$argon2id$v=19$m=19456,t=2,p=1$"; 2]);
    assert_ne!(hashes[0], hashes[1]);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_name_is_taken_in_any_case(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.create_account("alice", secret("one")).await?;
    let refused = store.create_account("ALICE", secret("two")).await;
    assert_eq!(
      refused.err().map(|error| error.to_string()).as_deref(),
      Some("the name alice is taken")
    );
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_hash_that_argon2_cannot_check_fails_the_sign_in(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool.clone()).await?;
    store.create_account("alice", secret("one")).await?;
    let mut refusals = Vec::new();
    for hash in ["one", "$argon2id$v=19$m=1,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNo"] {
      sqlx::query("UPDATE accounts SET password_hash = $1").bind(hash).execute(&pool).await?;
      let refused = store.sign_in("alice", secret("one")).await;
      refusals.push(refused.err().map(|error| error.to_string()));
    }
    assert_eq!(
      refusals,
      [
        Some("a password hash fails: invalid encoding".to_owned()),
        Some("a password hash fails: invalid parameter: \"m\"".to_owned()),
      ]
    );
    Ok(())
  }
}
