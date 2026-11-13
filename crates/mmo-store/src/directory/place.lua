-- The layer that a player enters in a zone of a shard, which Redis runs as one atomic step, as section 6 of
-- docs/architecture.md says: the layer of a member of its party, else the layer with the number that it leaves in
-- another zone, else the open layer with the most players and room, and else none, so the player waits.
-- KEYS: the layers, the draining layers and the caps of the shard, and the presence of the account.
-- ARGV: the zone, the account, the shard, the prefixes of the writers of the shard, of the presences and of the
-- parties of the shard, the channel of the directory, and when the player leaves a layer, its zone and its number.
-- It gives the number of the layer, or 0 when the player waits. When it gives 0 and each layer of the zone has a
-- writer, it adds the least free number of the zone to the layers, with a count of 0, for a writer to claim.
local zone, account, shard = ARGV[1], ARGV[2], ARGV[3]
local from_zone, from_number = ARGV[8], ARGV[9]
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
    if redis.call('EXISTS', ARGV[4] .. field) == 0 then
      unclaimed = true
    elseif redis.call('SISMEMBER', KEYS[2], field) == 0 then
      open[tonumber(n)] = tonumber(listed[i + 1])
    end
  end
end
local need, joined = 1, nil
local party = redis.call('HGET', KEYS[4], 'party')
if party then
  need = 0
  for _, member in ipairs(redis.call('SMEMBERS', ARGV[6] .. party)) do
    local at = redis.call('HMGET', ARGV[5] .. member, 'shard', 'zone', 'layer')
    if member == account or at[1] == shard then
      need = need + 1
      if member ~= account and at[2] == zone and not (from_zone == zone and at[3] == from_number) then
        joined = tonumber(at[3])
      end
    end
  end
  need = math.max(need, 1)
end
local function room(number, count)
  return open[number] ~= nil and open[number] + count <= cap
end
if joined then
  if room(joined, 1) then
    return joined
  end
  return 0
end
if from_zone and from_zone ~= zone and room(tonumber(from_number), need) then
  return tonumber(from_number)
end
local best
for number, count in pairs(open) do
  if count + need <= cap and (not best or count > open[best] or (count == open[best] and number < best)) then
    best = number
  end
end
if best then
  return best
end
if not unclaimed then
  local free = 1
  while used[free] do
    free = free + 1
  end
  redis.call('HSET', KEYS[1], zone .. ':' .. free, 0)
  redis.call('PUBLISH', ARGV[7], shard .. ':' .. zone .. ':' .. free)
end
return 0
