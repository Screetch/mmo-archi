-- The lease of the writer of a layer and its count of players, which Redis runs as one atomic step, unless a writer of
-- a newer epoch holds the lease.
-- KEYS: the lease of the layer and the layers of the shard.
-- ARGV: the life of the lease in milliseconds, the epoch and the address of the writer, the field of the layer and its
-- count of players.
-- It gives 0 when it wrote, and else the epoch that the lease holds.
local held = tonumber(redis.call('HGET', KEYS[1], 'epoch'))
if held and held > tonumber(ARGV[2]) then
  return held
end
redis.call('HSET', KEYS[1], 'address', ARGV[3], 'epoch', ARGV[2])
redis.call('PEXPIRE', KEYS[1], ARGV[1])
if redis.call('HEXISTS', KEYS[2], ARGV[4]) == 1 then
  redis.call('HSET', KEYS[2], ARGV[4], ARGV[5])
end
return 0
