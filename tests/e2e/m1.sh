#!/usr/bin/env bash
# M1 acceptance from spec 11.2: local repo, daemon, CLI and remote helper.
#
# The first section is the acceptance block verbatim. The sections after it
# cover the additional M1 requirements in the same spec bullet list.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# ----- spec 11.2 acceptance block -----
export SWARM_HOME=$(mktemp -d) SWARM_NO_DHT=1 SWARM_NO_LAN=1
SRC=$(mktemp -d); cd $SRC && git init -q -b main && swarm init
ID=$(swarm link)
git commit -q --allow-empty -m one && git push -q swarm main
swarm status --json | jq -e '.repos[0].seq == 2'
cd $(mktemp -d) && git clone -q $ID copy
test $(git -C copy rev-parse HEAD) = $(git -C $SRC rev-parse HEAD)
swarm daemon stop; git ls-remote $ID | grep -q refs/heads/main     # autostart works
cd $SRC && git push -q swarm main                                  # no change
swarm status --json | jq -e '.repos[0].seq == 2'                   # still 2, publish was a no-op
# ----- end spec 11.2 acceptance block -----

cd "$SRC"
REPO_ID=${ID#swarm://}
REPO_ID=${REPO_ID%%\?*}
KEY="$SWARM_HOME/keys/$REPO_ID.key"

echo "== a second swarm init fails with exit 1"
set +e
SECOND=$(swarm init 2>&1)
STATUS=$?
set -e
[[ $STATUS -eq 1 ]] || swarm_fail "second swarm init exited $STATUS, want 1"
grep -q "already has a remote named swarm" <<<"$SECOND" || \
    swarm_fail "second swarm init did not explain the existing remote: $SECOND"

echo "== key backup writes mode 0600 and the same bytes"
BACKUP=$(mktemp)
swarm key backup "$ID" -o "$BACKUP" 2>/dev/null
[[ $(stat -c %a "$BACKUP") == 600 ]] || swarm_fail "backup mode is not 0600"
cmp -s "$BACKUP" "$KEY" || swarm_fail "backup bytes differ from the key file"

echo "== a push without a signing key reports HELP-3"
NO_KEY_CLONE=$(mktemp -d)
cd "$NO_KEY_CLONE"
git clone -q "$ID" copy 2>/dev/null
cd copy
git remote add swarm "$ID"
git commit -q --allow-empty -m two
mv "$KEY" "$KEY.away"
set +e
NO_KEY=$(git push swarm main 2>&1)
STATUS=$?
set -e
mv "$KEY.away" "$KEY"
[[ $STATUS -ne 0 ]] || swarm_fail "push without a key unexpectedly succeeded"
grep -q "no signing key (use: git swarm fork)" <<<"$NO_KEY" || \
    swarm_fail "push without a key did not print HELP-3: $NO_KEY"

echo "== SEC-10: the daemon log never contains the key"
KEY_HEX=$(od -An -v -tx1 "$KEY" | tr -d ' \n')
KEY_B32=$(python3 - "$KEY" <<'PY'
import base64, sys
print(base64.b32encode(open(sys.argv[1], "rb").read()).decode().rstrip("="))
PY
)
if grep -Fq "$KEY_HEX" "$SWARM_HOME/daemon.log"; then
    swarm_fail "daemon.log contains the key in hex"
fi
if grep -Fq "$KEY_B32" "$SWARM_HOME/daemon.log"; then
    swarm_fail "daemon.log contains the key in base32"
fi

echo "== killing the daemon during publish leaves a whole manifest"
cd "$SRC"
git commit -q --allow-empty -m three
swarm status >/dev/null 2>&1          # make sure a daemon is running
PID=$(python3 - "$SWARM_HOME/daemon.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["pid"])
PY
)
git push -q swarm main >/dev/null 2>&1 &
PUSH=$!
sleep 0.05
kill -9 "$PID" 2>/dev/null || true
wait "$PUSH" 2>/dev/null || true

swarm status --json >/dev/null        # autostart and reload manifests
SEQ=$(swarm status --json | jq -r '.repos[0].seq')
[[ "$SEQ" == 2 || "$SEQ" == 3 ]] || swarm_fail "unexpected seq after kill: $SEQ"

if find "$SWARM_HOME" -name '*.tmp' -print -quit | grep -q .; then
    swarm_fail "interrupted publish left a temporary file"
fi
if grep -q "ignoring invalid manifest" "$SWARM_HOME/daemon.log"; then
    swarm_fail "daemon found a torn manifest after the kill"
fi

echo "== SEC-13: purge asks first, and deletes the key only when told to"
REPO_ID=$(swarm status --json | jq -r '.repos[0].repo')
if echo no | swarm purge "$REPO_ID" >/dev/null 2>&1; then
    swarm_fail "purge went ahead without a yes"
fi
[[ -f $SWARM_HOME/keys/$REPO_ID.key ]] || swarm_fail "a refused purge removed the key"
cp "$SWARM_HOME/keys/$REPO_ID.key" "$SWARM_HOME/key.copy"
cp -a "$SWARM_HOME/repos/$REPO_ID" "$SWARM_HOME/repo.copy"
echo yes | swarm purge "$REPO_ID" >/dev/null 2>&1 || swarm_fail "purge failed"
[[ -f $SWARM_HOME/keys/$REPO_ID.key ]] || swarm_fail "purge deleted the key unasked"
[[ ! -e $SWARM_HOME/repos/$REPO_ID ]] || swarm_fail "purge left the store"
# Bring the repo back to purge it again, key included.
cp -a "$SWARM_HOME/repo.copy" "$SWARM_HOME/repos/$REPO_ID"
swarm daemon stop >/dev/null
sleep 1
sqlite3 "$SWARM_HOME/swarm.db" "INSERT INTO repos VALUES(x'$(python3 -c 'import base64,sys; print(base64.b32decode(sys.argv[1].upper()+"====").hex())' "$REPO_ID")', 'sha256', 0, 0, NULL);"
printf 'yes\nyes\n' | swarm purge "$REPO_ID" >/dev/null 2>&1 || swarm_fail "second purge failed"
[[ ! -e $SWARM_HOME/keys/$REPO_ID.key ]] || swarm_fail "purge left the key"
[[ ! -e $SWARM_HOME/repos/$REPO_ID ]] || swarm_fail "purge left the store"
[[ $(swarm status --json | jq '.repos | length') == 0 ]] || swarm_fail "purge left the repo listed"
if swarm purge "$REPO_ID" --yes >/dev/null 2>&1; then
    swarm_fail "purging an unknown repo succeeded"
fi

echo "M1 PASS"
