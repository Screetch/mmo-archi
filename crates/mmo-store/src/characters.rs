use mmo_protocol::{AccountId, Angle, Character, CharacterId, Position, State, ZoneId};

use crate::{Error, Store, taken};

/// A character as its last save left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
  /// The character.
  pub character: CharacterId,
  /// The zone of the last save.
  pub zone: ZoneId,
  /// The state of the last save.
  pub state: State,
  /// The version of the last save. A new character has the version 0, and each save that the
  /// keeper writes carries a greater one.
  pub version: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
  id: i64,
  zone_id: i32,
  x: i32,
  y: i32,
  z: i32,
  yaw: i32,
  hit_points: i32,
  version: i64,
}

impl From<Row> for Saved {
  #[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the table checks that the yaw is from 0 to 65535"
  )]
  fn from(row: Row) -> Self {
    Self {
      character: CharacterId(row.id),
      zone: ZoneId(row.zone_id),
      state: State {
        position: Position { x: row.x, y: row.y, z: row.z },
        yaw: Angle(row.yaw as u16),
        hit_points: row.hit_points,
      },
      version: row.version,
    }
  }
}

impl Store {
  /// Makes a character of `account` in `zone`, with the version 0. The table counts the
  /// characters of the account while it holds the row of the account, so two makers of one account
  /// never pass the count together. It gives [`Error::Full`] when the account holds
  /// [`CHARACTERS`](crate::CHARACTERS) already.
  pub async fn create_character(
    &self,
    account: AccountId,
    name: &str,
    zone: ZoneId,
    state: State,
  ) -> Result<CharacterId, Error> {
    sqlx::query_scalar(
      "INSERT INTO characters (account_id, name, zone_id, x, y, z, yaw, hit_points) \
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(account.0)
    .bind(name)
    .bind(zone.0)
    .bind(state.position.x)
    .bind(state.position.y)
    .bind(state.position.z)
    .bind(i32::from(state.yaw.0))
    .bind(state.hit_points)
    .fetch_one(&self.pool)
    .await
    .map(CharacterId)
    .map_err(|error| match error {
      sqlx::Error::Database(ref found) if found.constraint() == Some("characters_account_cap") => {
        Error::Full { account }
      }
      error => taken(error, name),
    })
  }

  /// The characters of `account`, in the order of their ids.
  pub async fn characters(&self, account: AccountId) -> Result<Vec<Character>, Error> {
    let rows: Vec<(i64, String)> =
      sqlx::query_as("SELECT id, name FROM characters WHERE account_id = $1 ORDER BY id")
        .bind(account.0)
        .fetch_all(&self.pool)
        .await?;
    Ok(rows.into_iter().map(|(id, name)| Character { id: CharacterId(id), name }).collect())
  }

  /// The last save of each of `characters` that the table holds, in the order of their ids.
  pub async fn load(&self, characters: &[CharacterId]) -> Result<Vec<Saved>, Error> {
    let ids: Vec<i64> = characters.iter().map(|character| character.0).collect();
    let rows: Vec<Row> = sqlx::query_as(
      "SELECT id, zone_id, x, y, z, yaw, hit_points, version FROM characters \
       WHERE id = ANY($1) ORDER BY id",
    )
    .bind(ids)
    .fetch_all(&self.pool)
    .await?;
    Ok(rows.into_iter().map(Saved::from).collect())
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{Angle, Character, CharacterId, Password, Position, State, ZoneId};
  use sqlx::PgPool;

  use super::Saved;
  use crate::{CHARACTERS, Error, Store};

  const START: State =
    State { position: Position { x: 120, y: 0, z: -4500 }, yaw: Angle(u16::MAX), hit_points: 100 };

  #[sqlx::test(migrations = false)]
  async fn an_account_lists_its_characters_and_a_writer_loads_their_last_save(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.add_zone(ZoneId(3), "meadow").await?;
    let account = store.create_account("alice", Password("one".to_owned())).await?;
    let other = store.create_account("bob", Password("two".to_owned())).await?;
    let brom = store.create_character(account, "Brom", ZoneId(3), START).await?;
    let away = State { position: Position { x: i32::MIN, y: i32::MAX, z: 0 }, ..START };
    let cael = store.create_character(account, "Cael", ZoneId(3), away).await?;
    store.create_character(other, "Dara", ZoneId(3), START).await?;
    assert_eq!(
      store.characters(account).await?,
      [
        Character { id: brom, name: "Brom".to_owned() },
        Character { id: cael, name: "Cael".to_owned() }
      ]
    );
    assert_eq!(
      store.load(&[cael, CharacterId(i64::MAX), brom]).await?,
      [
        Saved { character: brom, zone: ZoneId(3), state: START, version: 0 },
        Saved { character: cael, zone: ZoneId(3), state: away, version: 0 },
      ]
    );
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn two_makers_of_an_account_at_7_characters_make_one_more_and_never_two(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.add_zone(ZoneId(3), "meadow").await?;
    let account = store.create_account("alice", Password("one".to_owned())).await?;
    let other = store.create_account("bob", Password("two".to_owned())).await?;
    for name in ["Alpha", "Bravo", "Cael", "Dara", "Elan", "Finn", "Gale"] {
      store.create_character(account, name, ZoneId(3), START).await?;
    }
    let (first, second) = tokio::join!(
      store.create_character(account, "Hana", ZoneId(3), START),
      store.create_character(account, "Iris", ZoneId(3), START),
    );
    let refused = [first, second].into_iter().filter_map(Result::err).collect::<Vec<_>>();
    assert!(matches!(refused[..], [Error::Full { account: held }] if held == account));
    assert_eq!(
      refused[0].to_string(),
      format!("the account {} holds 8 characters already", account.0)
    );
    assert_eq!(store.characters(account).await?.len(), CHARACTERS);
    store.create_character(other, "Jade", ZoneId(3), START).await?;
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_character_needs_a_free_name_and_a_zone(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.add_zone(ZoneId(3), "meadow").await?;
    let account = store.create_account("alice", Password("one".to_owned())).await?;
    store.create_character(account, "Brom", ZoneId(3), START).await?;
    let taken = store.create_character(account, "Brom", ZoneId(3), START).await;
    assert_eq!(
      taken.err().map(|error| error.to_string()).as_deref(),
      Some("the name Brom is taken")
    );
    let nowhere = store.create_character(account, "Cael", ZoneId(4), START).await;
    assert!(matches!(
      nowhere,
      Err(Error::Database(sqlx::Error::Database(ref found)))
        if found.constraint() == Some("characters_zone_id_fkey")
    ));
    Ok(())
  }
}
