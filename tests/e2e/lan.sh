#!/usr/bin/env bash
# M3 acceptance from spec 11.4: two network namespaces joined by a veth pair,
# A and B run with no hints, B clones from A over LAN discovery alone, and no
# LAN packet carries the repo ID.
#
# Needs root (network namespaces). Skipped otherwise. In the dev container:
#   docker compose -f compose.yaml -f compose.privileged.yaml run --rm \
#       --user root dev tests/e2e/lan.sh

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

if [[ $(id -u) -ne 0 ]]; then
    echo "SKIP: lan.sh needs root for network namespaces"
    exit 0
fi
for tool in ip tcpdump python3; do
    command -v "$tool" >/dev/null || { echo "SKIP: lan.sh needs $tool"; exit 0; }
done

# This test exercises route B only.
unset SWARM_NO_LAN
export SWARM_NO_DHT=1

NS_A=swg-lan-a-$$
NS_B=swg-lan-b-$$
VETH_A=swga$$
VETH_B=swgb$$
WORK=$(mktemp -d)
A_HOME=$WORK/a
B_HOME=$WORK/b
PCAP=$WORK/lan.pcap
TCPDUMP=

cleanup() {
    set +e
    [[ -n $TCPDUMP ]] && kill "$TCPDUMP" 2>/dev/null
    for home in "$A_HOME" "$B_HOME"; do
        if [[ -f $home/daemon.json ]]; then
            pid=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pid"])' \
                "$home/daemon.json" 2>/dev/null)
            [[ -n $pid ]] && kill "$pid" 2>/dev/null
        fi
    done
    ip netns pids "$NS_A" 2>/dev/null | xargs -r kill 2>/dev/null
    ip netns pids "$NS_B" 2>/dev/null | xargs -r kill 2>/dev/null
    ip netns del "$NS_A" 2>/dev/null
    ip netns del "$NS_B" 2>/dev/null
}
trap cleanup EXIT

repo_of() {
    local link=$1
    link=${link#swarm://}
    printf '%s' "${link%%\?*}"
}

in_a() { ip netns exec "$NS_A" env SWARM_HOME="$A_HOME" "$@"; }
in_b() { ip netns exec "$NS_B" env SWARM_HOME="$B_HOME" "$@"; }

echo "== two namespaces on one veth link"
ip netns add "$NS_A"
ip netns add "$NS_B"
ip link add "$VETH_A" type veth peer name "$VETH_B"
ip link set "$VETH_A" netns "$NS_A"
ip link set "$VETH_B" netns "$NS_B"
ip -n "$NS_A" addr add 10.231.0.1/24 dev "$VETH_A"
ip -n "$NS_B" addr add 10.231.0.2/24 dev "$VETH_B"
for ns in "$NS_A" "$NS_B"; do
    ip -n "$ns" link set lo up
done
ip -n "$NS_A" link set "$VETH_A" up
ip -n "$NS_B" link set "$VETH_B" up
# No default route and no multicast route: the daemon must pick the interface.

ip netns exec "$NS_A" tcpdump -i "$VETH_A" -s 0 -U -w "$PCAP" udp port 42099 \
    >/dev/null 2>&1 &
TCPDUMP=$!
sleep 1

echo "== A creates and pushes, with no hints anywhere"
SRC=$WORK/src
mkdir -p "$SRC"
cd "$SRC"
git init -q -b main
in_a swarm init >/dev/null
REPO=$(repo_of "$(in_a swarm link)")
git commit -q --allow-empty -m one
echo lan >file.txt
git add file.txt
git commit -q -m two
in_a git push -q swarm main
HEAD_A=$(git rev-parse HEAD)

echo "== B clones swarm://<repo> with no hint and finds A on the LAN"
mkdir -p "$WORK/clone"
cd "$WORK/clone"
in_b timeout 90 git clone -q "swarm://$REPO" out || swarm_fail "LAN clone failed"
[[ $(git -C out rev-parse HEAD) == "$HEAD_A" ]] || swarm_fail "clone HEAD differs"
git -C out fsck --no-progress --strict >/dev/null 2>&1 || swarm_fail "clone fsck failed"
PEER=$(sqlite3 "$B_HOME/swarm.db" "SELECT addr FROM peers LIMIT 1;" 2>/dev/null || true)
echo "   B learned peer ${PEER:-?}"

echo "== B pulls a new commit, still over LAN only"
cd "$SRC"
git commit -q --allow-empty -m three
in_a git push -q swarm main
HEAD_A2=$(git rev-parse HEAD)
cd "$WORK/clone/out"
in_b timeout 90 git pull -q --no-rebase origin main || swarm_fail "LAN pull failed"
[[ $(git rev-parse HEAD) == "$HEAD_A2" ]] || swarm_fail "pull HEAD differs"

echo "== status reports the LAN route"
in_a swarm status --json | python3 -c '
import json, sys
lan = json.load(sys.stdin).get("lan", {})
assert lan.get("enabled") is True, lan
' || swarm_fail "status does not report lan enabled"

sleep 1
kill "$TCPDUMP" 2>/dev/null || true
wait "$TCPDUMP" 2>/dev/null || true
TCPDUMP=

echo "== no LAN packet contains the repo ID"
[[ -s $PCAP ]] || swarm_fail "tcpdump captured nothing"
python3 - "$PCAP" "$REPO" <<'PY' || exit 1
import base64, binascii, struct, sys

data = open(sys.argv[1], "rb").read()
repo = sys.argv[2]
raw = base64.b32decode(repo.upper() + "=" * ((8 - len(repo) % 8) % 8))
needles = [raw, repo.lower().encode(), repo.upper().encode(),
           binascii.hexlify(raw), binascii.hexlify(raw).upper()]

magic = struct.unpack("<I", data[:4])[0]
endian = "<" if magic in (0xA1B2C3D4, 0xA1B23C4D) else ">"
off, packets, announces, queries = 24, 0, 0, 0
while off + 16 <= len(data):
    _, _, incl, _ = struct.unpack(endian + "IIII", data[off:off + 16])
    frame = data[off + 16:off + 16 + incl]
    off += 16 + incl
    packets += 1
    for needle in needles:
        if needle in frame:
            raise SystemExit(f"FAIL: LAN packet {packets} contains the repo ID")
    at = frame.find(b"SWG1")
    if at >= 0 and at + 5 <= len(frame):
        announces += frame[at + 4] == 1
        queries += frame[at + 4] == 2
for needle in needles:
    if needle in data:
        raise SystemExit("FAIL: capture contains the repo ID")
if announces == 0 or queries == 0:
    raise SystemExit(f"FAIL: expected announces and queries, got {announces}/{queries}")
print(f"   {packets} LAN packets: {announces} announces, {queries} queries, no repo ID")
PY

echo "LAN PASS"
