#!/usr/bin/env bash
# M5 acceptance from spec 11.6:
#  1. Relay: A seeds, B clones from A and seeds, A stops, and C clones from
#     B through the DHT testnet. Before A stops, a push on A reaches B within
#     10 s by Announce alone.
#  2. PEX: C has a hint only for B, and B knows D. C ends with D in its peers
#     table and still gets D's next push after B stops.
#  3. Two seeders: the client ends on the highest seq and never moves back.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

for tool in python3 sqlite3; do
    command -v "$tool" >/dev/null || { echo "SKIP: m5.sh needs $tool"; exit 0; }
done
[[ -x $SWARM_BIN_DIR/swarm-dht-testnet ]] || \
    swarm_fail "missing $SWARM_BIN_DIR/swarm-dht-testnet; run: cargo build --workspace"

WORK=$(mktemp -d)
TESTNET=
HOMES=()
for name in a b c b2 c2 d y k; do HOMES+=("$WORK/$name"); done

cleanup() {
    set +e
    for home in "${HOMES[@]}"; do
        [[ -d $home ]] && SWARM_HOME=$home swarm daemon stop >/dev/null 2>&1
    done
    [[ -n $TESTNET ]] && kill "$TESTNET" 2>/dev/null
}
trap cleanup EXIT

# run_as NAME PORT CMD...: run CMD as node NAME listening on
# 127.0.0.1:PORT, killed after 180 s.
run_as() {
    local name=$1 port=$2
    shift 2
    timeout 180 env SWARM_HOME="$WORK/$name" SWARM_LISTEN=127.0.0.1:$port SWARM_ADVERTISE=127.0.0.1 "$@"
}

