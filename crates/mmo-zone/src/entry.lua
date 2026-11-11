-- Writes the entry of a zone in the directory, unless a writer of a newer epoch wrote it.
-- KEYS[1] is the key of the entry, ARGV[1] its life in milliseconds, ARGV[2] and ARGV[3] the field
-- of the epoch and the epoch of this writer, and the rest are more pairs of a field and a value.
-- It gives 0 when it wrote the entry, and else the epoch that it found.
local held = tonumber(redis.call('HGET', KEYS[1], ARGV[2]))
if held and held > tonumber(ARGV[3]) then
  return held
end
redis.call('HSET', KEYS[1], unpack(ARGV, 2))
redis.call('PEXPIRE', KEYS[1], ARGV[1])
return 0
