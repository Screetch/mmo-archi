use mmo_protocol::{Batch, CharacterId, Epoch, Layer, Save, ShardId, ZoneId};

use crate::{Error, Store};

/// One statement, so one transaction: it locks the row of the layer, and writes only when the
/// batch carries the epoch of that row, and only the characters of the shard of the batch. A layer
/// that no writer claimed holds the epoch 0.
const SAVE: &str = "WITH layer AS (SELECT epoch FROM layers \
  WHERE shard_id = $1 AND zone_id = $2 AND number = $3 FOR UPDATE), \
  written AS (UPDATE characters AS c \
  SET zone_id = $2, x = s.x, y = s.y, z = s.z, yaw = s.yaw, hit_points = s.hit_points, \
  version = s.version, saved_at = now() \
  FROM layer, unnest($5::bigint[], $6::integer[], $7::integer[], $8::integer[], \
  $9::integer[], $10::integer[], $11::bigint[]) AS s (id, x, y, z, yaw, hit_points, version) \
  WHERE layer.epoch = $4 AND c.id = s.id AND c.shard_id = $1 AND s.version > c.version \
  RETURNING c.id) \
  SELECT coalesce((SELECT epoch FROM layer), 0), (SELECT count(*) FROM written), \
  ARRAY(SELECT s.id FROM unnest($5::bigint[]) AS s (id) WHERE NOT EXISTS \
  (SELECT 1 FROM characters AS c WHERE c.id = s.id AND c.shard_id = $1) ORDER BY s.id)";

/// What the keeper wrote of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
  /// The count of the characters whose save the keeper wrote.
  pub characters: u64,
  /// The characters of the batch that no row of its shard holds, in the order of their ids. A
  /// layer writer plays a character of its shard alone, so each of them is a fault: the keeper
  /// writes no save of it, and its play is lost.
  pub strangers: Vec<CharacterId>,
}

/// The next epoch of a layer, which its first claim makes at 1.
const CLAIM: &str = "INSERT INTO layers (shard_id, zone_id, number, epoch) VALUES ($1, $2, $3, 1) \
  ON CONFLICT (shard_id, zone_id, number) DO UPDATE SET epoch = layers.epoch + 1 RETURNING epoch";

impl Store {
  /// Adds a zone to the world.
  pub async fn add_zone(&self, zone: ZoneId, name: &str) -> Result<(), Error> {
    sqlx::query("INSERT INTO zones (id, name) VALUES ($1, $2)")
      .bind(zone.0)
      .bind(name)
      .execute(&self.pool)
      .await?;
    Ok(())
  }

  /// Gives the next epoch of `layer` of `shard` to the writer that claims it, so the keeper
  /// refuses each batch of the writers before it.
  pub async fn claim(&self, shard: ShardId, layer: Layer) -> Result<Epoch, Error> {
    sqlx::query_scalar(CLAIM)
      .bind(i64::from(shard.0))
      .bind(layer.zone.0)
      .bind(i64::from(layer.number))
      .fetch_one(&self.pool)
      .await
      .map(Epoch)
      .map_err(|error| match error {
        sqlx::Error::Database(ref found) if found.is_foreign_key_violation() => {
          Error::NoZone { zone: layer.zone }
        }
        error => Error::Database(error),
      })
  }

