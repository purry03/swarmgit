#!/usr/bin/env bash
# Repo name and description: set by the root or a maintainer, carried between
# peers, shown by `swarm inspect`.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT_A=42561
PORT_B=42562
PORT_C=42563
PORT_RELAY=42564
PORT_D=42565
PORT_E=42566
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
C_HOME=$WORK/c

RELAY=
cleanup() {
    [[ -n $RELAY ]] && kill "$RELAY" 2>/dev/null
    for home in "$A_HOME" "$B_HOME" "$C_HOME" "$WORK/d" "$WORK/e"; do
        SWARM_HOME=$home swarm daemon stop >/dev/null 2>&1 || true
    done
    rm -rf "$WORK" "$HELPER_DIR"
}
trap cleanup EXIT

a() { env SWARM_HOME="$A_HOME" SWARM_LISTEN=127.0.0.1:$PORT_A "$@"; }
b() { env SWARM_HOME="$B_HOME" SWARM_LISTEN=127.0.0.1:$PORT_B "$@"; }
c() { env SWARM_HOME="$C_HOME" SWARM_LISTEN=127.0.0.1:$PORT_C "$@"; }

expect() {
    local have=$1 want=$2
    grep -qF -- "$want" <<<"$have" || swarm_fail "wanted '$want' in: $have"
}

mkdir -p "$WORK/src"
cd "$WORK/src"
git init -q -b main
a swarm init >/dev/null
LINK=$(a swarm link)
REPO=${LINK#swarm://}
REPO=${REPO%%\?*}
git commit -q --allow-empty -m one
a git push -q swarm main

echo "== the root names the repo"
expect "$(a swarm inspect --json "$LINK")" '"name":null'
a swarm about "$REPO" --name demo --description "first words" >/dev/null
expect "$(a swarm about "$REPO")" 'name: demo'

echo "== inspect by a stranger shows it without storing the repo"
SEEN=$(c swarm inspect --json "$LINK")
expect "$SEEN" '"name":"demo"'
expect "$SEEN" '"description":"first words"'
[[ ! -d $C_HOME/repos/$REPO ]] || swarm_fail "inspect wrote a store"

echo "== a clone stores it; a key that is not listed cannot change it"
b git clone -q "$LINK" "$WORK/clone"
expect "$(b swarm about "$REPO")" 'name: demo'
KEY=$(b swarm key new "$REPO")
if b swarm about "$REPO" --name stolen >/dev/null 2>&1; then
    swarm_fail "an unlisted key renamed the repo"
fi

echo "== a maintainer changes one field and the root receives it"
a swarm maintainers add "$REPO" "$KEY" >/dev/null
b swarm seed "$LINK" >/dev/null
b swarm about "$REPO" --description "from the maintainer" >/dev/null
for _ in $(seq 50); do
    grep -q "from the maintainer" <<<"$(a swarm about "$REPO")" && break
    sleep 0.2
done
expect "$(a swarm about "$REPO")" 'description: from the maintainer'
expect "$(a swarm about "$REPO")" 'name: demo'

echo "== removing the maintainer withdraws what only they signed"
a swarm maintainers remove "$REPO" "$KEY" >/dev/null
expect "$(c swarm inspect --json "$LINK")" '"name":null'
a swarm about "$REPO" --name demo >/dev/null

echo "== unusable text is refused, and an empty value removes a field"
if a swarm about "$REPO" --name $'two\nlines' >/dev/null 2>&1; then
    swarm_fail "a two line name was accepted"
fi
a swarm about "$REPO" --name "" >/dev/null
expect "$(c swarm inspect --json "$LINK")" '"name":null'

echo "== inspect lists branches and tags, and the forks on either side"
a git tag v1
a git push -q swarm v1 main:refs/heads/side
a swarm about "$REPO" --name demo >/dev/null
SEEN=$(c swarm inspect --json "$LINK")
expect "$SEEN" '"branches":["main","side"]'
expect "$SEEN" '"tags":["v1"]'
expect "$SEEN" '"upstream":null'
expect "$SEEN" '"downstream":[]'
TEXT=$(c swarm inspect "$LINK")
expect "$TEXT" 'name: demo'
expect "$TEXT" 'main (default)'
FORK=$(c swarm fork "$LINK")
SEEN=$(c swarm inspect --json "$FORK")
expect "$SEEN" "\"upstream\":{\"link\":\"swarm://$REPO\""
expect "$(c swarm inspect "$FORK")" "forked from: swarm://$REPO"
expect "$(c swarm inspect "$LINK")" "fork: swarm://${FORK:8:52}"

echo "== a fork starts with the name and description of its source"
a swarm about "$REPO" --description "first words" >/dev/null
NAMED=$(b swarm fork "$LINK")
expect "$(b swarm about "${NAMED:8:52}")" 'name: demo'
expect "$(b swarm about "${NAMED:8:52}")" 'description: first words'
# A visitor stays connected and holds no copy of the record; the owner's own
# inspect still has it.
c swarm inspect "$NAMED" >/dev/null
SEEN=$(b swarm inspect "$NAMED")
expect "$SEEN" 'name: demo'
expect "$SEEN" 'description: first words'
b swarm about "${NAMED:8:52}" --name mine >/dev/null
expect "$(b swarm about "${NAMED:8:52}")" 'name: mine'
expect "$(a swarm about "$REPO")" 'name: demo'

echo "== inspect lists a seeder that only a relay knows, besides the one in the link"
"$SWARM_BIN_DIR/swarm-relay" --listen "127.0.0.1:$PORT_RELAY" --no-network >"$WORK/relay.log" 2>&1 &
RELAY=$!
export SWARM_RELAY_ALLOW_PRIVATE=1 SWARM_RELAY_DISCOVERY=off
via() { env SWARM_RELAYS=127.0.0.1:$PORT_RELAY "$@"; }
d() { via env SWARM_HOME="$WORK/d" SWARM_LISTEN=127.0.0.1:$PORT_D SWARM_RELAY_MODE=always "$@"; }
e() { via env SWARM_HOME="$WORK/e" SWARM_LISTEN=127.0.0.1:$PORT_E "$@"; }
# D gets its copy from a file, so A has never heard of it.
a swarm export "$LINK" -o "$WORK/repo.swarmbundle" >/dev/null
d swarm import "$WORK/repo.swarmbundle" >/dev/null
d swarm seed "swarm://$REPO" >/dev/null 2>&1 || true
for _ in $(seq 100); do
    d swarm status relays | grep -q ' registered' && break
    sleep 0.2
done
D_NODE=$(d swarm status | awk '$1 == "Node" { print $2 }')
[[ -n $D_NODE ]] || swarm_fail "no node ID for D"
# It tells the relay which repos it holds on a 5 second cycle.
sleep 7
SEEN=$(e swarm inspect --json "swarm://$REPO?h=tcp:127.0.0.1:$PORT_A")
expect "$SEEN" "$D_NODE"
[[ $(grep -o '"nodeId"' <<<"$SEEN" | wc -l) -ge 2 ]] || swarm_fail "inspect listed one peer: $SEEN"

echo "ABOUT PASS"