repo_of() {
    local link=$1
    link=${link#swarm://}
    printf '%s' "${link%%\?*}"
}

seq_of() {
    env SWARM_HOME="$WORK/$1" swarm status "$2" --json | python3 -c '
import json, sys
data = json.load(sys.stdin)
if "repos" in data:
    data = data["repos"][0]
print(data["seq"])'
}

stop_daemon() {
    SWARM_HOME="$WORK/$1" swarm daemon stop >/dev/null
    for _ in $(seq 1 100); do
        [[ -f $WORK/$1/daemon.json ]] || return 0
        sleep 0.1
    done
    swarm_fail "daemon $1 did not exit"
}

# wait_seq NAME REPO SEQ SECONDS: the local daemon reaches SEQ in time.
wait_seq() {
    local deadline=$((SECONDS + $4))
    while ((SECONDS < deadline)); do
        [[ $(seq_of "$1" "$2") -ge $3 ]] && return 0
        sleep 0.2
    done
    return 1
}

# new_repo NAME PORT DIR: NAME creates a repo in DIR, pushes, prints its ID.
new_repo() {
    mkdir -p "$3"
    (
        cd "$3"
        git init -q -b main
        run_as "$1" "$2" swarm init >/dev/null
        git commit -q --allow-empty -m one
        run_as "$1" "$2" git push -q swarm main
        repo_of "$(run_as "$1" "$2" swarm link)"
    )
}

# push NAME PORT DIR: commit and push in DIR, print the new HEAD.
push() {
    (
        cd "$3"
        git commit -q --allow-empty -m "next $(date +%s%N)"
        run_as "$1" "$2" git push -q swarm main
        git rev-parse HEAD
    )
}

# ---------------------------------------------------------------- relay --
echo "== start a 10-node DHT testnet"
"$SWARM_BIN_DIR/swarm-dht-testnet" 10 >"$WORK/testnet.out" 2>&1 &
TESTNET=$!
for _ in $(seq 1 100); do
    [[ -s $WORK/testnet.out ]] && break
    sleep 0.1
done
BOOTSTRAP=$(head -n1 "$WORK/testnet.out")
[[ $BOOTSTRAP == *:* ]] || swarm_fail "testnet printed no bootstrap list"

(
    unset SWARM_NO_DHT
    export SWARM_DHT_BOOTSTRAP=$BOOTSTRAP SWARM_DHT_BIND=127.0.0.1
    PA=42701 PB=42702 PC=42703

    echo "== A creates a repo and seeds it"
    REPO=$(new_repo a $PA "$WORK/relay-src")

    echo "== B clones from A and seeds"
    mkdir -p "$WORK/relay-b"
    cd "$WORK/relay-b"
    run_as b $PB swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PA" >/dev/null
    SEQ_B=$(seq_of b "$REPO")

    echo "== a push on A reaches B within 10 s by Announce"
    HEAD_2=$(push a $PA "$WORK/relay-src")
    wait_seq b "$REPO" $((SEQ_B + 1)) 10 || \
        swarm_fail "A's push did not reach B within 10 s"

    echo "== A stops; C clones through the DHT and gets B's copy"
    stop_daemon a
    mkdir -p "$WORK/relay-c"
    cd "$WORK/relay-c"
    run_as c $PC git clone -q "swarm://$REPO" out || \
        swarm_fail "relay clone from B failed"
    [[ $(git -C out rev-parse HEAD) == "$HEAD_2" ]] || swarm_fail "relay clone HEAD differs"
    git -C out fsck --no-progress --strict >/dev/null 2>&1 || swarm_fail "relay clone fsck failed"
    stop_daemon b
    stop_daemon c
)
kill "$TESTNET" 2>/dev/null || true
TESTNET=

# ------------------------------------------------------------------ PEX --
PB=42712 PC=42713 PD=42714
echo "== D creates a repo; B seeds it from D"
REPO=$(new_repo d $PD "$WORK/pex-src")
mkdir -p "$WORK/pex-b"
cd "$WORK/pex-b"
run_as b2 $PB swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PD" >/dev/null

echo "== C clones with a hint for B only and learns D by peer exchange"
mkdir -p "$WORK/pex-c"
cd "$WORK/pex-c"
run_as c2 $PC git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PB" out || \
    swarm_fail "clone from B failed"
D_ROWS=$(sqlite3 "$WORK/c2/swarm.db" \
    "SELECT count(*) FROM peers WHERE addr LIKE '%:$PD' AND node_id IS NOT NULL;")
[[ $D_ROWS -ge 1 ]] || swarm_fail "C's peers table has no pinned entry for D"

echo "== B stops; D's next push still reaches C"
stop_daemon b2
SEQ_C=$(seq_of c2 "$REPO")
HEAD_D=$(push d $PD "$WORK/pex-src")
wait_seq c2 "$REPO" $((SEQ_C + 1)) 10 || swarm_fail "D's push did not reach C within 10 s"
cd "$WORK/pex-c/out"
run_as c2 $PC git pull -q --no-rebase origin main || swarm_fail "pull from D failed"
[[ $(git rev-parse HEAD) == "$HEAD_D" ]] || swarm_fail "C did not get D's change"

# ---------------------------------------------------------- two seeders --
PY=42715 PK=42716
echo "== Y seeds the repo, then falls behind D"
mkdir -p "$WORK/two-y"
cd "$WORK/two-y"
run_as y $PY swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PD" >/dev/null
stop_daemon y
sqlite3 "$WORK/y/swarm.db" "DELETE FROM peers;"
stop_daemon c2
HEAD_NEW=$(push d $PD "$WORK/pex-src")
SEQ_D=$(seq_of d "$REPO")
cd "$WORK/two-y"
run_as y $PY swarm daemon start >/dev/null
SEQ_Y=$(seq_of y "$REPO")
[[ $SEQ_Y -lt $SEQ_D ]] || swarm_fail "Y is not behind D ($SEQ_Y vs $SEQ_D)"

echo "== a client of both seeders ends on the highest seq"
mkdir -p "$WORK/two-k"
cd "$WORK/two-k"
run_as k $PK git clone -q \
    "swarm://$REPO?h=tcp:127.0.0.1:$PY&h=tcp:127.0.0.1:$PD" out || \
    swarm_fail "clone from two seeders failed"
[[ $(git -C out rev-parse HEAD) == "$HEAD_NEW" ]] || swarm_fail "client is not on D's HEAD"
[[ $(seq_of k "$REPO") -eq $SEQ_D ]] || swarm_fail "client seq is not D's"

echo "== with D gone, a fetch from stale Y does not move the client back"
stop_daemon d
cd "$WORK/two-k/out"
run_as k $PK git fetch -q "swarm://$REPO?h=tcp:127.0.0.1:$PY" main || \
    swarm_fail "fetch from Y failed"
[[ $(seq_of k "$REPO") -eq $SEQ_D ]] || swarm_fail "client seq moved backwards"
[[ $(git rev-parse FETCH_HEAD) == "$HEAD_NEW" ]] || swarm_fail "fetch from Y moved main back"

echo "== gc repacks the store and keeps every reachable object"
run_as k $PK swarm gc "$REPO" >/dev/null || swarm_fail "swarm gc failed"
STORE=$(dirname "$(find "$WORK/k" -type d -name objects -print -quit)")
[[ -d $STORE ]] || swarm_fail "no store under the client home"
git --git-dir "$STORE" fsck --no-progress --connectivity-only >/dev/null 2>&1 || \
    swarm_fail "store fsck failed after gc"
git --git-dir "$STORE" cat-file -e "$HEAD_NEW" || swarm_fail "gc dropped the tip"
[[ $(seq_of k "$REPO") -eq $SEQ_D ]] || swarm_fail "gc changed the repo state"

echo "M5 PASS"
