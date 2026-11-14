-- The drain of a layer, which Redis runs as one atomic step: the layer drains only when the other open layers of its
-- zone have room for all its players together, after the players of the layers of the zone that drain already.
-- KEYS: the layers, the draining layers and the caps of the realm.
-- ARGV: the zone and the number of the layer, the prefix of the writers of the realm, the channel of the directory,
-- the realm, and the prefix of the places of the realm.
-- The count of a layer is the count of its writer and each place that a placement gave and its writer did not take yet,
-- as for the placement, so the players that a placement sends to a layer need room too.
-- It gives 1 when the layer drains, also when it drained already, and else 0.
local cap = tonumber(redis.call('HGET', KEYS[3], 'layer_cap'))
if not cap then
  return redis.error_reply('NOCAP realm ' .. ARGV[5] .. ' has no layer_cap')
end
local own = ARGV[1] .. ':' .. ARGV[2]
if redis.call('SISMEMBER', KEYS[2], own) == 1 then
  return 1
end
if not redis.call('HGET', KEYS[1], own) then
  return 0
end
local time = redis.call('TIME')
local now = tonumber(time[1]) * 1000 + math.floor(tonumber(time[2]) / 1000)
local function held(field, players)
  redis.call('ZREMRANGEBYSCORE', ARGV[6] .. field, '-inf', now)
  return tonumber(players) + redis.call('ZCARD', ARGV[6] .. field)
end
local players, room = 0, 0
local listed = redis.call('HGETALL', KEYS[1])
for i = 1, #listed, 2 do
  local field = listed[i]
  if string.match(field, '^(-?%d+):%d+$') == ARGV[1] then
    local epoch = redis.call('HGET', ARGV[3] .. field, 'epoch')
    if field == own then
      players = held(field, listed[i + 1])
    elseif redis.call('SISMEMBER', KEYS[2], field) == 1 then
      room = room - held(field, listed[i + 1])
    elseif epoch and epoch ~= '0' then
      room = room + math.max(cap - held(field, listed[i + 1]), 0)
    end
  end
end
if room < players then
  return 0
end
redis.call('SADD', KEYS[2], own)
redis.call('PUBLISH', ARGV[4], ARGV[5] .. ':' .. own)
return 1
