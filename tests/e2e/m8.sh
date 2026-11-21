#!/usr/bin/env bash
# M8 acceptance from spec 11.9: recovery-key rotation and static mirrors.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PA=42721; PB=42722; PC=42723
WORK=$(mktemp -d)
A=$WORK/a; B=$WORK/b; C=$WORK/c
repo_of() { local l=${1#swarm://}; printf '%s' "${l%%[?&]*}"; }
as() { local home=$1 port=$2; shift 2; env SWARM_HOME=$home SWARM_LISTEN=127.0.0.1:$port "$@"; }

mkdir "$WORK/src"; cd "$WORK/src"
as $A $PA git init -q -b main
as $A $PA swarm init >/dev/null
REPO=$(repo_of "$(as $A $PA swarm link)")
echo one >one; git add .; git commit -q -m one
as $A $PA git push -q swarm main
HINT="swarm://$REPO?h=tcp:127.0.0.1:$PA"

echo "== the root names a recovery key and saves it offline"
as $A $PA swarm key recovery "$REPO" -o "$WORK/recovery.key" >/dev/null 2>&1
[[ $(stat -c %a "$WORK/recovery.key") == 600 ]] || swarm_fail "recovery key is not mode 0600"

cd "$WORK"
as $B $PB git clone -q "$HINT" b-out
as $B $PB swarm status --json | jq -e '.repos[0].recovery == true' >/dev/null || swarm_fail "B does not see the recovery key"

echo "== a rotation with the wrong key is rejected"
mkdir "$WORK/other"; cd "$WORK/other"; as $C $PC git init -q -b main; as $C $PC swarm init >/dev/null
OTHER=$(repo_of "$(as $C $PC swarm link)")
as $C $PC swarm key recovery "$OTHER" -o "$WORK/other.key" >/dev/null 2>&1
cd "$WORK"
if as $B $PB swarm key rotate "$REPO" --recovery "$WORK/other.key" 2>"$WORK/err"; then
    swarm_fail "rotation with an unrelated key succeeded"
fi

echo "== the recovery key moves the root"
OLD_ROOT=$(as $B $PB swarm status --json | jq -r '.repos[0].root')
NEW_ROOT=$(as $B $PB swarm key rotate "$REPO" --recovery "$WORK/recovery.key")
[[ -n $NEW_ROOT && $NEW_ROOT != "$OLD_ROOT" ]] || swarm_fail "no new root"
as $B $PB swarm status --json | jq -e --arg r "$NEW_ROOT" '.repos[0].root == $r' >/dev/null

echo "== the new root publishes"
cd "$WORK/b-out"; echo two >two; git add .; git commit -q -m two
as $B $PB git push -q origin main
NEW_HEAD=$(git rev-parse HEAD)

echo "== a fresh clone follows the rotation"
cd "$WORK"
rm -rf "$C"; as $C $PC git clone -q "swarm://$REPO?h=tcp:127.0.0.1:$PB" c-out
[[ $(git -C c-out rev-parse HEAD) == "$NEW_HEAD" ]] || swarm_fail "fresh clone missed the new root's commit"

echo "== the old root key is rejected afterwards"
cd "$WORK/src"; echo three >three; git add .; git commit -q -m three
as $A $PA swarm seed "swarm://$REPO?h=tcp:127.0.0.1:$PB" >/dev/null 2>&1 || true
if as $A $PA git push -q -f swarm main 2>"$WORK/err"; then swarm_fail "old root key still publishes"; fi
grep -q unauthorized "$WORK/err" || swarm_fail "no unauthorized: $(cat "$WORK/err")"
echo PASS
