-- The layer that a player enters in a zone of a shard, which Redis runs as one atomic step, as section 6 of
-- docs/architecture.md says: the layer with the number that it leaves in another zone, else the open layer with the
-- most players and room, and else none, so the player waits.
-- KEYS: the layers, the draining layers and the caps of the shard.
-- ARGV: the zone, the account, the shard, the prefix of the writers of the shard, the channel of the directory, the
-- prefix of the places of the shard and their life in milliseconds, and when the player leaves a layer, its zone and
-- its number.
-- The count of a layer is the count of its writer and each place that a placement gave and its writer did not take yet,
-- so placements that come faster than the writer writes its count never pass the cap. The layer that it gives holds
-- the place of the account until the writer takes the player, or until the place ends.
-- A layer whose lease is absent, or holds the epoch 0 of a claim before the keeper answered, waits for its writer and
-- takes no player. It gives the number of the layer, or 0 when the player waits. When it gives 0 and no layer of the
-- zone waits for its writer, it adds the least free number of the zone to the layers, with a count of 0, for a writer
-- to claim.
local zone, account, shard = ARGV[1], ARGV[2], ARGV[3]
local from_zone, from_number = ARGV[8], ARGV[9]
local time = redis.call('TIME')
local now = tonumber(time[1]) * 1000 + math.floor(tonumber(time[2]) / 1000)
local cap = tonumber(redis.call('HGET', KEYS[3], 'layer_cap'))
if not cap then
  return redis.error_reply('NOCAP shard ' .. shard .. ' has no layer_cap')
end
local open, used, unclaimed = {}, {}, false
local listed = redis.call('HGETALL', KEYS[1])
for i = 1, #listed, 2 do
  local field = listed[i]
  local z, n = string.match(field, '^(-?%d+):(%d+)$')
  if z == zone then
    used[tonumber(n)] = true
    local epoch = redis.call('HGET', ARGV[4] .. field, 'epoch')
    if not epoch or epoch == '0' then
      unclaimed = true
    elseif redis.call('SISMEMBER', KEYS[2], field) == 0 then
      redis.call('ZREMRANGEBYSCORE', ARGV[6] .. field, '-inf', now)
      open[tonumber(n)] = tonumber(listed[i + 1]) + redis.call('ZCARD', ARGV[6] .. field)
    end
  end
end
local function give(number)
  redis.call('ZADD', ARGV[6] .. zone .. ':' .. number, now + tonumber(ARGV[7]), account)
  return number
end
local kept = from_zone and from_zone ~= zone and open[tonumber(from_number)]
if kept and kept < cap then
  return give(tonumber(from_number))
end
local best
for number, count in pairs(open) do
  if count < cap and (not best or count > open[best] or (count == open[best] and number < best)) then
    best = number
  end
end
if best then
  return give(best)
end
if not unclaimed then
  local free = 1
  while used[free] do
    free = free + 1
  end
  redis.call('HSET', KEYS[1], zone .. ':' .. free, 0)
  redis.call('PUBLISH', ARGV[5], shard .. ':' .. zone .. ':' .. free)
end
return 0
