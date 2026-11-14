-- The lease of the writer of a layer and its count of players, which Redis runs as one atomic step, unless a writer of
-- a newer epoch holds the lease.
-- KEYS: the lease of the layer, the layers of the realm and the places of the layer.
-- ARGV: the life of the lease in milliseconds, the epoch and the address of the writer, the field of the layer, its
-- count of players, and the accounts of the places whose players the writer took. Their places end in the same step
-- as the count that holds them, so a placement never counts a player twice or not at all.
-- It gives 0 when it wrote, -1 when the layer closed, and else the epoch that the lease holds. A closed layer gets
-- no lease, so a late lease of the writer that closed it gives its number no writer.
if redis.call('HEXISTS', KEYS[2], ARGV[4]) == 0 then
  return -1
end
local held = tonumber(redis.call('HGET', KEYS[1], 'epoch'))
if held and held > tonumber(ARGV[2]) then
  return held
end
redis.call('HSET', KEYS[1], 'address', ARGV[3], 'epoch', ARGV[2])
redis.call('PEXPIRE', KEYS[1], ARGV[1])
redis.call('HSET', KEYS[2], ARGV[4], ARGV[5])
if #ARGV > 5 then
  redis.call('ZREM', KEYS[3], unpack(ARGV, 6))
end
return 0
