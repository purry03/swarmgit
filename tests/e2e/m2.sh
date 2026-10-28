#!/usr/bin/env bash
# M2 acceptance from spec 11.3: encrypted transport, wire protocol, and
# cloning from a peer that is known only by a link hint.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT_A=42501
PORT_C=42503
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
C_HOME=$WORK/c
E_HOME=$WORK/e

repo_of() {
    local link=$1
    link=${link#swarm://}
    printf '%s' "${link%%\?*}"
}

node_of() {
    local link=$1
    case $link in
        *@*) printf '%s' "${link##*@}" ;;
        *) swarm_fail "link has no node pin: $link" ;;
    esac
}

free_repo_id() {
    python3 - <<'PY'
import base64, os
print(base64.b32encode(os.urandom(32)).decode().rstrip("="))
PY
}

# ----- A creates and pushes -----
export SWARM_HOME=$A_HOME SWARM_LISTEN=127.0.0.1:$PORT_A
SRC=$WORK/src
mkdir -p "$SRC"
cd "$SRC"
git init -q -b main
swarm init >/dev/null
LINK_A=$(swarm link)
REPO=$(repo_of "$LINK_A")
NODE_A=$(node_of "$LINK_A")
git commit -q --allow-empty -m one
git push -q swarm main
HEAD_A=$(git rev-parse HEAD)

echo "== B clones from A through the link hint"
mkdir -p "$WORK/clone"
cd "$WORK/clone"
env SWARM_HOME=$B_HOME git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A" out
git -C out remote rename origin swarm
[[ $(git -C out rev-parse HEAD) == "$HEAD_A" ]] || swarm_fail "clone HEAD differs"
git -C out fsck --no-progress --strict >/dev/null 2>&1 || swarm_fail "clone fsck failed"

echo "== the first clone stored A's node id (trust on first use)"
PEER_NODE=$(sqlite3 "$B_HOME/swarm.db" \
    "SELECT hex(node_id) FROM peers WHERE node_id IS NOT NULL LIMIT 1;")
[[ -n "$PEER_NODE" && "$PEER_NODE" != "NULL" ]] || \
    swarm_fail "peers table has no node id after an unpinned clone"

echo "== a wrong pin is refused with auth_failed"
WRONG_NODE=$(python3 - <<'PY'
import base64, os
print(base64.b32encode(os.urandom(32)).decode().rstrip("="))
PY
)
mkdir -p "$WORK/wrong-pin"
cd "$WORK/wrong-pin"
set +e
PIN_OUT=$(timeout 60 env SWARM_HOME=$WORK/wrongpin-home \
    git clone "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A@$WRONG_NODE" out 2>&1)
PIN_STATUS=$?
set -e
[[ $PIN_STATUS -ne 0 ]] || swarm_fail "clone with a wrong pin succeeded"
[[ $PIN_STATUS -ne 124 ]] || swarm_fail "clone with a wrong pin hung"
grep -q "auth_failed" <<<"$PIN_OUT" || \
    swarm_fail "wrong pin did not report auth_failed: $PIN_OUT"

echo "== the right pin connects"
mkdir -p "$WORK/right-pin"
cd "$WORK/right-pin"
timeout 60 env SWARM_HOME=$WORK/rightpin-home \
    git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A@$NODE_A" out
[[ $(git -C out rev-parse HEAD) == "$HEAD_A" ]] || \
    swarm_fail "pinned clone HEAD differs"

echo "== a fresh client asking for a repo A does not hold gets no_shared_repo"
WRONG_REPO=$(free_repo_id)
mkdir -p "$WORK/wrong-repo"
cd "$WORK/wrong-repo"
set +e
WRONG_OUT=$(timeout 45 env SWARM_HOME=$WORK/wrongrepo-home \
    git clone "swarm://$WRONG_REPO?h=tcp:127.0.0.1:$PORT_A" out 2>&1)
WRONG_STATUS=$?
set -e
[[ $WRONG_STATUS -ne 124 ]] || swarm_fail "wrong repo clone hung"
[[ $WRONG_STATUS -ne 0 ]] || swarm_fail "wrong repo clone succeeded"
grep -q "no_shared_repo" <<<"$WRONG_OUT" || \
    swarm_fail "wrong repo did not report no_shared_repo: $WRONG_OUT"

echo "== B can push after taking A's root key"
mkdir -p "$B_HOME/keys"
cp "$A_HOME/keys/$REPO.key" "$B_HOME/keys/$REPO.key"
chmod 600 "$B_HOME/keys/$REPO.key"
cd "$WORK/clone/out"
git commit -q --allow-empty -m two
env SWARM_HOME=$B_HOME git push -q swarm main
HEAD_B=$(git rev-parse HEAD)
[[ $HEAD_B != "$HEAD_A" ]] || swarm_fail "B's commit did not land"

