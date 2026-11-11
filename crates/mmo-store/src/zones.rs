use mmo_protocol::{Batch, Epoch, Save, ZoneId};

use crate::{Error, Store};

/// One statement, so one transaction: it locks the row of the zone, and writes only when the batch
/// carries the epoch of that row.
const SAVE: &str = "WITH zone AS (SELECT epoch FROM zones WHERE id = $1 FOR UPDATE), \
  written AS (UPDATE characters AS c \
  SET zone_id = $1, x = s.x, y = s.y, z = s.z, yaw = s.yaw, hit_points = s.hit_points, \
  version = s.version, saved_at = now() \
  FROM zone, unnest($3::bigint[], $4::integer[], $5::integer[], $6::integer[], \
  $7::integer[], $8::integer[], $9::bigint[]) AS s (id, x, y, z, yaw, hit_points, version) \
  WHERE zone.epoch = $2 AND c.id = s.id AND s.version > c.version RETURNING c.id) \
  SELECT (SELECT epoch FROM zone), (SELECT count(*) FROM written)";

impl Store {
  /// Adds a zone to the world, at the epoch 0.
  pub async fn add_zone(&self, zone: ZoneId, name: &str) -> Result<(), Error> {
    sqlx::query("INSERT INTO zones (id, name) VALUES ($1, $2)")
      .bind(zone.0)
      .bind(name)
      .execute(&self.pool)
      .await?;
    Ok(())
  }

  /// Gives the next epoch of `zone` to the writer that claims it, so the keeper refuses each batch
  /// of the writers before it.
  pub async fn claim(&self, zone: ZoneId) -> Result<Epoch, Error> {
    let epoch: Option<i64> =
      sqlx::query_scalar("UPDATE zones SET epoch = epoch + 1 WHERE id = $1 RETURNING epoch")
        .bind(zone.0)
        .fetch_optional(&self.pool)
        .await?;
    epoch.map(Epoch).ok_or(Error::NoZone { zone })
  }

  /// Writes `batch` in one transaction, and gives the count of the characters that it wrote.
  ///
  /// It refuses a batch whose epoch is not the last one of its zone. It writes a save only when its
  /// version is greater than the saved one, so an old copy of a character never writes over a
  /// newer one, and it moves each character that it writes to the zone of the batch, as a hand-off
  /// needs.
  pub async fn save(&self, batch: &Batch) -> Result<u64, Error> {
    let column = |of: fn(&Save) -> i32| batch.saves.iter().map(of).collect::<Vec<i32>>();
    let (held, written): (Option<i64>, i64) = sqlx::query_as(SAVE)
      .bind(batch.zone.0)
      .bind(batch.epoch.0)
      .bind(batch.saves.iter().map(|save| save.character.0).collect::<Vec<i64>>())
      .bind(column(|save| save.state.position.x))
      .bind(column(|save| save.state.position.y))
      .bind(column(|save| save.state.position.z))
      .bind(column(|save| i32::from(save.state.yaw.0)))
      .bind(column(|save| save.state.hit_points))
      .bind(batch.saves.iter().map(|save| save.version).collect::<Vec<i64>>())
      .fetch_one(&self.pool)
      .await?;
    match held.map(Epoch) {
      Some(held) if held == batch.epoch => Ok(written.unsigned_abs()),
      Some(held) => Err(Error::Epoch { zone: batch.zone, held, given: batch.epoch }),
      None => Err(Error::NoZone { zone: batch.zone }),
    }
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{Angle, Batch, CharacterId, Epoch, Password, Position, Save, State, ZoneId};
  use sqlx::PgPool;

  use crate::{Error, Saved, Store};

  const START: State =
    State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };
  const MOVED: State =
    State { position: Position { x: 102_400, y: 250, z: -3 }, yaw: Angle(16384), hit_points: 7 };

  async fn world(store: &Store) -> [CharacterId; 2] {
    store.add_zone(ZoneId(1), "meadow").await.expect("a new zone");
    store.add_zone(ZoneId(2), "forest").await.expect("a new zone");
    let account = store.create_account("alice", Password("one".to_owned())).await.expect("one");
    let brom = store.create_character(account, "Brom", ZoneId(1), START).await.expect("Brom");
    let cael = store.create_character(account, "Cael", ZoneId(1), START).await.expect("Cael");
    [brom, cael]
  }

  #[sqlx::test(migrations = false)]
  async fn each_claim_gives_the_next_epoch_of_its_zone(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.add_zone(ZoneId(1), "meadow").await?;
    assert_eq!(store.claim(ZoneId(1)).await?, Epoch(1));
    assert_eq!(store.claim(ZoneId(1)).await?, Epoch(2));
    let refused = store.claim(ZoneId(9)).await.err().map(|error| error.to_string());
    assert_eq!(refused.as_deref(), Some("no zone has the id 9"));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_batch_writes_each_newer_save_and_moves_it_to_its_zone(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let [brom, cael] = world(&store).await;
    let epoch = store.claim(ZoneId(2)).await?;
    let saves = vec![
      Save { character: brom, state: MOVED, version: 1 },
      Save { character: cael, state: MOVED, version: 0 },
    ];
    assert_eq!(store.save(&Batch { zone: ZoneId(2), epoch, saves }).await?, 1);
    assert_eq!(
      store.load(&[brom, cael]).await?,
      [
        Saved { character: brom, zone: ZoneId(2), state: MOVED, version: 1 },
        Saved { character: cael, zone: ZoneId(1), state: START, version: 0 },
      ]
    );
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_batch_of_an_old_writer_or_of_no_zone_writes_nothing(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let [brom, cael] = world(&store).await;
    let old = store.claim(ZoneId(1)).await?;
    store.claim(ZoneId(1)).await?;
    let saves = vec![Save { character: brom, state: MOVED, version: 1 }];
    let mut refusals = Vec::new();
    for (zone, epoch) in [(ZoneId(1), old), (ZoneId(9), old)] {
      let refused = store.save(&Batch { zone, epoch, saves: saves.clone() }).await;
      refusals.push(refused.err().map(|error| error.to_string()));
    }
    assert_eq!(
      refusals,
      [
        Some("zone 1 is at epoch 2, and the batch is of epoch 1".to_owned()),
        Some("no zone has the id 9".to_owned()),
      ]
    );
    let saved = store.load(&[brom, cael]).await?;
    assert_eq!(
      saved.iter().map(|one| (one.state, one.version)).collect::<Vec<_>>(),
      [(START, 0); 2]
    );
    Ok(())
  }
}