  /// Writes `batch` in one transaction, and gives what it wrote.
  ///
  /// It refuses a batch whose epoch is not the last one of its layer. It writes a save only when
  /// its version is greater than the saved one and its character belongs to the shard of the
  /// batch, so an old copy of a character never writes over a newer one, and it moves each
  /// character that it writes to the zone of the batch, as a hand-off needs. It names each
  /// character of the batch that no row of the shard holds in [`Written::strangers`].
  pub async fn save(&self, batch: &Batch) -> Result<Written, Error> {
    let column = |of: fn(&Save) -> i32| batch.saves.iter().map(of).collect::<Vec<i32>>();
    let (held, written, strangers): (i64, i64, Vec<i64>) = sqlx::query_as(SAVE)
      .bind(i64::from(batch.shard.0))
      .bind(batch.layer.zone.0)
      .bind(i64::from(batch.layer.number))
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
    match Epoch(held) {
      held if held == batch.epoch => Ok(Written {
        characters: written.unsigned_abs(),
        strangers: strangers.into_iter().map(CharacterId).collect(),
      }),
      held => {
        Err(Error::Epoch { shard: batch.shard, layer: batch.layer, held, given: batch.epoch })
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use mmo_protocol::{
    AccountId, Angle, Batch, CharacterId, Epoch, Layer, Password, Position, Save, ShardId, State,
    ZoneId,
  };
  use sqlx::PgPool;

  use super::Written;
  use crate::{Error, NewCharacter, Saved, Store};

  const START: State =
    State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(0), hit_points: 100 };
  const MOVED: State =
    State { position: Position { x: 102_400, y: 250, z: -3 }, yaw: Angle(16384), hit_points: 7 };
  const SHARD: ShardId = ShardId(1);

  const fn on(zone: i32, number: u32) -> Layer {
    Layer { zone: ZoneId(zone), number }
  }

  const fn new(account: AccountId, shard: ShardId, name: &str) -> NewCharacter<'_> {
    NewCharacter { account, shard, name, zone: ZoneId(1), state: START }
  }

  const fn written(characters: u64, strangers: Vec<CharacterId>) -> Written {
    Written { characters, strangers }
  }

  /// The meadow and the forest, Brom and Cael in shard 1, and Eda in shard 2, all in the meadow.
  async fn world(store: &Store) -> [CharacterId; 3] {
    store.add_zone(ZoneId(1), "meadow").await.expect("a new zone");
    store.add_zone(ZoneId(2), "forest").await.expect("a new zone");
    let account = store.create_account("alice", Password("one".to_owned())).await.expect("one");
    let mut made = Vec::new();
    for (shard, name) in [(SHARD, "Brom"), (SHARD, "Cael"), (ShardId(2), "Eda")] {
      made.push(store.create_character(&new(account, shard, name)).await.expect("a character"));
    }
    made.try_into().expect("three characters")
  }

  #[sqlx::test(migrations = false)]
  async fn each_claim_gives_the_next_epoch_of_its_layer_alone(pool: PgPool) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    store.add_zone(ZoneId(1), "meadow").await?;
    let mut epochs = Vec::new();
    for (shard, layer) in
      [(SHARD, on(1, 1)), (SHARD, on(1, 1)), (SHARD, on(1, 2)), (ShardId(2), on(1, 1))]
    {
      epochs.push(store.claim(shard, layer).await?);
    }
    assert_eq!(epochs, [Epoch(1), Epoch(2), Epoch(1), Epoch(1)]);
    let refused = store.claim(SHARD, on(9, 1)).await.err().map(|error| error.to_string());
    assert_eq!(refused.as_deref(), Some("no zone has the id 9"));
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_batch_writes_each_newer_save_and_moves_it_to_its_zone(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let [brom, cael, _] = world(&store).await;
    let epoch = store.claim(SHARD, on(2, 3)).await?;
    let saves = vec![
      Save { character: brom, state: MOVED, version: 1 },
      Save { character: cael, state: MOVED, version: 0 },
    ];
    let batch = Batch { shard: SHARD, layer: on(2, 3), epoch, saves };
    assert_eq!(store.save(&batch).await?, written(1, Vec::new()));
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
  async fn a_batch_of_an_old_writer_or_of_an_unclaimed_layer_writes_nothing(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let [brom, cael, _] = world(&store).await;
    let old = store.claim(SHARD, on(1, 1)).await?;
    store.claim(SHARD, on(1, 1)).await?;
    let saves = vec![Save { character: brom, state: MOVED, version: 1 }];
    let mut answers = Vec::new();
    for (layer, epoch) in [(on(1, 1), old), (on(1, 2), old)] {
      let answer = store.save(&Batch { shard: SHARD, layer, epoch, saves: saves.clone() }).await;
      answers.push(answer.err().map(|error| error.to_string()));
    }
    assert_eq!(
      answers,
      [
        Some("layer 1:1 of shard 1 is at epoch 2, and the batch is of epoch 1".to_owned()),
        Some("layer 1:2 of shard 1 is at epoch 0, and the batch is of epoch 1".to_owned()),
      ]
    );
    let saved = store.load(&[brom, cael]).await?;
    assert_eq!(
      saved.iter().map(|one| (one.state, one.version)).collect::<Vec<_>>(),
      [(START, 0); 2]
    );
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn a_batch_of_each_shard_writes_its_characters_alone_and_names_each_other(
    pool: PgPool,
  ) -> Result<(), Error> {
    let store = Store::migrate(pool).await?;
    let [brom, cael, eda] = world(&store).await;
    let nobody = CharacterId(i64::MAX);
    let save = |character| Save { character, state: MOVED, version: 1 };
    let mut answers = Vec::new();
    for (shard, saves) in [
      (SHARD, vec![save(eda), save(brom)]),
      (ShardId(2), vec![save(nobody), save(cael), save(eda)]),
    ] {
      let epoch = store.claim(shard, on(2, 1)).await?;
      answers.push(store.save(&Batch { shard, layer: on(2, 1), epoch, saves }).await?);
    }
    assert_eq!(answers, [written(1, vec![eda]), written(1, vec![cael, nobody])]);
    assert_eq!(
      store.load(&[brom, cael, eda]).await?,
      [
        Saved { character: brom, zone: ZoneId(2), state: MOVED, version: 1 },
        Saved { character: cael, zone: ZoneId(1), state: START, version: 0 },
        Saved { character: eda, zone: ZoneId(2), state: MOVED, version: 1 },
      ]
    );
    Ok(())
  }
}
