#!/usr/bin/env bash
# Merge requests: opened by a contributor without a repo key through a plain
# git push, discussed and reviewed over the network, merged by the owner.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT_A=42571
PORT_B=42572
PORT_C=42573
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
C_HOME=$WORK/c

cleanup() {
    [[ -z ${KEEP_LOGS:-} ]] || tail -n 60 "$A_HOME"/*.log "$B_HOME"/*.log >&2 || true
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

# Wait until the output of a command holds the wanted text.
eventually() {
    local want=$1 have=
    shift
    for _ in $(seq 50); do
        have=$("$@" 2>&1) || true
        grep -qF -- "$want" <<<"$have" && return 0
        sleep 0.2
    done
    swarm_fail "never saw '$want' in: $have"
}

first_id() { grep -oE '^[0-9a-f]{8}' | head -n 1; }

mkdir -p "$WORK/src"
cd "$WORK/src"
git init -q -b main
git config user.name Ann
a swarm init >/dev/null
LINK=$(a swarm link)
REPO=${LINK#swarm://}
REPO=${REPO%%\?*}
printf 'one\ntwo\n' >file.txt
git add file.txt
git commit -qm "first"
a git push -q swarm main

echo "== a contributor with no repo key proposes a branch with git push"
b git clone -q "$LINK" "$WORK/bob"
cd "$WORK/bob"
git config user.name Bob
git checkout -qb feature
printf 'three\n' >>file.txt
git commit -qam "Add a third line" -m "It was missing."
DENIED=$(b git push origin feature:main 2>&1) && swarm_fail "pushed to main without a key"
expect "$DENIED" "swarm mr create"
OPENED=$(b git push origin HEAD:refs/for/main 2>&1)
expect "$OPENED" "opened merge request"
expect "$OPENED" "1 peer has it"
MR=$(b swarm mr list | first_id)
[[ -n $MR ]] || swarm_fail "no merge request listed"

echo "== the owner sees it"
cd "$WORK/src"
LIST=$(a swarm mr list)
expect "$LIST" "$MR  open    Add a third line"
expect "$LIST" "Bob (contributor"
SHOW=$(a swarm mr show "$MR")
expect "$SHOW" "It was missing."
expect "$SHOW" "wants to merge feature into main"
expect "$SHOW" "commits:"
expect "$(a swarm mr show feature)" "merge request $MR"
expect "$(a swarm mr diff "$MR")" "+three"
expect "$(a swarm status)" "1 open merge request"

echo "== comments and a review reach the author"
a swarm mr comment "$MR" -m "Why a third line?" >/dev/null
a swarm mr comment "$MR" --file file.txt --line 3 -m "Spell it 3." >/dev/null
a swarm mr review "$MR" --request-changes -m "Please use a digit." >/dev/null
if a swarm mr comment "$MR" --file file.txt --line 9 -m x >/dev/null 2>&1; then
    swarm_fail "a comment on a missing line was taken"
fi
cd "$WORK/bob"
# Without a name, the current branch is meant.
SHOW=$(b swarm mr show)
expect "$SHOW" "Why a third line?"
expect "$SHOW" "commented on file.txt:3"
expect "$SHOW" "Ann (owner"
expect "$SHOW" "requested changes"
expect "$SHOW" "changes requested: 1"
EVENT=$(grep -F "Ann (owner" <<<"$SHOW" | head -n 1 | sed 's/^\[\([0-9a-f]*\)\].*/\1/')
b swarm mr comment "$MR" --reply-to "$EVENT" -m "Good point." >/dev/null
if b swarm mr review "$MR" --approve >/dev/null 2>&1; then
    swarm_fail "an author approved their own merge request"
fi

echo "== pushing again updates the same merge request"
sed -i 's/^three$/3/' file.txt
git commit -qam "Use a digit"
expect "$(b git push origin HEAD:refs/for/main 2>&1)" "updated merge request $MR"
expect "$(b git push origin HEAD:refs/for/main 2>&1)" "already proposes"
cd "$WORK/src"
SHOW=$(a swarm mr show "$MR")
expect "$SHOW" "Good point."
expect "$SHOW" "in reply to $EVENT"
expect "$SHOW" "(outdated)"
expect "$SHOW" "changes requested: 0"

echo "== a stranger may comment but not close, and a seeder passes it on"
c git clone -q "$LINK" "$WORK/carol"
cd "$WORK/carol"
git config user.name Carol
if c swarm mr close "$MR" >/dev/null 2>&1; then
    swarm_fail "a stranger closed the merge request"
fi
c swarm mr comment "$MR" -m "Looks fine to me." >/dev/null
cd "$WORK/src"
eventually "Looks fine to me." a swarm mr show "$MR"
# The events are ordinary git refs, fetched the ordinary way.
a git fetch -q swarm 'refs/mr/*:refs/mr/*'
EVENTS=$(git for-each-ref --format='%(refname)' refs/mr | wc -l)
[[ $EVENTS -eq 3 ]] || swarm_fail "wanted 3 author chains, got $EVENTS"
FIRST=$(git rev-parse "$(git for-each-ref --count=1 --format='%(refname)' refs/mr)")
git cat-file commit "$FIRST" | grep -q '^swarmsig ' || swarm_fail "unsigned event"
git fsck --no-progress >/dev/null || swarm_fail "fsck"

echo "== approve and merge"
cd "$WORK/src"
a swarm mr review "$MR" --approve -m "Thanks." >/dev/null
expect "$(a swarm mr show "$MR")" "approvals: 1"
if (cd "$WORK/bob" && b swarm mr merge "$MR" >/dev/null 2>&1); then
    swarm_fail "merged without a key"
fi
a swarm mr merge "$MR" >/dev/null
expect "$(git log --format=%s -1)" "Merge merge request $MR: Add a third line"
expect "$(a swarm mr list)" "no open merge requests"
cd "$WORK/bob"
eventually "$MR  merged" b swarm mr list --all

echo "== close and reopen"
git checkout -q main
git pull -q origin main
git checkout -qb other
git commit -q --allow-empty -m "Another idea"
git checkout -q main
b swarm mr create other --title "An idea" -m "Maybe." >/dev/null
git checkout -q other
OTHER=$(b swarm mr list | first_id)
cd "$WORK/src"
expect "$(a swarm mr show "$OTHER")" "An idea"
a swarm mr close other -m "Not now." >/dev/null
cd "$WORK/bob"
eventually "$OTHER  closed" b swarm mr list --all
b swarm mr reopen "$OTHER" >/dev/null
b swarm mr checkout "$OTHER" >/dev/null 2>&1
[[ $(git symbolic-ref --short HEAD) == "mr/$OTHER" ]] || swarm_fail "not on mr branch"
cd "$WORK/src"
eventually "$OTHER  open" a swarm mr list

echo "== push options set the title and description"
cd "$WORK/bob"
git checkout -q main
git checkout -qb opts
git commit -q --allow-empty -m "wip"
b git push -q -o 'title=Say "hello" é' -o body="Given by hand." origin HEAD:refs/for/main 2>/dev/null
SHOW=$(b swarm mr list)
OPTS=$(grep -F 'Say "hello" é' <<<"$SHOW" | first_id)
[[ -n $OPTS ]] || swarm_fail "title option not used: $SHOW"
expect "$(b swarm mr show "$OPTS")" "Given by hand."
cd "$WORK/src"

echo "== a branch in a fork is proposed to the original repo"
MINE=$(c swarm fork "$LINK")
c git clone -q "$MINE" "$WORK/carol-fork"
cd "$WORK/carol-fork"
git config user.name Carol
git checkout -qb from-fork
git commit -q --allow-empty -m "Made in a fork"
expect "$(c swarm mr create -R "$LINK")" "opened merge request"
git commit -q --allow-empty -m "More"
expect "$(c swarm mr create -R "$LINK")" "updated merge request"
cd "$WORK/src"
eventually "from-fork into main" a swarm mr list
expect "$(a swarm mr show from-fork)" "Made in a fork"

echo "== a fork does not carry merge requests"
eventually "$OPTS  open" a swarm mr list
FORK=$(a swarm fork "$LINK")
FORK=${FORK#swarm://}
expect "$(a swarm mr list -R "${FORK%%\?*}" --offline --all)" "no merge requests"

echo "== a bundle does not carry them either"
a swarm export "$REPO" -o "$WORK/repo.swarmbundle" >/dev/null
if grep -qa 'refs/mr/' "$WORK/repo.swarmbundle"; then
    swarm_fail "the bundle lists merge request refs"
fi

echo "== merge requests survive a restart"
a swarm daemon stop >/dev/null
expect "$(a swarm mr list --offline --all)" "$MR  merged"

echo "MR PASS"
