-- The first tables of the game. A migration only adds: a new column has a default, and a column goes
-- away only after no deployed process reads it.

CREATE TABLE accounts (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name text NOT NULL UNIQUE CHECK (name = lower(name)),
  password_hash text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE zones (
  id integer PRIMARY KEY,
  name text NOT NULL UNIQUE,
  epoch bigint NOT NULL DEFAULT 0
);

-- The position is in whole centimetres and the yaw in steps of 1/65536 of a turn, as the zone writer
-- holds them and the wire carries them, so a save and a load give the same state.
CREATE TABLE characters (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  account_id bigint NOT NULL REFERENCES accounts (id),
  name text NOT NULL UNIQUE,
  zone_id integer NOT NULL REFERENCES zones (id),
  x integer NOT NULL,
  y integer NOT NULL,
  z integer NOT NULL,
  yaw integer NOT NULL CHECK (yaw BETWEEN 0 AND 65535),
  hit_points integer NOT NULL,
  version bigint NOT NULL DEFAULT 0,
  saved_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX characters_account ON characters (account_id);
