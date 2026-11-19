#!/usr/bin/env bash
# M7 acceptance from spec 11.8: maintainers, scoped refs and conflicts.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PA=42711; PB=42712; PC=42713
WORK=$(mktemp -d)
A=$WORK/a; B=$WORK/b; C=$WORK/c
repo_of() { local l=${1#swarm://}; printf '%s' "${l%%[?&]*}"; }
as() { local home=$1 port=$2; shift 2; env SWARM_HOME=$home SWARM_LISTEN=127.0.0.1:$port "$@"; }
sync_from() { # machine home, port: pull the latest state through the helper
    as "$1" "$2" git -C "$3" fetch -q origin 2>/dev/null || true
}

# ----- A is the root -----
mkdir "$WORK/src"; cd "$WORK/src"
as $A $PA git init -q -b main
as $A $PA swarm init >/dev/null
REPO=$(repo_of "$(as $A $PA swarm link)")
echo base >base; git add .; git commit -q -m base
as $A $PA git push -q swarm main
HINT="swarm://$REPO?h=tcp:127.0.0.1:$PA"

# ----- B and C clone and make their own keys -----
cd "$WORK"
as $B $PB git clone -q "$HINT" b-out
as $C $PC git clone -q "$HINT" c-out
KEY_B=$(as $B $PB swarm key new "$REPO")
KEY_C=$(as $C $PC swarm key new "$REPO")

echo "== a clone without authorization cannot publish"
cd "$WORK/b-out"; git checkout -q -b dev/x; echo b >bfile; git add .; git commit -q -m b1
if as $B $PB git push -q origin dev/x 2>"$WORK/err"; then swarm_fail "unlisted key published"; fi
grep -q unauthorized "$WORK/err" || swarm_fail "no unauthorized error: $(cat "$WORK/err")"

echo "== the root authorizes B for dev/* and C for shared/*; B for shared/* too"
as $A $PA swarm maintainers add "$REPO" "$KEY_B" 'refs/heads/dev/*' 'refs/heads/shared/*' >/dev/null
as $A $PA swarm maintainers add "$REPO" "$KEY_C" 'refs/heads/shared/*' >/dev/null
as $B $PB swarm key id "$REPO" 2>&1 | grep -q maintainer || true
sync_from $B $PB "$WORK/b-out"; sync_from $C $PC "$WORK/c-out"
as $B $PB swarm key id "$REPO" 2>&1 | grep -q "is maintainer" || swarm_fail "B did not learn it is a maintainer"

echo "== B publishes inside its globs and the root sees it"
cd "$WORK/b-out"
as $B $PB git push -q origin dev/x
cd "$WORK/src"
for i in 1 2 3 4 5 6 7 8 9 10; do
    as $A $PA git ls-remote swarm | grep -q refs/heads/dev/x && break
    as $A $PA swarm seed "$HINT" >/dev/null 2>&1 || true
    sleep 1
done
as $A $PA git ls-remote swarm | grep -q refs/heads/dev/x || swarm_fail "root does not see dev/x"

echo "== B pushing outside its globs gets out_of_scope"
cd "$WORK/b-out"; git checkout -q main; echo evil >evil; git add .; git commit -q -m evil
if as $B $PB git push -q origin main 2>"$WORK/err"; then swarm_fail "out of scope push accepted"; fi
grep -q out_of_scope "$WORK/err" || swarm_fail "no out_of_scope: $(cat "$WORK/err")"

echo "== a maintainer cannot change maintainers"
if as $B $PB swarm maintainers add "$REPO" "$KEY_C" 2>"$WORK/err"; then swarm_fail "maintainer edited maintainers"; fi
grep -q unauthorized "$WORK/err" || swarm_fail "no unauthorized: $(cat "$WORK/err")"

echo "== two maintainers diverging on one ref conflict; canonical keeps its old value"
cd "$WORK/b-out"; git checkout -q -B shared/t dev/x; echo base2 >s; git add .; git commit -q -m s0
as $B $PB git push -q -f origin shared/t
cd "$WORK/src"
for i in 1 2 3 4 5 6 7 8 9 10; do
    as $A $PA swarm seed "$HINT" >/dev/null 2>&1 || true
    as $A $PA git ls-remote swarm | grep -q refs/heads/shared/t && break
    sleep 1
done
BASE=$(as $A $PA git ls-remote swarm refs/heads/shared/t | cut -f1)
[[ -n $BASE ]] || swarm_fail "root does not see shared/t"
cd "$WORK/c-out"; as $C $PC git fetch -q origin shared/t 2>/dev/null; git checkout -q -B shared/t FETCH_HEAD
echo c >cfile; git add .; git commit -q -m c1; as $C $PC git push -q origin shared/t
cd "$WORK/b-out"; echo b2 >bfile2; git add .; git commit -q -m b2; as $B $PB git push -q -f origin shared/t
cd "$WORK/src"
for i in 1 2 3 4 5 6 7 8 9 10; do
    as $A $PA swarm seed "$HINT" >/dev/null 2>&1 || true
    as $A $PA swarm status --json | jq -e '.repos[0].conflicts | index("refs/heads/shared/t")' >/dev/null && break
    sleep 1
done
as $A $PA swarm status --json | jq -e '.repos[0].conflicts | index("refs/heads/shared/t")' >/dev/null \
    || { as $A $PA swarm seed "swarm://$REPO" >&2; as $A $PA swarm status --json >&2; for st in $A $B $C; do for m in $st/repos/*/manifests/*.cbor; do echo $m >&2; done; done; for h in $B $C; do ls $h/repos/*/manifests >&2; done; ls $A/repos/*/manifests >&2; swarm_fail "no conflict reported"; }
NOW=$(as $A $PA git ls-remote swarm refs/heads/shared/t | cut -f1)
C_TIP=$(git -C "$WORK/c-out" rev-parse shared/t); B_TIP=$(git -C "$WORK/b-out" rev-parse shared/t)
[[ $NOW == "$C_TIP" ]] || swarm_fail "canonical changed during the conflict (got $NOW, want the first value $C_TIP)"

echo "== swarm accept picks one signer's value"
as $A $PA swarm accept "$REPO" refs/heads/shared/t "$KEY_B" >/dev/null
NOW=$(as $A $PA git ls-remote swarm refs/heads/shared/t | cut -f1)
[[ $NOW == "$B_TIP" ]] || swarm_fail "accept did not take B's value"

echo "== removing a maintainer makes its refs disappear"
as $A $PA swarm maintainers remove "$REPO" "$KEY_B" >/dev/null
as $A $PA git ls-remote swarm | grep -q refs/heads/dev/x && swarm_fail "dev/x survived the removal"
echo PASS
