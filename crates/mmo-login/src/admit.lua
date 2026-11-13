-- The admission of one world, which Redis runs as one atomic step, so many copies of the login service can run it.
-- KEYS: the queue, the admitted accounts and the admissions of the world, and its caps.
-- ARGV: the cap of the world, the most admissions in one second, the slot and its extension in milliseconds, the
-- window of the admissions in milliseconds, the life of an admit mark in seconds, the prefixes of the presence, of
-- the queued mark and of the admit mark, and the cap of a layer of the world.
-- It writes the caps of the world first, so the directory reads them, and a Redis that lost them holds them again.
redis.call('HSET', KEYS[4], 'cap', ARGV[1], 'layer_cap', ARGV[10])
local time = redis.call('TIME')
local now = tonumber(time[1]) * 1000 + math.floor(tonumber(time[2]) / 1000)
for _, account in ipairs(redis.call('ZRANGEBYSCORE', KEYS[2], '-inf', now)) do
  if redis.call('EXISTS', ARGV[7] .. account) == 1 then
    redis.call('ZADD', KEYS[2], now + tonumber(ARGV[4]), account)
  else
    redis.call('ZREM', KEYS[2], account)
  end
end
redis.call('ZREMRANGEBYSCORE', KEYS[3], '-inf', now - tonumber(ARGV[5]))
local free = math.min(
  tonumber(ARGV[1]) - redis.call('ZCARD', KEYS[2]),
  tonumber(ARGV[2]) - redis.call('ZCOUNT', KEYS[3], now - 999, '+inf'))
local admitted = 0
while admitted < free do
  local head = redis.call('ZPOPMIN', KEYS[1], free - admitted)
  if #head == 0 then
    break
  end
  for i = 1, #head, 2 do
    local account = head[i]
    if redis.call('EXISTS', ARGV[8] .. account) == 1 then
      redis.call('ZADD', KEYS[2], now + tonumber(ARGV[3]), account)
      redis.call('SET', ARGV[9] .. account, 1, 'EX', ARGV[6])
      redis.call('ZADD', KEYS[3], now, now .. ':' .. account)
      admitted = admitted + 1
    end
  end
end
return admitted
