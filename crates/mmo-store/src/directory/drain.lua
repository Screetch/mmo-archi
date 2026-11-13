-- The drain of a layer, which Redis runs as one atomic step: the layer drains only when the other open layers of its
-- zone have room for all its players together.
-- KEYS: the layers, the draining layers and the caps of the shard.
-- ARGV: the zone and the number of the layer, the prefix of the writers of the shard, the channel of the directory
-- and the shard.
-- It gives 1 when the layer drains now, and else 0.
local cap = tonumber(redis.call('HGET', KEYS[3], 'layer_cap'))
if not cap then
  return redis.error_reply('NOCAP shard ' .. ARGV[5] .. ' has no layer_cap')
end
local own = ARGV[1] .. ':' .. ARGV[2]
local players = tonumber(redis.call('HGET', KEYS[1], own))
if not players then
  return 0
end
local room = 0
local listed = redis.call('HGETALL', KEYS[1])
for i = 1, #listed, 2 do
  local field = listed[i]
  local zone = string.match(field, '^(-?%d+):%d+$')
  local epoch = redis.call('HGET', ARGV[3] .. field, 'epoch')
  if zone == ARGV[1] and field ~= own and epoch and epoch ~= '0'
    and redis.call('SISMEMBER', KEYS[2], field) == 0 then
    room = room + math.max(cap - tonumber(listed[i + 1]), 0)
  end
end
if room < players then
  return 0
end
redis.call('SADD', KEYS[2], own)
redis.call('PUBLISH', ARGV[4], ARGV[5] .. ':' .. own)
return 1
