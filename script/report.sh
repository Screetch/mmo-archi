#!/bin/sh
# Reads the report that `sh script/local.sh bots <count>` printed, and fails when the run broke a rule of scale:
#
#   sh script/report.sh <report> <count> <shards>
#
# The program of the bots exits with 0 when a bot fails, so this script reads its report. It fails when the report
# is missing, when a bot did not play or ended with a fault (a message that fails to decode is one), when a layer
# passed its cap, when a shard opened fewer layers than its players need, and when the report shows fewer shards than
# <shards>. It checks no tick time and no byte budget, which stage 2 of section 12 of docs/architecture.md proves.
set -eu

report=${1:?usage: sh script/report.sh <report> <count> <shards>}
count=${2:?usage: sh script/report.sh <report> <count> <shards>}
shards=${3:?usage: sh script/report.sh <report> <count> <shards>}

awk -v want="$count" -v shards="$shards" '
  /^load test: / { found = 1; bots = $3; played = $5 }
  /^  [0-9]+ bots: / { faults += $1; print "fault: " $0 }
  /^shard [0-9]+: / { opened++ }
  /^budget (no layer over its cap|each shard opened the layers that its players need): / {
    lines++
    if ($NF != "ok") { print "error: " $0; bad = 1 }
  }
  END {
    if (!found) { print "error: the bots printed no report"; exit 1 }
    if (bots != want || played != bots || faults) {
      printf "error: %s of %s bots played, and %d ended with a fault\n", played, want, faults
      bad = 1
    }
    if (opened != shards || lines != 2) {
      printf "error: the report shows %d of %d shards, and %d of its 2 lines on the layers\n", opened, shards, lines
      bad = 1
    }
    exit bad
  }' "$report"
