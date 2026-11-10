-- The place of one account in the queue of its world, which Redis runs as one atomic step.
-- KEYS: the queue and the admissions of the world, and the queued mark, the admit mark, the resume mark and the
-- presence of the account.
-- ARGV: the account, 1 to enter the queue when the account has no place, the life of the marks in seconds, and the
-- window of the admissions in milliseconds.
-- It gives {-3} when the account plays already, {-1} when it has a slot, {-2} when it has no place, and else its
-- rank, the length of the queue and the count of the admissions in the window.
if redis.call('EXISTS', KEYS[6]) == 1 then
  return {-3}
end
if redis.call('EXISTS', KEYS[4]) == 1 then
  return {-1}
end
local time = redis.call('TIME')
local now = tonumber(time[1]) * 1000 + math.floor(tonumber(time[2]) / 1000)
if ARGV[2] == '1' then
  redis.call('ZADD', KEYS[1], 'NX', now, ARGV[1])
end
local rank = redis.call('ZRANK', KEYS[1], ARGV[1])
if not rank then
  return {-2}
end
redis.call('SET', KEYS[3], 1, 'EX', ARGV[3])
redis.call('SET', KEYS[5], ARGV[1], 'EX', ARGV[3])
return {rank, redis.call('ZCARD', KEYS[1]), redis.call('ZCOUNT', KEYS[2], now - tonumber(ARGV[4]), '+inf')}
