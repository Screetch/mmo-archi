-- The map of the world. Each zone stands on one square of 1024 m of the map: its column grows toward the east, along x,
-- and its row toward the north, along z. Two zones on next squares share a border, which a player crosses with the
-- hand-off. A zone with no square, as each zone that stood before the map, has no neighbour.

ALTER TABLE zones ADD COLUMN map_column integer, ADD COLUMN map_row integer;

ALTER TABLE zones ADD CONSTRAINT zones_square UNIQUE (map_column, map_row);
