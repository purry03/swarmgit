#!/usr/bin/env bash
# M4 acceptance from spec 11.5 on a local Mainline testnet of 10 nodes:
#  1. A announces, and B finds A with no hints at all and clones.
#  2. A publishes seq 3 while a stale peer S still serves seq 2. B starts
#     from S, sees the DHT head pointer ahead of S and keeps looking until
#     it reaches seq 3.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

for tool in python3 sqlite3; do
    command -v "$tool" >/dev/null || { echo "SKIP: dht.sh needs $tool"; exit 0; }
done
[[ -x $SWARM_BIN_DIR/swarm-dht-testnet ]] || \
    swarm_fail "missing $SWARM_BIN_DIR/swarm-dht-testnet; run: cargo build --workspace"

PORT_A=42601
PORT_S=42602
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
S_HOME=$WORK/s
H_HOME=$WORK/h
TESTNET=

cleanup() {
    set +e
    for home in "$A_HOME" "$B_HOME" "$S_HOME" "$H_HOME"; do
        [[ -d $home ]] && SWARM_HOME=$home swarm daemon stop >/dev/null 2>&1
    done
    [[ -n $TESTNET ]] && kill "$TESTNET" 2>/dev/null
}
trap cleanup EXIT

seq_of() {
    env SWARM_HOME="$1" swarm status "$REPO" --json | python3 -c '
import json, sys
data = json.load(sys.stdin)
if "repos" in data:
    data = data["repos"][0]
print(data["seq"])'
}

# Stop a daemon and wait until it has exited (it persists DHT nodes last).
stop_daemon() {
    SWARM_HOME=$1 swarm daemon stop >/dev/null
    for _ in $(seq 1 100); do
        [[ -f $1/daemon.json ]] || return 0
        sleep 0.1
    done
    swarm_fail "daemon in $1 did not exit"
}

repo_of() {
    local link=$1
    link=${link#swarm://}
    printf '%s' "${link%%\?*}"
}

echo "== start a 10-node DHT testnet"
"$SWARM_BIN_DIR/swarm-dht-testnet" 10 >"$WORK/testnet.out" 2>&1 &
TESTNET=$!
for _ in $(seq 1 100); do
    [[ -s $WORK/testnet.out ]] && break
    sleep 0.1
done
BOOTSTRAP=$(head -n1 "$WORK/testnet.out")
[[ $BOOTSTRAP == *:* ]] || swarm_fail "testnet printed no bootstrap list"

# Route C only: no LAN, and only the local testnet.
unset SWARM_NO_DHT
export SWARM_NO_LAN=1
export SWARM_DHT_BOOTSTRAP=$BOOTSTRAP
export SWARM_DHT_BIND=127.0.0.1

echo "== A creates a repo and pushes"
SRC=$WORK/src
mkdir -p "$SRC"
cd "$SRC"
git init -q -b main
export SWARM_HOME=$A_HOME SWARM_LISTEN=127.0.0.1:$PORT_A
swarm init >/dev/null
REPO=$(repo_of "$(swarm link)")
git commit -q --allow-empty -m one
git push -q swarm main
HEAD_1=$(git rev-parse HEAD)
unset SWARM_LISTEN

echo "== B clones with no hints, through the DHT alone"
mkdir -p "$WORK/b-clone"
cd "$WORK/b-clone"
timeout 120 env SWARM_HOME=$B_HOME git clone -q "swarm://$REPO" out || \
    swarm_fail "hintless clone over the DHT failed"
[[ $(git -C out rev-parse HEAD) == "$HEAD_1" ]] || swarm_fail "hintless clone HEAD differs"
git -C out fsck --no-progress --strict >/dev/null 2>&1 || swarm_fail "hintless clone fsck failed"

echo "== the DHT node table is persisted (DHT-3)"
stop_daemon "$B_HOME"
NODES=$(sqlite3 "$B_HOME/swarm.db" "SELECT count(*) FROM dht_nodes;")
[[ $NODES -gt 0 ]] || swarm_fail "dht_nodes is empty after a DHT session"

echo "== a stale peer S takes seq 2 and stays off the DHT"
cd "$SRC"
git commit -q --allow-empty -m two
SWARM_HOME=$A_HOME git push -q swarm main
mkdir -p "$WORK/s-seed"
cd "$WORK/s-seed"
env SWARM_HOME=$S_HOME SWARM_LISTEN=127.0.0.1:$PORT_S SWARM_NO_DHT=1 \
    swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A" >/dev/null
SEQ_S=$(seq_of "$S_HOME")
stop_daemon "$S_HOME"
# S must not lead B to A by peer exchange: forget every peer it knew.
sqlite3 "$S_HOME/swarm.db" "DELETE FROM peers;"

echo "== A publishes seq 3 while S is down"
cd "$SRC"
git commit -q --allow-empty -m three
SWARM_HOME=$A_HOME git push -q swarm main
HEAD_3=$(git rev-parse HEAD)
SEQ_A=$(seq_of "$A_HOME")
[[ $SEQ_A -gt $SEQ_S ]] || swarm_fail "A is not ahead of S ($SEQ_A vs $SEQ_S)"

echo "== S comes back serving the old seq"
cd "$WORK/s-seed"
env SWARM_HOME=$S_HOME SWARM_LISTEN=127.0.0.1:$PORT_S SWARM_NO_DHT=1 \
    swarm daemon >/dev/null
for _ in $(seq 1 50); do
    [[ -f $S_HOME/daemon.json ]] && break
    sleep 0.1
done

echo "== H starts from S only, sees head seq $SEQ_A on the DHT and reaches it"
mkdir -p "$WORK/h-clone"
cd "$WORK/h-clone"
timeout 180 env SWARM_HOME=$H_HOME \
    git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_S" out || \
    swarm_fail "clone from the stale peer failed"
[[ $(git -C out rev-parse HEAD) == "$HEAD_3" ]] || \
    swarm_fail "the client stopped at the stale peer's seq"
SEQ_H=$(seq_of "$H_HOME")
[[ $SEQ_H -eq $SEQ_A ]] || swarm_fail "client root seq $SEQ_H, want $SEQ_A"

echo "== no DHT data carries the repo ID"
# Topics and the head salt are derived; the testnet output is the only text
# we can grep, and our own logs must not name the repo either.
for home in "$A_HOME" "$B_HOME" "$H_HOME"; do
    if grep -rqF "$REPO" "$home"/*.log 2>/dev/null; then
        swarm_fail "a daemon log names the repo ID"
    fi
done

echo "DHT PASS"
