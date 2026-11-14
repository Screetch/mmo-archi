-- The close of a layer, which Redis runs as one atomic step: it removes the layer and its lease when the layer holds
-- no player and its lease holds the epoch of the writer, so the number of the layer is free again.
-- A layer that holds the place of a player that a placement gave does not close.
-- KEYS: the layers and the draining layers of the realm, and the lease and the places of the layer.
-- ARGV: the field of the layer, the epoch of the writer, the channel of the directory and the name of the layer on it.
-- It gives 1 when it closed the layer, and else 0.
local time = redis.call('TIME')
redis.call('ZREMRANGEBYSCORE', KEYS[4], '-inf', tonumber(time[1]) * 1000)
if redis.call('HGET', KEYS[1], ARGV[1]) ~= '0' or redis.call('HGET', KEYS[3], 'epoch') ~= ARGV[2]
  or redis.call('ZCARD', KEYS[4]) > 0 then
  return 0
end
redis.call('HDEL', KEYS[1], ARGV[1])
redis.call('SREM', KEYS[2], ARGV[1])
redis.call('DEL', KEYS[3])
redis.call('PUBLISH', ARGV[3], ARGV[4])
return 1
