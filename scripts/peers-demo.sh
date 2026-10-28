#!/usr/bin/env bash
# Non-interactive tour of the two-peer setup in compose.peers.yaml.
#
#   ./scripts/peers-demo.sh          # start the peers and run the full flow
#   ./scripts/peers-demo.sh --reset  # wipe both peers first
#
# The interesting part is the commands it runs; docs/USER-GUIDE.md explains the
# same steps interactively.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
COMPOSE=(docker compose -f compose.peers.yaml)

if [[ ${1:-} == "--reset" ]]; then
    "${COMPOSE[@]}" down -v
fi

echo "== building and starting peer-a and peer-b"
"${COMPOSE[@]}" up -d --build

run_a() { "${COMPOSE[@]}" exec -T peer-a bash -c "$1"; }
run_b() { "${COMPOSE[@]}" exec -T peer-b bash -c "$1"; }

echo "== peer-a: create a repo and publish it"
run_a 'rm -rf /state/work/src && mkdir -p /state/work/src && cd /state/work/src \
  && git init -q -b main && swarm init >/dev/null \
  && printf "hello from A\n" > README.md && git add README.md \
  && git commit -q -m one && git push -q swarm main \
  && swarm link > /state/work/a.link'
LINK_A=$("${COMPOSE[@]}" exec -T peer-a cat /state/work/a.link | tr -d '\r')
echo "   link: $LINK_A"

head_a() { run_a 'git -C /state/work/src rev-parse HEAD' | tr -d '\r'; }
head_b() { run_b 'git -C /state/work/clone rev-parse HEAD' | tr -d '\r'; }

echo "== peer-b: clone through the link hint"
run_b "rm -rf /state/work/clone && cd /state/work && git clone -q '$LINK_A' clone \
  && git -C clone remote rename origin swarm \
  && git -C clone fsck --no-progress --strict"
[[ $(head_a) == $(head_b) ]] || { echo "clone HEAD differs" >&2; exit 1; }

echo "== copy A's repo key to B so B is allowed to publish"
"${COMPOSE[@]}" exec -T peer-a tar -C /state/home/keys -cf - . \
  | "${COMPOSE[@]}" exec -T peer-b bash -c 'mkdir -p /state/home/keys && tar -C /state/home/keys -xf -'

echo "== peer-b: commit and push"
run_b 'cd /state/work/clone && printf "hello from B\n" > b.txt && git add b.txt \
  && git commit -q -m two && git push -q swarm main'

echo "== peer-a: pull B's change from B's listener"
LINK_B=$(run_b 'cd /state/work/clone && swarm link' | tr -d '\r')
run_a "cd /state/work/src && git fetch -q '$LINK_B' main && git merge -q --ff-only FETCH_HEAD"
[[ $(head_a) == $(head_b) ]] || { echo "A did not get B's change" >&2; exit 1; }

echo "== peer-a: commit and push; peer-b pulls"
run_a 'cd /state/work/src && printf "hello again from A\n" > a.txt && git add a.txt \
  && git commit -q -m three && git push -q swarm main'
run_b 'cd /state/work/clone && git pull -q --no-rebase swarm main'
[[ $(head_a) == $(head_b) ]] || { echo "B did not get A's change" >&2; exit 1; }

echo
echo "PASS: clone and two-way push/pull worked."
echo "Shell into a peer with: docker compose -f compose.peers.yaml exec peer-a bash"