echo "== A pulls B's change from B's own listener"
LINK_B=$(env SWARM_HOME=$B_HOME swarm link)
NODE_B=$(node_of "$LINK_B")
PORT_B=$(python3 - "$B_HOME/daemon.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["port"])
PY
)
cd "$SRC"
timeout 60 git fetch -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_B@$NODE_B" main
git merge -q --ff-only FETCH_HEAD
[[ $(git rev-parse HEAD) == "$HEAD_B" ]] || swarm_fail "A did not get B's change"

echo "== A pushes again and B pulls"
git commit -q --allow-empty -m three
git push -q swarm main
HEAD_A2=$(git rev-parse HEAD)
cd "$WORK/clone/out"
timeout 60 env SWARM_HOME=$B_HOME git pull -q --no-rebase swarm main
[[ $(git rev-parse HEAD) == "$HEAD_A2" ]] || swarm_fail "B did not get A's change"

echo "== interest tokens never contain the repo ID on the wire"
PCAP=$WORK/handshake.pcap
mkdir -p "$WORK/pcap"
cd "$WORK/pcap"
tcpdump -i lo -s 0 -w "$PCAP" "tcp port $PORT_A" >/dev/null 2>&1 &
TCPDUMP=$!
sleep 0.5
timeout 60 env SWARM_HOME=$WORK/pcap-home \
    git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A@$NODE_A" out
sleep 0.5
kill "$TCPDUMP" 2>/dev/null || true
wait "$TCPDUMP" 2>/dev/null || true
[[ -s "$PCAP" ]] || swarm_fail "tcpdump captured nothing"
if grep -aFq "$REPO" "$PCAP"; then
    swarm_fail "pcap contains the repo ID in base32"
fi
python3 - "$PCAP" "$REPO" <<'PY' || exit 1
import base64, sys
pcap = open(sys.argv[1], "rb").read()
repo = sys.argv[2]
raw = base64.b32decode(repo.upper() + "=" * ((8 - len(repo) % 8) % 8))
if raw in pcap:
    raise SystemExit("FAIL: pcap contains the raw repo ID bytes")
PY

echo "== C seeds the repo so a pack can fail over to it"
# 50 MiB of incompressible data makes a pack that is still in flight when A dies.
dd if=/dev/urandom of="$SRC/big.bin" bs=1M count=50 status=none
cd "$SRC"
git add big.bin
git commit -q -m big
git push -q swarm main
HEAD_BIG=$(git rev-parse HEAD)
# A's daemon keeps serving the old pack; start C and copy the data.
mkdir -p "$WORK/seed"
cd "$WORK/seed"
env SWARM_HOME=$C_HOME SWARM_LISTEN=127.0.0.1:$PORT_C SWARM_ADVERTISE=127.0.0.1 \
    swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A@$NODE_A" >/dev/null
LINK_C=$(env SWARM_HOME=$C_HOME SWARM_LISTEN=127.0.0.1:$PORT_C swarm link)
NODE_C=$(node_of "$LINK_C")

echo "== a clone from C alone is correct"
mkdir -p "$WORK/from-c"
cd "$WORK/from-c"
timeout 120 env SWARM_HOME=$WORK/fromc-home \
    git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PORT_C@$NODE_C" out
[[ $(git -C out rev-parse HEAD) == "$HEAD_BIG" ]] || \
    swarm_fail "clone from the second seeder has the wrong HEAD"
git -C out fsck --no-progress --strict >/dev/null 2>&1 || \
    swarm_fail "clone from the second seeder failed fsck"

echo "== killing A mid-pack falls over to C"
mkdir -p "$WORK/failover"
cd "$WORK/failover"
LINK_AC="swarm://$REPO?h=tcp:127.0.0.1:$PORT_A@$NODE_A&h=tcp:127.0.0.1:$PORT_C@$NODE_C"
env SWARM_HOME=$E_HOME timeout 180 git clone -q "$LINK_AC" out &
CLONE=$!
# Wait until A has an inbound peer connection, then kill it while the pack is
# still being produced or sent.
for _ in $(seq 1 100); do
    if ss -tn state established "( sport = :$PORT_A )" 2>/dev/null | grep -q .; then
        break
    fi
    sleep 0.05
done
A_PID=$(python3 - "$A_HOME/daemon.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["pid"])
PY
)
kill -9 "$A_PID" 2>/dev/null || swarm_fail "could not kill A"
set +e
wait "$CLONE"
CLONE_STATUS=$?
set -e
[[ $CLONE_STATUS -eq 0 ]] || swarm_fail "failover clone failed"
[[ $(git -C out rev-parse HEAD) == "$HEAD_BIG" ]] || \
    swarm_fail "failover clone has the wrong HEAD"
git -C out fsck --no-progress --strict >/dev/null 2>&1 || \
    swarm_fail "failover clone failed fsck"

echo "M2 PASS"
