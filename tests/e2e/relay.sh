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

echo "PASS"
