-- The shards and the layers. A shard is one independent world, and each character belongs to one. The first shard is
-- 1, so each character that stood before the shards belongs to it. A layer is one copy of a zone in a shard, and its
-- epoch replaces the epoch of its zone: the column epoch of zones stays until no deployed process reads it.

ALTER TABLE characters ADD COLUMN shard_id integer NOT NULL DEFAULT 1;

CREATE TABLE layers (
  shard_id integer NOT NULL,
  zone_id integer NOT NULL REFERENCES zones (id),
  number integer NOT NULL CHECK (number > 0),
  epoch bigint NOT NULL DEFAULT 0,
  PRIMARY KEY (shard_id, zone_id, number)
);
