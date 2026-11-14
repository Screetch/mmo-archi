-- The realm of a character and of a layer. The migration of the layers gave the column its first name, `shard_id`.
-- The primary key of `layers` follows the column, so it keeps its columns in the same order.

ALTER TABLE characters RENAME COLUMN shard_id TO realm_id;

ALTER TABLE layers RENAME COLUMN shard_id TO realm_id;
