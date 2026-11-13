#!/bin/sh
# Reads the report that `sh script/local.sh bots <count>` printed, and fails when the run broke a rule of scale:
#
#   sh script/report.sh <report> <count> <shards> [<waiting> [<recover>]]
#
# The program of the bots exits with 0 when a bot fails, so this script reads its report. It fails when the report
# is missing, when a bot did not play or wait in the queue, or ended with a fault (a message that fails to decode is
# one), when a layer passed its cap, when a shard opened fewer layers than its players need, when the report shows
# fewer shards than <shards>, when the queue does not hold the <waiting> bots that must wait (0 by default), when the
# queue does not admit in the order of entry, does not give each waiting bot the place of its rank and does not renew
# it each 7 s, and, when <recover> is 1, when a bot that lost its link was not back in play within its bound or came
# back on another layer. It checks no tick time and no byte budget, which stage 2 of section 12 of
# docs/architecture.md proves.
set -eu

report=${1:?usage: sh script/report.sh <report> <count> <shards>}
count=${2:?usage: sh script/report.sh <report> <count> <shards>}
shards=${3:?usage: sh script/report.sh <report> <count> <shards>}
waiting=${4:-0}
recover=${5:-0}

awk -v want="$count" -v shards="$shards" -v waiting="$waiting" -v recover="$recover" '
  /^load test: / { found = 1; bots = $3; played = $5; queued = $7 }
  /^  [0-9]+ bots: / { faults += $1; print "fault: " $0 }
  /^shard [0-9]+: / { opened++ }
  /^budget (no layer over its cap|each shard opened the layers that its players need|the queue holds the bots that must wait|the queue admits in the order of entry|each waiting bot holds the place of its order of entry|each waiting bot gets its place at least each 7 s|each bot that lost its link played again|each bot played again within [0-9]+ s|each bot played again on its layer): / {
    lines++
    if ($NF != "ok") { print "error: " $0; bad = 1 }
  }
  END {
    if (!found) { print "error: the bots printed no report"; exit 1 }
    if (bots != want || played + queued != bots || queued != waiting || faults) {
      printf "error: %s of %s bots played, %s waited of %s that must, and %d ended with a fault\n", played, want, queued, waiting, faults
      bad = 1
    }
    expected = 6 + 3 * recover
    if (opened != shards || lines != expected) {
      printf "error: the report shows %d of %d shards, and %d of its %d lines of the budget on the layers, the queue and the recovery\n", opened, shards, lines, expected
      bad = 1
    }
    exit bad
  }' "$report"
