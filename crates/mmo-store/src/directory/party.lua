-- A player joins a party or leaves its party, which Redis runs as one atomic step, so a party never stands on two
-- layers of one zone.
-- KEYS: the presence of the account.
-- ARGV: the prefix of the parties of the shard, the prefix of the presences, the account, the shard, the most members
-- of a party, and the party to join, or nothing to leave.
-- A player joins only when it plays in the shard and has no party, the party has room, and no member of the party is
-- on another layer of the zone of the player. A party with one member left ends. It gives 1 when it changed the
-- party, and else 0.
local at = redis.call('HMGET', KEYS[1], 'shard', 'zone', 'layer', 'party')
local party = ARGV[6]
if party then
  if at[1] ~= ARGV[4] or at[4] then
    return 0
  end
  local key = ARGV[1] .. party
  local members = redis.call('SMEMBERS', key)
  if #members >= tonumber(ARGV[5]) then
    return 0
  end
  for _, member in ipairs(members) do
    local other = redis.call('HMGET', ARGV[2] .. member, 'zone', 'layer')
    if other[1] == at[2] and other[2] ~= at[3] then
      return 0
    end
  end
  redis.call('SADD', key, ARGV[3])
  redis.call('HSET', KEYS[1], 'party', party)
  return 1
end
if not at[4] then
  return 0
end
local key = ARGV[1] .. at[4]
redis.call('SREM', key, ARGV[3])
redis.call('HDEL', KEYS[1], 'party')
local left = redis.call('SMEMBERS', key)
if #left < 2 then
  for _, member in ipairs(left) do
    redis.call('HDEL', ARGV[2] .. member, 'party')
  end
  redis.call('DEL', key)
end
return 1
