#!/usr/bin/env bash
# Repo name and description: set by the root or a maintainer, carried between
# peers, shown by `swarm inspect`.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT_A=42561
PORT_B=42562
PORT_C=42563
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
C_HOME=$WORK/c

cleanup() {
    for home in "$A_HOME" "$B_HOME" "$C_HOME"; do
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

echo "ABOUT PASS"
