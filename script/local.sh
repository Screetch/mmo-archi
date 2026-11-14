#!/bin/sh
# The whole stack of the game on this machine, with one command each to start it, to check it and to stop it:
#
#   sh script/local.sh start    builds the servers, then starts PostgreSQL, Redis, the keeper, one login service and
#                               one pool of layer writers for each realm, and one gateway, and prints the addresses
#                               that a client uses.
#   sh script/local.sh check    prints the list of the realms that a client gets, makes an account and a character
#                               through the login service of the realm 1, links to the gateway, and waits for the first
#                               snapshot of the zone, with the headless binary mmo-check.
#   sh script/local.sh status   says which processes of the stack run.
#   sh script/local.sh stop     ends the processes of the stack, and no other process.
#   sh script/local.sh fault <gateway|keeper|writer> [seconds]
#                               ends one process with KILL, as a crash does, waits that many seconds (0 by default),
#                               then starts it again as the process manager does. `writer` ends the writer of the layer
#                               1 of the zone 1 of the realm 1, which an idle writer of its pool takes over.
#   sh script/local.sh versions prints the id and the version of the save of each character of the realm 1.
#   sh script/local.sh players  prints the count of the players that the layers of the zone 1 of the realm 1 hold.
#
# The stack lives in one folder, `$TMPDIR/mmo-local` (`MMO_LOCAL_DIR` replaces it): the data of PostgreSQL, the TLS
# material, and for each process its log and the file of its pid. `stop` reads those pids and ends them alone, as
# AGENTS.md asks, and it ends a pid only when the command of that pid is still the program of the stack.
#
# The stores run on the ports 55432 and 56379, not on the ports of compose.yaml, so a PostgreSQL or a Redis that
# already runs on this machine keeps its data and its ports. `MMO_LOCAL_PG_PORT`, `MMO_LOCAL_REDIS_PORT`,
# `MMO_LOCAL_KEEPER_PORT`, `MMO_LOCAL_LOGIN_PORT`, `MMO_LOCAL_ZONE_PORT` and `MMO_LOCAL_GATEWAY_PORT` change a port:
# the layer writers take the ports from `MMO_LOCAL_ZONE_PORT` up, and the login service of the realm n the port of
# `MMO_LOCAL_LOGIN_PORT` plus 100 (n - 1). `MMO_LOCAL_REALMS` sets the count of realms, `MMO_LOCAL_REALM_CAP` the cap of
# each realm and `MMO_LOCAL_LAYER_CAP` the cap of a layer, which are small, so that a few bots open many layers.
# `MMO_LOCAL_ZONES` sets the count of zones, 1 by default: the zone n stands in the column n - 1 of the first row of the
# map, so each zone shares a border with the zone before it and the zone after it. The zone 1 is the meadow, where the
# new characters start. The pool of each realm holds one writer for each zone, one for each layer that the cap of the
# realm fills, and 2 more.
# `MMO_LOCAL_ATTRIBUTES` gives the attributes of each realm, one word for each in its order, as region:language:rules,
# where rules is normal, player-against-player or role-play: `Europe:French:normal America:English:role-play`. A realm
# with no word takes Europe:French:normal, and the realm n takes the name Realm<n>.
# The stores need initdb, postgres, pg_isready, psql, redis-server and redis-cli: `MMO_LOCAL_PG_BIN` names the folder
# of the programs of PostgreSQL when it is not in the PATH. The TLS material needs openssl.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=${TMPDIR:-/tmp}
state=${MMO_LOCAL_DIR:-${tmp%/}/mmo-local}
pg_port=${MMO_LOCAL_PG_PORT:-55432}
redis_port=${MMO_LOCAL_REDIS_PORT:-56379}
keeper_port=${MMO_LOCAL_KEEPER_PORT:-54433}
login_port=${MMO_LOCAL_LOGIN_PORT:-54434}
# The layer writers take the ports from this one up, one each.
zone_port=${MMO_LOCAL_ZONE_PORT:-54440}
gateway_port=${MMO_LOCAL_GATEWAY_PORT:-54436}
realms=${MMO_LOCAL_REALMS:-1}
realm_cap=${MMO_LOCAL_REALM_CAP:-20}
layer_cap=${MMO_LOCAL_LAYER_CAP:-5}
zones=${MMO_LOCAL_ZONES:-1}
attributes=${MMO_LOCAL_ATTRIBUTES:-}
# The pool of the layer writers of each realm, as section 6 of docs/architecture.md says: one for each zone, one for
# each layer that the cap of the realm fills, and 2 for the recovery.
pool=$((zones + (realm_cap + layer_cap - 1) / layer_cap + 2))
# debug or release: the numbers of docs/architecture.md come from release builds.
profile=${MMO_LOCAL_PROFILE:-debug}
bin=${CARGO_TARGET_DIR:-$root/target}/$profile
game=mmo
database=postgres://postgres:postgres@127.0.0.1:$pg_port/$game
redis=redis://127.0.0.1:$redis_port
tls=$state/tls
PATH=${MMO_LOCAL_PG_BIN:+$MMO_LOCAL_PG_BIN:}$PATH

say() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*"; }

die() {
  say "error: $*" >&2
  # The logs of a run that did not start yet are the logs of an older run.
  for log in "$state"/*.log; do
    [ -n "${started:-}" ] && [ -f "$log" ] && { printf -- '--- %s\n' "$log" >&2; tail -n 5 "$log" >&2; }
  done
  exit 1
}

need() {
  for program in "$@"; do
    command -v "$program" >/dev/null 2>&1 || die "$program is not in the PATH"
  done
}

# The program that a process of the stack runs, which the command of its pid must hold.
program() {
  case $1 in
    postgres) echo postgres ;;
    redis) echo redis-server ;;
    zone*) echo mmo-zone ;;
    login*) echo mmo-login ;;
    *) echo "mmo-$1" ;;
  esac
}

# Succeeds when the pid file of the process holds the pid of a live process that runs its program.
alive() {
  [ -f "$state/$1.pid" ] || return 1
  command=$(ps -p "$(cat "$state/$1.pid")" -o command= 2>/dev/null) || return 1
  case $command in
    *"$(program "$1")"*) return 0 ;;
  esac
  return 1
}

# Starts a process in the background, with its output in its log and its pid in its file.
launch() {
  name=$1
  shift
  nohup "$@" >"$state/$name.log" 2>&1 </dev/null &
  echo $! >"$state/$name.pid"
}

# Runs a function once each half second until it succeeds, for at most the given count of tries.
wait_for() {
  what=$1
  tries=$2
  shift 2
  say "waiting for $what"
  while ! "$@"; do
    tries=$((tries - 1))
    [ "$tries" -gt 0 ] || die "$what did not come in time"
    sleep 0.5
  done
}

stores_up() {
  alive postgres || die "PostgreSQL stopped, so the port $pg_port holds a server of another stack"
  alive redis || die "Redis stopped"
  pg_isready -q -h 127.0.0.1 -p "$pg_port" &&
    [ "$(redis-cli -p "$redis_port" ping 2>/dev/null)" = PONG ]
}

# The game keeps its tables in the database `mmo`. The tests of the workspace read the database `postgres` of their
# server and need it empty, so a server of the stack serves them too.
game_made() {
  psql -q -h 127.0.0.1 -p "$pg_port" -U postgres -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$game'" |
    grep -q 1 || psql -q -h 127.0.0.1 -p "$pg_port" -U postgres -d postgres -c "CREATE DATABASE $game" >/dev/null
}

# The keeper migrates the database. The migrations make no zone, so the script adds the zones once the table exists,
# each on its square of the map, and puts a zone of an older run on its square.
zones_made() {
  alive keeper || die "the keeper stopped"
  psql -q -h 127.0.0.1 -p "$pg_port" -U postgres -d "$game" -c \
    "INSERT INTO zones (id, name, map_column, map_row)
     SELECT n, CASE WHEN n = 1 THEN 'meadow' ELSE 'zone' || n END, n - 1, 0 FROM generate_series(1, $zones) AS n
     ON CONFLICT (id) DO UPDATE SET map_column = excluded.map_column, map_row = excluded.map_row" >/dev/null 2>&1
}

# The names of the processes of one kind, `prefix1` up to `prefix<count>`, or those of the pid files of a stack that runs.
names() {
  if [ "${3:-}" = held ]; then
    for file in "$state/$1"*.pid; do
      [ -f "$file" ] && basename "$file" .pid
    done
    return 0
  fi
  number=1
  while [ "$number" -le "$2" ]; do
    echo "$1$number"
    number=$((number + 1))
  done
}

# The layer writers of the pool of each realm, zone1 and up, and the login service of each realm.
writers() { names zone $((pool * realms)) "${1:-}"; }
logins() { names login "$realms" "${1:-}"; }

# The port of the login service of the realm $1: the first takes MMO_LOCAL_LOGIN_PORT, and each next one 100 more.
login_at() { echo $((login_port + 100 * ($1 - 1))); }

# The attributes of the realm $1 as region:language:rules: the word $1 of MMO_LOCAL_ATTRIBUTES, or the default.
attributes() {
  printf '%s\n' "$attributes" | awk -v n="$1" '{ print (NF >= n ? $n : "Europe:French:normal") }'
}

# An authority and one certificate for each server of the stack, for local work only. The certificate holds the names
# localhost and 127.0.0.1, and serves as a server and as a client, so each process shows it to its peers.
make_tls() {
  if [ -f "$tls/leaf.pem" ] && openssl x509 -checkend 86400 -noout -in "$tls/leaf.pem" >/dev/null 2>&1; then
    return
  fi
  say "making the TLS material in $tls"
  rm -rf "$tls"
  mkdir -p "$tls"
  cat >"$tls/ca.cnf" <<'EOF'
[req]
distinguished_name = dn
prompt = no
x509_extensions = authority
[dn]
CN = mmo local authority
[authority]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign
EOF
  cat >"$tls/leaf.cnf" <<'EOF'
[leaf]
basicConstraints = CA:FALSE
keyUsage = digitalSignature
extendedKeyUsage = serverAuth,clientAuth
subjectAltName = DNS:localhost,IP:127.0.0.1
EOF
  ssl() {
    openssl "$@" >>"$tls/openssl.log" 2>&1 || die "openssl $1 failed, see $tls/openssl.log"
  }
  ssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$tls/ca.key"
  ssl req -x509 -new -key "$tls/ca.key" -sha256 -days 30 -config "$tls/ca.cnf" -out "$tls/ca.pem"
  ssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$tls/leaf.key"
  ssl req -new -key "$tls/leaf.key" -subj /CN=localhost -config "$tls/ca.cnf" -out "$tls/leaf.csr"
  ssl x509 -req -in "$tls/leaf.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" -CAcreateserial -days 30 -sha256 \
    -extfile "$tls/leaf.cnf" -extensions leaf -out "$tls/leaf.pem"
  chmod 600 "$tls/ca.key" "$tls/leaf.key"
}

launch_keeper() {
  launch keeper env KEEPER_ADDRESS="127.0.0.1:$keeper_port" DATABASE_URL="$database" \
    KEEPER_CERTIFICATE="$tls/leaf.pem" KEEPER_KEY="$tls/leaf.key" KEEPER_AUTHORITY="$tls/ca.pem" "$bin/mmo-keeper"
}

launch_gateway() {
  launch gateway env GATEWAY_ADDRESS="127.0.0.1:$gateway_port" GATEWAY_CERTIFICATE="$tls/leaf.pem" \
    GATEWAY_KEY="$tls/leaf.key" GATEWAY_LINK_CERTIFICATE="$tls/leaf.pem" GATEWAY_LINK_KEY="$tls/leaf.key" \
    GATEWAY_AUTHORITY="$tls/ca.pem" REDIS_URL="$redis" "$bin/mmo-gateway"
}

# The layer writer of the pool at the place $1, counted from 0: its realm and its port come from that place.
launch_writer() {
  port=$((zone_port + $1))
  launch "zone$(($1 + 1))" env ZONE_REALM=$(($1 / pool + 1)) ZONE_ADDRESS="127.0.0.1:$port" \
    ZONE_PUBLIC="localhost:$port" DATABASE_URL="$database" REDIS_URL="$redis" ZONE_KEEPER="localhost:$keeper_port" \
    ZONE_CERTIFICATE="$tls/leaf.pem" ZONE_KEY="$tls/leaf.key" ZONE_AUTHORITY="$tls/ca.pem" "$bin/mmo-zone"
}

start() {
  need initdb postgres pg_isready psql redis-server redis-cli openssl nc
  for port in "$pg_port" "$redis_port"; do
    ! nc -z -w 1 127.0.0.1 "$port" || die "the port $port is in use: set MMO_LOCAL_PG_PORT and MMO_LOCAL_REDIS_PORT"
  done
  for name in postgres redis keeper gateway $(logins held) $(writers held); do
    ! alive "$name" || die "$name runs already: run sh script/local.sh stop first"
  done
  rm -f "$state"/zone*.pid "$state"/login*.pid
  mkdir -p "$state/redis"
  say "building the servers, which takes minutes when the build is cold"
  case $profile in
    debug) flag= ;;
    release) flag=--release ;;
    *) die "MMO_LOCAL_PROFILE is debug or release" ;;
  esac
  (cd "$root" && sh script/build-slot.sh cargo build $flag -p mmo-keeper -p mmo-login -p mmo-zone -p mmo-gateway -p mmo-client -p mmo-bots)
  make_tls

  started=1
  [ -d "$state/postgres" ] || initdb -D "$state/postgres" -U postgres --auth=trust -E UTF8 >"$state/initdb.log" 2>&1 ||
    die "initdb failed"
  # Each layer writer of the pool holds 4 connections, so the default of 100 fails a stack of 2 realms.
  launch postgres postgres -D "$state/postgres" -h 127.0.0.1 -p "$pg_port" -c unix_socket_directories= -c max_connections=400
  launch redis redis-server --bind 127.0.0.1 --port "$redis_port" --save "" --appendonly no --dir "$state/redis"
  wait_for "PostgreSQL and Redis" 40 stores_up
  game_made

  launch_keeper
  wait_for "the migrations of the keeper" 60 zones_made

  number=0
  for name in $(logins); do
    number=$((number + 1))
    words=$(attributes "$number")
    region=${words%%:*}
    rest=${words#*:}
    language=${rest%%:*}
    rules=${rest#*:}
    [ "$rest" != "$words" ] && [ "$rules" != "$rest" ] ||
      die "the word $words of MMO_LOCAL_ATTRIBUTES is not region:language:rules"
    launch "$name" env MMO_LOGIN_ADDRESS="127.0.0.1:$(login_at "$number")" MMO_LOGIN_CERTIFICATE="$tls/leaf.pem" \
      MMO_LOGIN_KEY="$tls/leaf.key" DATABASE_URL="$database" REDIS_URL="$redis" MMO_REALM="$number" \
      MMO_REALM_NAME="Realm$number" MMO_REALM_REGION="$region" MMO_REALM_LANGUAGE="$language" \
      MMO_REALM_RULES="$rules" MMO_REALM_LOGIN="localhost:$(login_at "$number")" \
      MMO_REALM_CAP="$realm_cap" MMO_LAYER_CAP="$layer_cap" MMO_START_ZONE=1 MMO_START_POSITION=10000,0,10000 \
      "$bin/mmo-login"
  done
  # Each writer of the pool starts idle, and takes a layer that a placement opens.
  number=0
  for name in $(writers); do
    launch_writer "$number"
    number=$((number + 1))
  done
  launch_gateway

  sleep 1
  for name in postgres redis keeper gateway $(logins) $(writers); do
    alive "$name" || die "$name stopped just after its start"
  done
  say "the stack runs. A client uses these addresses, all on QUIC over UDP except the two stores:"
  number=1
  while [ "$number" -le "$realms" ]; do
    echo "  login      localhost:$(login_at "$number")  (realm $number, $(attributes "$number"), cap $realm_cap, layers of cap $layer_cap, $zones zones)"
    number=$((number + 1))
  done
  echo "  gateway    localhost:$gateway_port"
  echo "  authority  $tls/ca.pem  (the certificate that signs each server)"
  echo "  keeper     localhost:$keeper_port  $((pool * realms)) layer writers from localhost:$zone_port  (the servers use these)"
  echo "  postgres   $database"
  echo "  redis      $redis"
  echo "  logs       $state/<process>.log"
  echo "Check it with: sh script/local.sh check. Stop it with: sh script/local.sh stop."
}

check() {
  for name in login1 gateway zone1; do
    alive "$name" || die "$name does not run: run sh script/local.sh start first"
  done
  # An account name holds letters and digits, and the login service makes 5 accounts an hour for one address.
  MMO_CHECK_AUTHORITY="$tls/ca.pem" MMO_CHECK_LOGIN="localhost:$login_port" \
    MMO_CHECK_GATEWAY="localhost:$gateway_port" MMO_CHECK_NAME="check$(date +%s)" \
    MMO_CHECK_PASSWORD=local-check-password MMO_CHECK_CHARACTER=Probe "$bin/mmo-check" || die "the check failed"
  say "the check passed"
}

# The load test: bots that make accounts in the database, log in through the login service, wait in its queue, link to
# the gateway and walk. The count is the first argument, and the other variables of mmo-bots pass through the environment.
bots() {
  for name in $(logins) gateway zone1; do
    alive "$name" || die "$name does not run: run sh script/local.sh start first"
  done
  # The bots go round the realms, and each realm has its own login service.
  address=
  number=1
  while [ "$number" -le "$realms" ]; do
    address=${address:+$address,}localhost:$(login_at "$number")
    number=$((number + 1))
  done
  # Each bot opens a socket for its login and one for its gateway.
  ulimit -n 65536 2>/dev/null || ulimit -n "$(ulimit -Hn)" 2>/dev/null || true
  MMO_BOTS_AUTHORITY="$tls/ca.pem" MMO_BOTS_LOGIN="$address" MMO_BOTS_GATEWAY="localhost:$gateway_port" \
    DATABASE_URL="$database" REDIS_URL="$redis" MMO_BOTS_ZONE=1 MMO_BOTS_COUNT="${1:?usage: sh script/local.sh bots <count>}" \
    "$bin/mmo-bots" || die "the load test failed"
}

# Ends one process with the signal, then with KILL when it stays, and forgets its pid.
halt() {
  name=$1
  if alive "$name"; then
    pid=$(cat "$state/$name.pid")
    kill -s "$2" "$pid"
    tries=20
    while alive "$name" && [ "$tries" -gt 0 ]; do
      tries=$((tries - 1))
      sleep 0.5
    done
    if alive "$name"; then
      say "$name does not stop on $2: sending KILL to $pid"
      kill -s KILL "$pid"
    fi
    say "stopped $name (pid $pid)"
  fi
  rm -f "$state/$name.pid"
}

# Ends one process of the stack with KILL, waits $2 seconds, and starts it again.
fault() {
  case ${1:-} in
    gateway | keeper) name=$1 ;;
    writer)
      held=$(redis-cli -p "$redis_port" hget writer:1:1:1 address)
      [ -n "$held" ] || die "no writer holds the layer 1 of the zone 1 of the realm 1"
      name=zone$((${held##*:} - zone_port + 1))
      ;;
    *) die "fault is gateway, keeper or writer" ;;
  esac
  alive "$name" || die "$name does not run: run sh script/local.sh start first"
  halt "$name" KILL
  say "ended $name, which the fault $1 asks"
  sleep "${2:-0}"
  case $name in
    gateway) launch_gateway ;;
    keeper) launch_keeper ;;
    *) launch_writer $((${name#zone} - 1)) ;;
  esac
  sleep 1
  alive "$name" || die "$name stopped just after its start"
  say "started $name again"
}

# The players that the layers of the zone 1 of the realm 1 hold, as the directory counts them.
players() {
  redis-cli -p "$redis_port" hvals layers:1 | awk '{ n += $1 } END { print n + 0 }'
}

versions() {
  psql -q -h 127.0.0.1 -p "$pg_port" -U postgres -d "$game" -AtF ' ' -c \
    "SELECT id, version FROM characters WHERE realm_id = 1 ORDER BY id"
}

stop() {
  halt gateway TERM
  for name in $(writers held); do
    halt "$name" TERM
  done
  for name in $(logins held); do
    halt "$name" TERM
  done
  halt keeper TERM
  halt redis TERM
  # SIGINT is the fast shutdown of PostgreSQL: it does not wait for a client to leave.
  halt postgres INT
  for name in postgres redis keeper gateway $(logins held) $(writers held); do
    ! alive "$name" || die "$name stays"
  done
  say "no process of the stack stays"
}

status() {
  for name in postgres redis keeper gateway $(logins held) $(writers held); do
    if alive "$name"; then echo "$name runs, pid $(cat "$state/$name.pid")"; else echo "$name is stopped"; fi
  done
}

case $realms in
  '' | *[!0-9]* | 0) die "MMO_LOCAL_REALMS is a count of 1 or more" ;;
esac
case $zones in
  '' | *[!0-9]* | 0) die "MMO_LOCAL_ZONES is a count of 1 or more" ;;
esac

case ${1:-} in
  start) start ;;
  check) check ;;
  bots) bots "${2:-}" ;;
  stop) stop ;;
  status) status ;;
  fault) fault "${2:-}" "${3:-0}" ;;
  versions) versions ;;
  players) players ;;
  *) echo "usage: sh script/local.sh start|check|bots <count>|fault <what> [seconds]|versions|players|status|stop" >&2; exit 2 ;;
esac
