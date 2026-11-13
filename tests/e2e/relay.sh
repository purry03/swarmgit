#!/usr/bin/env bash
# Relay acceptance (design spec R1): two peers on isolated networks reach each
# other only through the relay, with the swarm handshake end to end.
#
#   tests/e2e/relay.sh
#
# Needs docker. It builds the image, runs the topology in compose.relay.yaml,
# and cleans up on exit.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
COMPOSE=(docker compose -f compose.relay.yaml)

fail() {
    echo "FAIL: $*" >&2
    "${COMPOSE[@]}" logs --no-color 2>&1 | tail -40 >&2 || true
    exit 1
}

cleanup() { "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

"${COMPOSE[@]}" up -d --build >/dev/null 2>&1 || fail "could not start the topology"

peer() { "${COMPOSE[@]}" exec -T "$@"; }

echo "== the peers cannot reach each other directly"
B_IP=$("${COMPOSE[@]}" exec -T peer-b sh -c "ip -4 -o addr show eth0 | awk '{print \$4}' | cut -d/ -f1")
[[ -n $B_IP ]] || fail "no address for peer-b"
if peer peer-a bash -c "timeout 3 bash -c '</dev/tcp/$B_IP/42425'" 2>/dev/null; then
    fail "peer-a reached peer-b directly; the networks are not isolated"
fi
if peer peer-a getent hosts peer-b >/dev/null 2>&1; then
    fail "peer-a can resolve peer-b"
fi

echo "== peer-b registers with the relay"
B_ID=$(peer peer-b swarm-relay-peer --key /state/b.key id)
A_ID=$(peer peer-a swarm-relay-peer --key /state/a.key id)
[[ -n $B_ID && -n $A_ID && $A_ID != "$B_ID" ]] || fail "bad node ids"
peer peer-b sh -c "nohup swarm-relay-peer --key /state/b.key listen --relay relay:42425 >/state/b.log 2>&1 &"
for _ in $(seq 50); do
    peer peer-b grep -q "listening node=$B_ID" /state/b.log 2>/dev/null && break
    sleep 0.2
done
peer peer-b grep -q "listening node=$B_ID" /state/b.log || fail "peer-b did not register"

echo "== peer-a dials peer-b by node ID through the relay"
OUT=$(peer peer-a swarm-relay-peer --key /state/a.key dial --relay relay:42425 --target "$B_ID") \
    || fail "dial failed"
echo "$OUT"
[[ $OUT == *"dialed peer=$B_ID reply=pong via relay"* ]] || fail "unexpected dial output: $OUT"

echo "== peer-b saw peer-a's authenticated node ID and the message"
sleep 0.5
LOG=$(peer peer-b cat /state/b.log)
echo "$LOG"
[[ $LOG == *"served peer=$A_ID got=ping via relay"* ]] || fail "peer-b did not serve peer-a"

echo "== dialing an unregistered node is refused"
GHOST=$(peer peer-a swarm-relay-peer --key /state/ghost.key id)
if peer peer-a swarm-relay-peer --key /state/a.key dial --relay relay:42425 --target "$GHOST" 2>/dev/null; then
    fail "dial to an unknown node succeeded"
fi

echo "== R2: git clone through the relay between isolated networks"
# peer-b seeds a repo and registers with the relay; its tcp hint is useless
# from peer-a, so the relay hint in the link is the only way in.
peer peer-b bash -c '
    set -e
    mkdir -p /state/work/src && cd /state/work/src
    git init -q -b main && swarm init >/dev/null
    echo "hello through the relay" > file.txt
    git add file.txt && git commit -q -m one && git push -q swarm main
' || fail "peer-b could not create its repo"
for _ in $(seq 50); do
    LINK=$(peer peer-b bash -c 'cd /state/work/src && swarm link' 2>/dev/null) || true
    [[ $LINK == *"h=relay:relay:42425@"* ]] && break
    sleep 0.2
done
[[ $LINK == *"h=relay:relay:42425@"* ]] || fail "link has no relay hint: $LINK"
echo "link: $LINK"
peer peer-b swarm status 2>&1 | grep -q "relay relay:42425 registered" || fail "swarm status does not show the relay"

CLONE_OUT=$(peer peer-a bash -c "cd /state/work && git clone '$LINK' out 2>&1") || fail "clone failed: $CLONE_OUT"
echo "$CLONE_OUT" | tail -5
[[ $(peer peer-a cat /state/work/out/file.txt) == "hello through the relay" ]] || fail "cloned content differs"
peer peer-a git -C /state/work/out fsck --strict >/dev/null 2>&1 || fail "clone fsck failed"

echo "== a pinned peer is also tried through our own relay, without a relay hint in the link"
NODE=${LINK##*@}
PIN_LINK="${LINK%%\?*}?h=tcp:10.255.255.1:42424@$NODE"
PIN_OUT=$(peer peer-a bash -c "cd /state/work && SWARM_RELAYS=relay:42425 SWARM_RELAY_DELAY_MS=200 SWARM_HOME=/state/home-pin SWARM_LISTEN=0.0.0.0:42502 git clone '$PIN_LINK' out4 2>&1") || fail "pinned clone through our own relay failed: $PIN_OUT"
[[ $(peer peer-a cat /state/work/out4/file.txt) == "hello through the relay" ]] || fail "pinned clone content differs"

echo "== a link without the relay hint cannot reach peer-b, and says why"
BARE=${LINK%%\?*}
PEER_ERR=$(peer peer-a bash -c "cd /state/work && SWARM_HOME=/state/home-bare SWARM_LISTEN=0.0.0.0:42500 SWARM_FIND_TIMEOUT=5 git clone '$BARE' out2 2>&1" || true)
echo "$PEER_ERR" | tail -3
[[ $PEER_ERR == *"no_peers"* ]] || fail "expected no_peers for a link with no way in"
[[ $PEER_ERR == *"SWARM_RELAYS"* ]] || fail "the error should suggest a relay"

echo "== SWARM_RELAY_MODE=off ignores relay hints"
OFF_ERR=$(peer peer-a bash -c "cd /state/work && SWARM_RELAY_MODE=off SWARM_FIND_TIMEOUT=5 SWARM_HOME=/state/home-off SWARM_LISTEN=0.0.0.0:42501 git clone '$LINK' out3 2>&1" || true)
[[ $OFF_ERR != *"download complete"* ]] || fail "relay mode off should not use the relay: $OFF_ERR"
[[ $OFF_ERR == *"no_peers"* || $OFF_ERR == *"auth_failed"* ]] || fail "unexpected result with relays off: $OFF_ERR"

echo "PASS"
