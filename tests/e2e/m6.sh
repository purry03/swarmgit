#!/usr/bin/env bash
# M6 acceptance from spec 11.7: bundles for offline transfer.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

A=$(mktemp -d); B=$(mktemp -d); SRC=$(mktemp -d); WORK=$(mktemp -d)
export SWARM_HOME=$A
cd "$SRC" && git init -q -b main && swarm init >/dev/null
ID=$(swarm link)
for i in 1 2 3; do echo "$i" >"f$i"; git add .; git commit -q -m "c$i"; done
git push -q swarm main
REPO=${ID#swarm://}; REPO=${REPO%%[?&]*}

echo "== export"
swarm export "$ID" -o "$WORK/repo.swarmbundle" >/dev/null
test -s "$WORK/repo.swarmbundle"
head -c 5 "$WORK/repo.swarmbundle" | grep -q SWGB1

echo "== import on a machine with no network, then clone"
export SWARM_HOME=$B
swarm daemon stop >/dev/null 2>&1 || true
# Root needs no user namespace for this, and must not have one: inside it, root
# cannot reach other users' files, such as a build in a CI runner's home.
if [[ $(id -u) -eq 0 ]]; then userns=(-n); else userns=(-rn); fi
if unshare "${userns[@]}" true 2>/dev/null; then
    NONET="unshare ${userns[*]}"
    $NONET bash -c 'ip link set lo up 2>/dev/null; true'
else
    echo "(no network namespaces here; importing without them)"
    NONET=""
fi
$NONET env PATH="$PATH" SWARM_HOME=$B swarm import "$WORK/repo.swarmbundle" >/dev/null
$NONET env PATH="$PATH" SWARM_HOME=$B git clone -q "swarm://$REPO" "$WORK/copy"
test "$(git -C "$WORK/copy" rev-parse HEAD)" = "$(git -C "$SRC" rev-parse HEAD)" \
    || swarm_fail "cloned HEAD differs"
git -C "$WORK/copy" fsck --no-progress 2>/dev/null

echo "== a tampered bundle is rejected and leaves a fresh store unchanged"
SIZE=$(stat -c %s "$WORK/repo.swarmbundle")
for where in 20 100 $((SIZE - 100)); do
    export SWARM_HOME=$(mktemp -d)
    cp "$WORK/repo.swarmbundle" "$WORK/bad.swarmbundle"
    printf '\xff' | dd of="$WORK/bad.swarmbundle" bs=1 seek="$where" conv=notrunc 2>/dev/null
    if swarm import "$WORK/bad.swarmbundle" >/dev/null 2>&1; then
        swarm_fail "tampered bundle (byte $where) was accepted"
    fi
    [[ ! -e "$SWARM_HOME/repos/$(echo "$REPO")" ]] || \
        [[ -z $(ls "$SWARM_HOME/repos" 2>/dev/null) ]] || swarm_fail "store left behind at byte $where"
done

echo "== an older bundle does not move refs"
export SWARM_HOME=$A
cd "$SRC" && echo 4 >f4 && git add . && git commit -q -m c4 && git push -q swarm main
NEW=$(git rev-parse HEAD)
swarm import "$WORK/repo.swarmbundle" >/dev/null
test "$(git ls-remote "swarm://$REPO" refs/heads/main | cut -f1)" = "$NEW" \
    || swarm_fail "old bundle moved refs backwards"
echo PASS
