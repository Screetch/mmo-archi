#!/bin/sh
# Runs a command in one of the slots of builds that this machine holds, and caps its jobs of cargo. Each worktree builds
# on its own, so with no slot the builds add up: 28 rustc processes took 13.8 GB, and the machine ran out of memory.
# A slot is a folder of FURB_BUILD_DIR that holds the pid of its holder.
# FURB_BUILD_SLOTS: the slots, one for each 12 GB of memory of the machine and at least 1. FURB_BUILD_JOBS: the cap of CARGO_BUILD_JOBS, 4. FURB_BUILD_WAIT: the seconds to
# wait for a slot, 1200, and then exit 75. FURB_BUILD_POLL: the seconds between two looks at the slots, 2.
[ $# -gt 0 ] || { echo "usage: sh script/build-slot.sh <command> [argument...]" >&2; exit 2; }
jobs=${FURB_BUILD_JOBS:-4}
case ${CARGO_BUILD_JOBS:-} in
  '' | *[!0-9]*) ;;
  *) [ "$CARGO_BUILD_JOBS" -ge "$jobs" ] || jobs=$CARGO_BUILD_JOBS ;;
esac
export CARGO_BUILD_JOBS=$jobs
# A command that this script started holds its slot already, so a build inside it takes no second one.
[ -z "${FURB_BUILD_SLOT:-}" ] || exec "$@"
# FURB_BUILD_MEM_GB replaces the memory that the machine reports.
memory() {
  if [ -n "${FURB_BUILD_MEM_GB:-}" ]; then echo "$FURB_BUILD_MEM_GB"
  elif bytes=$(sysctl -n hw.memsize 2>/dev/null); then echo $((bytes / 1073741824))
  else echo $(($(awk '/MemTotal/ { print $2 }' /proc/meminfo 2>/dev/null || echo 0) / 1048576))
  fi
}
slots=${FURB_BUILD_SLOTS:-$(($(memory) / 12))}
[ "$slots" -ge 1 ] || slots=1
limit=${FURB_BUILD_WAIT:-1200}
poll=${FURB_BUILD_POLL:-2}
dir=${FURB_BUILD_DIR:-/tmp/furb-build-slots-$(id -u)}
mkdir -p "$dir" || exit 1

# A holder writes its pid at once, so a folder that nobody touched for a minute belongs to a process that stopped.
old() { [ -n "$(find "$1" -prune -mmin +1 2>/dev/null)" ]; }
stale() {
  holder=$(cat "$1/pid" 2>/dev/null)
  if [ -n "$holder" ]; then ! kill -0 "$holder" 2>/dev/null; else old "$1"; fi
}

# The look at the slots runs under a guard that stands for milliseconds, so two waiters never free one stale slot twice.
take() {
  got=
  until mkdir "$dir/guard" 2>/dev/null; do
    if old "$dir/guard"; then rmdir "$dir/guard" 2>/dev/null; fi
    sleep 0.2
  done
  n=1
  while [ -z "$got" ] && [ "$n" -le "$slots" ]; do
    s=$dir/slot$n
    if mkdir "$s" 2>/dev/null; then got=$s
    elif stale "$s" && rm -rf "$s" && mkdir "$s" 2>/dev/null; then got=$s
    fi
    n=$((n + 1))
  done
  [ -z "$got" ] || echo $$ >"$got/pid"
  rmdir "$dir/guard"
}

start=$(date +%s)
said=$start
take
while [ -z "$got" ]; do
  now=$(date +%s)
  if [ $((now - start)) -ge "$limit" ]; then
    echo "build-slot: no slot of $slots freed in ${limit}s: run it again" >&2
    exit 75
  fi
  if [ $((now - said)) -ge 30 ]; then
    echo "build-slot: waiting $((now - start))s for a slot, held by pids $(cat "$dir"/slot*/pid 2>/dev/null | tr '\n' ' ')" >&2
    said=$now
  fi
  sleep "$poll"
  take
done
# A holder that a signal ends frees its slot, and one that nothing can catch leaves a pid that stale finds dead.
trap 'rm -rf "$got"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
FURB_BUILD_SLOT=$got "$@"
exit $?
