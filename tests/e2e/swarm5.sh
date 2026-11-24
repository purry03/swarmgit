#!/usr/bin/env bash
# Five-container end-to-end test: 2 relays, 3 isolated peers (compose.swarm5.yaml).
#
#   tests/e2e/swarm5.sh [--keep]
#
# peer-a, peer-b and peer-c sit on three internal networks with no route
# between them; relay1 and relay2 are the only hosts on all three. Everything
# the peers do together therefore goes through a relay, a bundle carried
# out of band, or a static mirror served from relay1.
#
# Covered: relay registration, clone/fetch/push through relays, tags, branch
# deletion, force pushes, large and odd files, concurrent clones, relay
# failover and total relay loss, bad links, maintainers with scoped refs,
# conflicts and accept, bundles (and tampering), forks, unseed/gc, the YAML
# config, daemon restart, recovery-key rotation, and static mirrors.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
DC=(docker compose -f compose.swarm5.yaml)
KEEP=0; [[ ${1:-} == --keep ]] && KEEP=1

PASSED=0
fail() {
    echo "FAIL: $*" >&2
    for svc in peer-a peer-b peer-c relay1 relay2; do
        echo "--- $svc (daemon log tail)" >&2
        "${DC[@]}" exec -T "$svc" bash -c 'tail -15 /state/home*/daemon.log 2>/dev/null; true' >&2 || true
    done
    "${DC[@]}" logs --no-color relay1 relay2 2>&1 | tail -15 >&2 || true
    exit 1
}
cleanup() { [[ $KEEP == 1 ]] || "${DC[@]}" down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

run() { local svc=$1; shift; "${DC[@]}" exec -T "$svc" bash -c "$*"; }
ok() { PASSED=$((PASSED + 1)); }
section() { echo "== $*"; }
# expect_fail <svc> <cmd> <a|b|c>: the command must FAIL and mention one of the words.
expect_fail() {
    local svc=$1 cmd=$2 want=$3 out w
    if out=$(run "$svc" "$cmd" 2>&1); then fail "should have failed: $cmd  -> $out"; fi
    IFS='|' read -ra words <<<"$want"
    for w in "${words[@]}"; do [[ $out == *"$w"* ]] && { ok; return 0; }; done
    fail "wanted '$want' from: $cmd  -> $out"
}
# eventually <svc> <cmd>: retry up to 40s until the command succeeds.
eventually() {
    local svc=$1 cmd=$2
    for _ in $(seq 40); do run "$svc" "$cmd" >/dev/null 2>&1 && { ok; return 0; }; sleep 1; done
    fail "never became true on $svc: $cmd"
}
same() { [[ $1 == "$2" ]] || fail "$3: '$1' != '$2'"; ok; }
# as <svc> <name> <cmd>: run as a separate "machine" (own home, own listen port).
as() {
    local svc=$1 name=$2 port=$3; shift 3
    run "$svc" "export SWARM_HOME=/state/h-$name SWARM_LISTEN=127.0.0.1:$port; $*"
}
copy() { # copy <from-svc> <path> <to-svc> <path>: move a file between isolated containers
    run "$1" "cat '$2'" | run "$3" "mkdir -p \$(dirname '$4') && cat > '$4'"
}

"${DC[@]}" up -d --build >/dev/null 2>&1 || fail "could not start the topology"

section "the peers are isolated from each other, the relays reach everyone"
B_IP=$("${DC[@]}" exec -T peer-b sh -c "ip -4 -o addr show eth0 | awk '{print \$4}' | cut -d/ -f1")
[[ -n $B_IP ]] || fail "no address for peer-b"
if run peer-a "timeout 3 bash -c '</dev/tcp/$B_IP/42424'" 2>/dev/null; then fail "a reached b directly"; fi
run peer-a "getent hosts peer-b" >/dev/null 2>&1 && fail "a resolves peer-b"
for p in peer-a peer-b peer-c; do
    for r in relay1 relay2; do
        run "$p" "timeout 3 bash -c '</dev/tcp/$r/42425'" || fail "$p cannot reach $r"
    done
done
ok

section "every peer registers with both relays"
for p in peer-a peer-b peer-c; do
    run "$p" "swarm status >/dev/null 2>&1 || true"
done
for p in peer-a peer-b peer-c; do
    eventually "$p" 'swarm status --json | jq -e "[.relays[].registered] == [true,true]"'
done

section "A creates a repo with branches, a tag, big and odd files"
run peer-a 'set -e; mkdir -p /state/work/src && cd /state/work/src && git init -q -b main && swarm init >/dev/null
  echo one > one.txt; mkdir "dir with space"; echo spaced > "dir with space/ünï.txt"
  head -c 3000000 /dev/urandom > big.bin; ln -s one.txt link
  git add -A; git commit -q -m one
  git checkout -q -b feature; echo feat > feat.txt; git add -A; git commit -q -m feat; git checkout -q main
  git tag -a v1 -m "release one"
  git push -q swarm main feature v1'
LINK=$(run peer-a 'cd /state/work/src && swarm link')
REPO=${LINK#swarm://}; REPO=${REPO%%[?&]*}
[[ $LINK == *"h=relay:relay1:42425@"* && $LINK == *"h=relay:relay2:42425@"* ]] || fail "link lacks both relay hints: $LINK"
ok
A_HEAD=$(run peer-a 'cd /state/work/src && git rev-parse HEAD')
BIG=$(run peer-a 'sha256sum < /state/work/src/big.bin')

section "B and C clone at the same time through the relays"
run peer-b "cd /state/work && git clone -q '$LINK' b 2>&1" >/tmp/swarm5-b.log 2>&1 &
PB=$!
run peer-c "cd /state/work && git clone -q '$LINK' c 2>&1" >/tmp/swarm5-c.log 2>&1 &
PC=$!
wait $PB || { cat /tmp/swarm5-b.log; fail "B clone failed"; }
wait $PC || { cat /tmp/swarm5-c.log; fail "C clone failed"; }
for p in b c; do
    svc=peer-$p
    same "$(run $svc "git -C /state/work/$p rev-parse HEAD")" "$A_HEAD" "$p head"
    same "$(run $svc "sha256sum < /state/work/$p/big.bin")" "$BIG" "$p big file"
    same "$(run $svc "cat '/state/work/$p/dir with space/ünï.txt'")" spaced "$p odd path"
    same "$(run $svc "readlink /state/work/$p/link")" one.txt "$p symlink"
    run $svc "git -C /state/work/$p fsck --strict" >/dev/null 2>&1 || fail "$p fsck"
    run $svc "git -C /state/work/$p branch -r" | grep -q origin/feature || fail "$p lacks feature"
    same "$(run $svc "git -C /state/work/$p tag -l")" v1 "$p tag"
    same "$(run $svc "git -C /state/work/$p cat-file -t v1")" tag "$p annotated tag kept"
done

section "new commits, force pushes and deletions propagate"
run peer-a 'cd /state/work/src && echo two > two.txt && git add -A && git commit -q -m two && git push -q swarm main'
A_HEAD2=$(run peer-a 'cd /state/work/src && git rev-parse HEAD')
run peer-b 'cd /state/work/b && git pull -q --ff-only 2>&1'
same "$(run peer-b 'git -C /state/work/b rev-parse HEAD')" "$A_HEAD2" "B fast-forwards"
expect_fail peer-a 'cd /state/work/src && git reset -q --hard HEAD~1 && git push swarm main 2>&1' "non-fast-forward"
run peer-a 'cd /state/work/src && git push -q -f swarm main'
run peer-c 'cd /state/work/c && git fetch -q origin 2>&1'
same "$(run peer-c 'git -C /state/work/c rev-parse origin/main')" "$A_HEAD" "force push reached C"
run peer-a 'cd /state/work/src && git push -q swarm :feature'
run peer-c 'cd /state/work/c && git fetch -q --prune origin 2>&1'
run peer-c 'git -C /state/work/c branch -r' | grep -q origin/feature && fail "deleted branch survived on C"
ok
run peer-a 'cd /state/work/src && git checkout -q -b wip && echo w > w && git add -A && git commit -q -m wip && git push -q swarm wip'
run peer-a 'cd /state/work/src && git checkout -q main && git merge -q --no-edit wip 2>&1; git push -q swarm main'
A_HEAD=$(run peer-a 'cd /state/work/src && git rev-parse HEAD')

section "relay failover: relay1 down, a fresh clone still works through relay2"
"${DC[@]}" stop relay1 >/dev/null 2>&1
as peer-c f1 42601 "cd /state/work && git clone -q '$LINK' f1 2>&1"
same "$(run peer-c 'git -C /state/work/f1 rev-parse HEAD')" "$A_HEAD" "clone via relay2"
as peer-b f1 42602 "cd /state/work/b && echo bb > bb && git add -A && git commit -q -m bb; git fetch -q origin 2>&1"
ok

section "relay2 down as well: clean error, no hang, nothing stored"
"${DC[@]}" stop relay2 >/dev/null 2>&1
expect_fail peer-c "export SWARM_HOME=/state/h-f2 SWARM_LISTEN=127.0.0.1:42603 SWARM_FIND_TIMEOUT=5; cd /state/work && timeout 60 git clone '$LINK' f2 2>&1" "no_peers|auth_failed"
run peer-c 'test ! -e /state/work/f2/.git' || fail "a failed clone left a repo behind"
ok

section "relays return: peers re-register without a restart"
"${DC[@]}" start relay1 relay2 >/dev/null 2>&1
for p in peer-a peer-b peer-c; do
    eventually "$p" 'swarm status --json | jq -e "[.relays[].registered] == [true,true]"'
done
as peer-c f3 42604 "cd /state/work && git clone -q '$LINK' f3 2>&1"
same "$(run peer-c 'git -C /state/work/f3 rev-parse HEAD')" "$A_HEAD" "clone after recovery"

section "bad links fail cleanly"
BAD_REPO=$(printf 'a%.0s' $(seq 52))
expect_fail peer-c "export SWARM_HOME=/state/h-bad1 SWARM_LISTEN=127.0.0.1:42605 SWARM_FIND_TIMEOUT=5; cd /state/work && timeout 60 git clone 'swarm://$BAD_REPO' bad1 2>&1" "swarm:"
expect_fail peer-c "cd /state/work && git clone 'swarm://not-a-repo' bad2 2>&1" "swarm"
NODE=${LINK##*@}
WRONG_NODE=$(printf 'b%.0s' $(seq 52))
expect_fail peer-c "export SWARM_HOME=/state/h-bad3 SWARM_LISTEN=127.0.0.1:42606 SWARM_RELAYS= SWARM_FIND_TIMEOUT=5; cd /state/work && timeout 60 git clone 'swarm://$REPO?h=relay:relay1:42425@$WRONG_NODE' bad3 2>&1" "swarm:"
expect_fail peer-c "export SWARM_HOME=/state/h-bad4 SWARM_LISTEN=127.0.0.1:42607 SWARM_RELAY_MODE=off SWARM_FIND_TIMEOUT=5; cd /state/work && timeout 60 git clone '$LINK' bad4 2>&1" "swarm:"

section "relay-only link works even when the cloner has no relays of its own"
as peer-c r1 42608 "export SWARM_RELAYS=; cd /state/work && git clone -q '$LINK' r1 2>&1"
same "$(run peer-c 'git -C /state/work/r1 rev-parse HEAD')" "$A_HEAD" "hint-only clone"

section "maintainers: scoped refs, conflicts, accept, removal (via relays)"
KEY_B=$(run peer-b "swarm key new $REPO")
KEY_C=$(run peer-c "swarm key new $REPO")
[[ -n $KEY_B && -n $KEY_C && $KEY_B != "$KEY_C" ]] || fail "maintainer keys"
ok
run peer-b 'cd /state/work/b && git checkout -q -b dev/x && echo b > bfile && git add -A && git commit -q -m b1'
expect_fail peer-b 'cd /state/work/b && git push origin dev/x 2>&1' "unauthorized"
run peer-a "swarm maintainers add $REPO $KEY_B 'refs/heads/dev/*' 'refs/heads/shared/*' >/dev/null"
run peer-a "swarm maintainers add $REPO $KEY_C 'refs/heads/shared/*' >/dev/null"
run peer-a "swarm maintainers list $REPO" | grep -q "${KEY_B:0:8}" || fail "maintainers list lacks B"
ok
run peer-b "cd /state/work/b && git fetch -q origin 2>&1"
run peer-c "cd /state/work/c && git fetch -q origin 2>&1"
run peer-b "swarm key id $REPO 2>&1" | grep -q "is maintainer" || fail "B did not learn it is a maintainer"
ok
run peer-b 'cd /state/work/b && git push -q origin dev/x'
eventually peer-a "cd /state/work/src && swarm seed '$LINK' >/dev/null 2>&1; git ls-remote swarm | grep -q refs/heads/dev/x"
run peer-b 'cd /state/work/b && git checkout -q main && echo evil > evil && git add -A && git commit -q -m evil'
expect_fail peer-b 'cd /state/work/b && git push -f origin main 2>&1' "out_of_scope"
expect_fail peer-b "swarm maintainers add $REPO $KEY_C 2>&1" "unauthorized"
expect_fail peer-c 'cd /state/work/c && git checkout -q -b dev/c && echo c > c && git add -A && git commit -q -m c && git push origin dev/c 2>&1' "out_of_scope"
run peer-b 'cd /state/work/b && git checkout -q -B shared/t dev/x && echo s > s && git add -A && git commit -q -m s0 && git push -q -f origin shared/t'
eventually peer-a "swarm seed '$LINK' >/dev/null 2>&1; git -C /state/work/src ls-remote swarm | grep -q refs/heads/shared/t"
run peer-c 'cd /state/work/c && git fetch -q origin shared/t 2>&1; git checkout -q -B shared/t FETCH_HEAD && echo c > cfile && git add -A && git commit -q -m c1 && git push -q origin shared/t'
run peer-b 'cd /state/work/b && echo b2 > bfile2 && git add -A && git commit -q -m b2 && git push -q -f origin shared/t'
B_TIP=$(run peer-b 'git -C /state/work/b rev-parse shared/t')
C_TIP=$(run peer-c 'git -C /state/work/c rev-parse shared/t')
eventually peer-a "swarm seed '$LINK' >/dev/null 2>&1; swarm status --json | jq -e '.repos[0].conflicts | index(\"refs/heads/shared/t\")'"
NOW=$(run peer-a 'cd /state/work/src && git ls-remote swarm refs/heads/shared/t | cut -f1')
same "$NOW" "$C_TIP" "canonical keeps the first value during a conflict"
run peer-a "swarm accept $REPO refs/heads/shared/t $KEY_B >/dev/null"
same "$(run peer-a 'cd /state/work/src && git ls-remote swarm refs/heads/shared/t | cut -f1')" "$B_TIP" "accept took B's value"
run peer-a "swarm maintainers remove $REPO $KEY_B >/dev/null"
run peer-a 'cd /state/work/src && git ls-remote swarm' | grep -q refs/heads/dev/x && fail "dev/x survived removal"
ok

section "bundles: carried out of band, verified, tamper-proof, never regress"
run peer-a "swarm export $REPO -o /state/repo.swarmbundle >/dev/null"
copy peer-a /state/repo.swarmbundle peer-c /state/in/repo.swarmbundle
same "$(run peer-a 'sha256sum < /state/repo.swarmbundle')" "$(run peer-c 'sha256sum < /state/in/repo.swarmbundle')" "bundle transfer intact"
as peer-c bun 42609 "export SWARM_RELAYS= SWARM_RELAY_MODE=off; swarm import /state/in/repo.swarmbundle >/dev/null; cd /state/work && git clone -q 'swarm://$REPO' bun 2>&1"
same "$(run peer-c 'git -C /state/work/bun rev-parse HEAD')" "$A_HEAD" "bundle clone head"
SIZE=$(run peer-c 'stat -c %s /state/in/repo.swarmbundle')
for where in 3 20 100 $((SIZE - 100)); do
    run peer-c "cp /state/in/repo.swarmbundle /state/in/bad.swarmbundle && printf '\\xff' | dd of=/state/in/bad.swarmbundle bs=1 seek=$where conv=notrunc 2>/dev/null"
    expect_fail peer-c "SWARM_HOME=/state/h-tamper$where SWARM_LISTEN=127.0.0.1:426$((10 + where % 80)) swarm import /state/in/bad.swarmbundle 2>&1" "swarm"
    run peer-c "ls /state/h-tamper$where/repos 2>/dev/null | wc -l" | grep -qx 0 || fail "tampered import left a store (byte $where)"
done
run peer-c "head -c $((SIZE / 2)) /state/in/repo.swarmbundle > /state/in/trunc.swarmbundle"
expect_fail peer-c "SWARM_HOME=/state/h-trunc SWARM_LISTEN=127.0.0.1:42700 swarm import /state/in/trunc.swarmbundle 2>&1" "swarm"
run peer-c "echo not a bundle > /state/in/junk.swarmbundle"
expect_fail peer-c "SWARM_HOME=/state/h-junk SWARM_LISTEN=127.0.0.1:42701 swarm import /state/in/junk.swarmbundle 2>&1" "swarm"
expect_fail peer-c "SWARM_HOME=/state/h-nofile SWARM_LISTEN=127.0.0.1:42702 swarm import /state/in/missing.swarmbundle 2>&1" "swarm"
run peer-a 'cd /state/work/src && echo three > three && git add -A && git commit -q -m three && git push -q swarm main'
A_HEAD3=$(run peer-a 'cd /state/work/src && git rev-parse HEAD')
as peer-c bun 42609 "swarm import /state/in/repo.swarmbundle >/dev/null; git ls-remote 'swarm://$REPO' refs/heads/main | cut -f1" >/dev/null
run peer-a "swarm export $REPO -o /state/new.swarmbundle >/dev/null"
copy peer-a /state/new.swarmbundle peer-c /state/in/new.swarmbundle
as peer-c bun 42609 "swarm import /state/in/new.swarmbundle >/dev/null; swarm import /state/in/repo.swarmbundle >/dev/null"
same "$(as peer-c bun 42609 "git ls-remote 'swarm://$REPO' refs/heads/main | cut -f1")" "$A_HEAD3" "an older bundle did not rewind refs"

section "fork, unseed, gc"
FORK=$(run peer-c "cd /state/work && SWARM_HOME=/state/h-fork SWARM_LISTEN=127.0.0.1:42703 swarm fork '$LINK' --json" | jq -r .repo_id)
[[ -n $FORK && $FORK != "$REPO" && $FORK != null ]] || fail "fork did not make a new repo: $FORK"
ok
run peer-c "SWARM_HOME=/state/h-fork SWARM_LISTEN=127.0.0.1:42703 swarm status --json" | jq -e --arg f "$FORK" '.repos[] | select(.repo==$f)' >/dev/null || fail "fork not in status"
ok
run peer-c "swarm unseed $REPO >/dev/null"
run peer-c "swarm status --json" | jq -e --arg r "$REPO" '.repos[] | select(.repo==$r) | .seeding == false' >/dev/null || fail "unseed did not stop seeding"
run peer-c "swarm gc $REPO >/dev/null 2>&1; git -C /state/work/c fsck --strict" >/dev/null 2>&1 || fail "gc broke a clone"
ok
run peer-c "swarm seed '$LINK' >/dev/null"
run peer-c "swarm status --json" | jq -e --arg r "$REPO" '.repos[] | select(.repo==$r) | .seeding == true' >/dev/null || fail "reseed failed"
ok

section "YAML config: file sets the relays, the environment overrides it"
run peer-c "mkdir -p /etc/swarmgit && printf 'relays:\n  - relay1:42425\n  - relay2:42425\n' > /etc/swarmgit/config.yaml"
as peer-c yml 42704 "unset SWARM_RELAYS; swarm status >/dev/null 2>&1; sleep 3; swarm status --json" | jq -e '[.relays[].relay] == ["relay1:42425","relay2:42425"]' >/dev/null || fail "config.yaml relays not used"
ok
as peer-c yml2 42705 "export SWARM_RELAYS=relay2:42425; swarm status >/dev/null 2>&1; sleep 3; swarm status --json" | jq -e '[.relays[].relay] == ["relay2:42425"]' >/dev/null || fail "env did not override config.yaml"
ok
run peer-c "rm -f /etc/swarmgit/config.yaml"

section "daemon restart: A stops, comes back, peers still fetch from it"
run peer-a "swarm daemon stop >/dev/null 2>&1; sleep 1; swarm status >/dev/null 2>&1"
eventually peer-a 'swarm status --json | jq -e "[.relays[].registered] == [true,true]"'
as peer-b rs 42706 "cd /state/work && git clone -q '$LINK' rs 2>&1"
same "$(run peer-b 'git -C /state/work/rs rev-parse HEAD')" "$A_HEAD3" "clone after A restart"

section "recovery key: wrong key rejected, rotation moves the root"
run peer-a "swarm key recovery $REPO -o /state/recovery.key >/dev/null 2>&1"
[[ $(run peer-a 'stat -c %a /state/recovery.key') == 600 ]] || fail "recovery key mode"
ok
run peer-c "swarm key recovery $FORK -o /state/other.key >/dev/null 2>&1" || true
as peer-b rot 42707 "cd /state/work && git clone -q '$LINK' rot 2>&1; swarm status --json" | jq -e '.repos[0].recovery == true' >/dev/null || fail "B does not see the recovery key"
ok
copy peer-a /state/recovery.key peer-b /state/recovery.key
run peer-b "chmod 600 /state/recovery.key"
run peer-c "SWARM_HOME=/state/h-fork SWARM_LISTEN=127.0.0.1:42703 swarm key recovery $FORK -o /state/other.key >/dev/null 2>&1"
copy peer-c /state/other.key peer-b /state/other.key
run peer-b "chmod 600 /state/other.key"
expect_fail peer-b "export SWARM_HOME=/state/h-rot SWARM_LISTEN=127.0.0.1:42707; swarm key rotate $REPO --recovery /state/other.key 2>&1" "swarm"
OLD_ROOT=$(as peer-b rot 42707 "swarm status --json" | jq -r '.repos[0].root')
NEW_ROOT=$(as peer-b rot 42707 "swarm key rotate $REPO --recovery /state/recovery.key")
[[ -n $NEW_ROOT && $NEW_ROOT != "$OLD_ROOT" ]] || fail "no new root"
ok
as peer-b rot 42707 "cd /state/work/rot && echo four > four && git add -A && git commit -q -m four && git push -q origin main"
B_HEAD=$(run peer-b 'git -C /state/work/rot rev-parse HEAD')
NEW_LINK=$(as peer-b rot 42707 "swarm link $REPO")
as peer-c rot 42708 "cd /state/work && git clone -q '$NEW_LINK' rot 2>&1"
same "$(run peer-c 'git -C /state/work/rot rev-parse HEAD')" "$B_HEAD" "C follows the rotation"
run peer-a "swarm seed '$NEW_LINK' >/dev/null 2>&1 || true"
run peer-a 'cd /state/work/src && echo five > five && git add -A && git commit -q -m five'
expect_fail peer-a 'cd /state/work/src && git push -f swarm main 2>&1' "unauthorized"

section "static mirror on relay1: clone with no peers and no relays"
as peer-b rot 42707 "rm -rf /state/www && swarm mirror export $REPO /state/www >/dev/null"
run peer-b "test -s /state/www/$REPO/repo.swarmbundle" || fail "no mirror file"
ok
run peer-b "tar -C /state/www -cf - ." | run relay1 "rm -rf /www && mkdir -p /www && tar -C /www -xf -"
run relay1 "busybox httpd -p 8080 -h /www"
# tampered copy on 8081; the wrong repo's bundle at this repo's path on 8082
run relay1 "rm -rf /www2 && cp -r /www /www2 && printf '\\xff' | dd of=/www2/$REPO/repo.swarmbundle bs=1 seek=200 conv=notrunc 2>/dev/null; busybox httpd -p 8081 -h /www2"
run peer-c "SWARM_HOME=/state/h-fork SWARM_LISTEN=127.0.0.1:42703 swarm export $FORK -o /state/fork.swarmbundle >/dev/null"
copy peer-c /state/fork.swarmbundle relay1 /www3/$REPO/repo.swarmbundle
run relay1 "busybox httpd -p 8082 -h /www3"
NOPEERS="export SWARM_RELAYS= SWARM_RELAY_MODE=off SWARM_FIND_TIMEOUT=5"
as peer-c mir 42710 "$NOPEERS; cd /state/work && git clone -q 'swarm://$REPO?h=http:relay1:8080' mir 2>&1"
same "$(run peer-c 'git -C /state/work/mir rev-parse HEAD')" "$B_HEAD" "mirror clone head"
as peer-c mir 42710 "swarm status --json" | jq -e --arg r "$NEW_ROOT" '.repos[0].root == $r' >/dev/null || fail "mirror clone ignored the rotation"
ok
expect_fail peer-c "export SWARM_HOME=/state/h-mir2 SWARM_LISTEN=127.0.0.1:42711; $NOPEERS; cd /state/work && timeout 60 git clone 'swarm://$REPO?h=http:relay1:8081' mir2 2>&1" "swarm"
expect_fail peer-c "export SWARM_HOME=/state/h-mir3 SWARM_LISTEN=127.0.0.1:42712; $NOPEERS; cd /state/work && timeout 60 git clone 'swarm://$REPO?h=http:relay1:8082' mir3 2>&1" "swarm"
expect_fail peer-c "export SWARM_HOME=/state/h-mir4 SWARM_LISTEN=127.0.0.1:42713; $NOPEERS; cd /state/work && timeout 60 git clone 'swarm://$REPO?h=http:relay1:8999' mir4 2>&1" "swarm"
as peer-c mir5 42714 "$NOPEERS; export SWARM_MIRRORS=http://relay1:8080; cd /state/work && git clone -q 'swarm://$REPO' mir5 2>&1"
same "$(run peer-c 'git -C /state/work/mir5 rev-parse HEAD')" "$B_HEAD" "SWARM_MIRRORS clone head"

echo "PASS ($PASSED checks)"
