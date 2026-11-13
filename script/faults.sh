#!/bin/sh
# Stage 6 of section 12 of docs/architecture.md, for one fault, on the stack that `sh script/local.sh start` made:
#
#   sh script/faults.sh <writer|gateway|keeper> <count> <log>
#
# It runs <count> bots in the background, waits until each of them plays, then ends one process of the stack with
# `sh script/local.sh fault`, and reads the report of the bots with `sh script/report.sh`:
#
#   writer    the layer writer of the layer of the bots stops. Each bot must play again on the same layer within 10 s of
#             its last snapshot.
#   gateway   the gateway stops. Each bot must connect again and play within 5 s of its last snapshot.
#   keeper    the keeper stops for 60 s. No bot may lose its link for more than 1 s, and each character that plays must
#             have a version in PostgreSQL that grew 25 s after the keeper came back, so no save is lost.
#
# <log> receives the output of the bots. The play lasts as long as the fault needs, and no run passes 500 bots.
set -eu

fault=${1:?usage: sh script/faults.sh <writer|gateway|keeper> <count> <log>}
count=${2:?usage: sh script/faults.sh <writer|gateway|keeper> <count> <log>}
log=${3:?usage: sh script/faults.sh <writer|gateway|keeper> <count> <log>}
here=$(cd "$(dirname "$0")" && pwd)
local="sh $here/local.sh"
down=0
case $fault in
  writer) bound=10 play=45 ;;
  gateway) bound=5 play=45 ;;
  keeper) bound=1 play=120 down=60 ;;
  *) echo "the fault is writer, gateway or keeper" >&2; exit 2 ;;
esac
[ "$count" -le 500 ] || { echo "no run of bots passes 500 bots" >&2; exit 2; }

# The characters that this run makes, or changes, are the ones of its bots: a stack keeps the characters of earlier runs.
$local versions >"$log.base"
MMO_BOTS_RECOVER_SECONDS=$bound MMO_BOTS_PLAY_SECONDS=$play MMO_BOTS_JOIN_SECONDS=60 \
  $local bots "$count" >"$log" 2>&1 &
bots=$!

# The directory counts the players of the layers: the fault comes 10 s after the last bot plays.
tries=0
until [ "$($local players)" -ge "$count" ]; do
  kill -0 "$bots" 2>/dev/null || { cat "$log"; echo "the bots ended before they played" >&2; exit 1; }
  tries=$((tries + 1))
  [ "$tries" -le 150 ] || { cat "$log"; echo "the bots did not play within 75 s" >&2; exit 1; }
  sleep 0.5
done
sleep 10

if [ "$fault" = keeper ]; then
  $local versions >"$log.before"
fi
$local fault "$fault" "$down"
if [ "$fault" = keeper ]; then
  # A writer sends its dirty characters again each 10 s once the keeper answers: 25 s is two rounds and more.
  sleep 25
  $local versions >"$log.after"
  # The characters of the bots are those whose version grew since the run began: the others did not play.
  stale=$(awk -v count="$count" 'FILENAME == ARGV[1] { base[$1] = $2; next } FILENAME == ARGV[2] { before[$1] = $2; next }
    { old = ($1 in base) ? base[$1] : -1; if ($2 > old) { played++; if ($2 <= before[$1]) print $1 } }
    END { if (played != count) print "only " played " characters of " count " saved" }' \
    "$log.base" "$log.before" "$log.after")
  [ -z "$stale" ] || { echo "error: the save of each of these characters did not grow after the keeper came back:" "$stale" >&2; failed=1; }
fi

wait "$bots" || true
cat "$log"
sh "$here/report.sh" "$log" "$count" 1 0 1 || failed=1
exit "${failed:-0}"
