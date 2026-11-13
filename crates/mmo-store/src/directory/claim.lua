-- The claim of an idle writer, which Redis runs as one atomic step, so two writers never take one layer.
-- KEYS: the layers of the shard.
-- ARGV: the prefix of the writers of the shard, the address of the writer, the life of its lease in milliseconds,
-- the channel of the directory and the shard.
-- It gives the zone and the number of the layer with no writer that has the lowest zone, then the lowest number, and
-- writes the lease of that layer with the address and the epoch 0, which the epoch of the keeper then replaces. It
-- gives nil when each layer has a writer.
local best
for _, field in ipairs(redis.call('HKEYS', KEYS[1])) do
  local zone, number = string.match(field, '^(-?%d+):(%d+)$')
  zone, number = tonumber(zone), tonumber(number)
  if redis.call('EXISTS', ARGV[1] .. field) == 0
    and (not best or zone < best[1] or (zone == best[1] and number < best[2])) then
    best = {zone, number}
  end
end
if not best then
  return false
end
local field = best[1] .. ':' .. best[2]
redis.call('HSET', ARGV[1] .. field, 'address', ARGV[2], 'epoch', 0)
redis.call('PEXPIRE', ARGV[1] .. field, ARGV[3])
redis.call('PUBLISH', ARGV[4], ARGV[5] .. ':' .. field)
return best
