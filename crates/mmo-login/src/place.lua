-- The place of one account in the queue of its realm, which Redis runs as one atomic step.
-- KEYS: the queue and the admissions of the realm, and the queued mark, the admit mark, the resume mark and the
-- presence of the account.
-- ARGV: the account, 1 to enter the queue when the account has no place, the life of the marks in seconds, and the
-- window of the admissions in milliseconds.
-- The score of an account is the time of Redis in milliseconds, or one more than the score of the last account when
-- that is not lower, so accounts that enter in one millisecond, or after the clock went back, stand in the order of
-- their entry and not in the order of their ids.
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
if ARGV[2] == '1' and not redis.call('ZSCORE', KEYS[1], ARGV[1]) then
  local tail = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')
  local score = now
  if tail[2] and tonumber(tail[2]) >= now then
    score = tonumber(tail[2]) + 1
  end
  redis.call('ZADD', KEYS[1], score, ARGV[1])
end
local rank = redis.call('ZRANK', KEYS[1], ARGV[1])
if not rank then
  return {-2}
end
redis.call('SET', KEYS[3], 1, 'EX', ARGV[3])
redis.call('SET', KEYS[5], ARGV[1], 'EX', ARGV[3])
return {rank, redis.call('ZCARD', KEYS[1]), redis.call('ZCOUNT', KEYS[2], now - tonumber(ARGV[4]), '+inf')}
