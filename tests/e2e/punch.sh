#!/usr/bin/env bash
# UDP hole punching between two NATed machines, with the relay as the way
# back. Two peers sit behind their own Linux NAT routers (network namespaces);
# the relay is on the shared "internet" between them. Nothing leaves the
# container. The relay's byte counter says which way the objects really went.
#
# Needs root, ip and iptables; scripts/ci.sh runs it in the privileged dev
# container.

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
export PATH=$PATH:/usr/sbin:/sbin

if [[ $(id -u) -ne 0 ]] || ! command -v iptables >/dev/null || ! command -v ip >/dev/null; then
    echo "SKIP: the NAT test needs root, ip and iptables"
    exit 0
fi

WORK=$(mktemp -d)
RPORT=42781 MPORT=42782
NET=10.233.0
RELAY_NAME=relay.swarm.test
ID=$$
BRIDGE=swp$ID
NAT_A=swg-nata-$ID NAT_B=swg-natb-$ID A=swg-pa-$ID B=swg-pb-$ID
PIDS=()
HOMES=()

cleanup() {
    set +e
    for entry in "${HOMES[@]}"; do
        read -r ns home <<<"$entry"
        ip netns exec "$ns" env SWARM_HOME="$home" swarm daemon stop >/dev/null 2>&1
    done
    kill "${PIDS[@]}" 2>/dev/null
    for ns in "$A" "$B" "$NAT_A" "$NAT_B"; do
        ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
        ip netns del "$ns" 2>/dev/null
        rm -rf "/etc/netns/${ns:?}"
    done
    ip link del "$BRIDGE" 2>/dev/null
}
trap cleanup EXIT

wait_for() { # wait_for WHAT cmd...
    local what=$1 i
    shift
    for i in $(seq 200); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    swarm_fail "timed out waiting for $what"
}

ip link add "$BRIDGE" type bridge
ip addr add "$NET.1/24" dev "$BRIDGE"
ip link set "$BRIDGE" up

# nat NAT_NS PEER_NS WAN_ADDR LAN_PREFIX TAG
nat() {
    local router=$1 peer=$2 wan=$3 lan=$4 tag=$5
    ip netns add "$router"
    ip netns add "$peer"
    ip link add "w$tag$ID" type veth peer name wan netns "$router"
    ip link set "w$tag$ID" master "$BRIDGE" up
    ip link add lan netns "$router" type veth peer name eth0 netns "$peer"
    ip -n "$router" addr add "$wan/24" dev wan
    ip -n "$router" addr add "$lan.1/24" dev lan
    for link in lo wan lan; do ip -n "$router" link set "$link" up; done
    ip -n "$peer" addr add "$lan.2/24" dev eth0
    ip -n "$peer" link set lo up
    ip -n "$peer" link set eth0 up
    ip -n "$peer" route add default via "$lan.1"
    ip netns exec "$router" sh -c 'echo 1 >/proc/sys/net/ipv4/ip_forward'
    # The relay is known by name, as in a link shared between people.
    mkdir -p "/etc/netns/$peer"
    printf '127.0.0.1 localhost\n%s.1 %s\n' "$NET" "$RELAY_NAME" >"/etc/netns/$peer/hosts"
}
nat "$NAT_A" "$A" $NET.10 192.168.101 a
nat "$NAT_B" "$B" $NET.11 192.168.102 b

# rules ROUTER cone|symmetric: a home router that lets nothing in unasked.
rules() {
    local ipt=(ip netns exec "$1" iptables) random=()
    [[ $2 == symmetric ]] && random=(--random-fully)
    "${ipt[@]}" -F
    "${ipt[@]}" -t nat -F
    "${ipt[@]}" -t nat -A POSTROUTING -o wan -p udp -j MASQUERADE "${random[@]}"
    "${ipt[@]}" -t nat -A POSTROUTING -o wan ! -p udp -j MASQUERADE
    "${ipt[@]}" -A INPUT -i wan -m conntrack --ctstate NEW -j DROP
    "${ipt[@]}" -A FORWARD -i wan -m conntrack --ctstate NEW,INVALID -j DROP
}
both() { rules "$NAT_A" "$1"; rules "$NAT_B" "$1"; }
both cone

"$SWARM_BIN_DIR/swarm-relay" --listen "$NET.1:$RPORT" --no-network \
    --name "$RELAY_NAME:$RPORT" --metrics "127.0.0.1:$MPORT" >"$WORK/relay.log" 2>&1 &
PIDS+=($!)
wait_for "the relay" curl -fsS "http://127.0.0.1:$MPORT/metrics"
relayed_bytes() { curl -fsS "http://127.0.0.1:$MPORT/metrics" | awk '$1 == "swarm_relay_bytes_total" { print $2 }'; }

# on PEER_NS HOME cmd...
on() {
    local ns=$1 home=$2
    shift 2
    ip netns exec "$ns" env SWARM_HOME="$home" SWARM_RELAYS="$RELAY_NAME:$RPORT" \
        SWARM_RELAY_DISCOVERY=off SWARM_RELAY_MODE=always SWARM_EXPERIMENTAL_UDP=1 \
        SWARM_LOG=info "$@"
}

echo "== two machines, each behind its own NAT, cannot reach each other"
reaches() { ip netns exec "$1" timeout 2 bash -c "exec 3<>/dev/tcp/$2/$3" 2>/dev/null; }
reaches "$A" $NET.1 $RPORT || swarm_fail "no route to the relay"
ip netns exec "$B" python3 -c 'import socket,time
s=socket.create_server(("0.0.0.0",42799)); time.sleep(5)' &
sleep 0.5
reaches "$B" 127.0.0.1 42799 || swarm_fail "the probe listener did not start"
if reaches "$A" 192.168.102.2 42799; then
    swarm_fail "the peers can reach each other directly"
fi

SEED=$WORK/seed
HOMES+=("$A $SEED")
mkdir "$WORK/src"
cd "$WORK/src"
# Slow enough that a path can be cut while a transfer is under way.
on "$A" "$SEED" env SWARM_UPLOAD_KBPS=4000 swarm daemon start >/dev/null
on "$A" "$SEED" git init -q -b main
on "$A" "$SEED" swarm init >/dev/null
head -c 2000000 /dev/urandom >blob
git add blob
git commit -q -m one
on "$A" "$SEED" git push -q swarm main
HEAD=$(git rev-parse HEAD)
registered() { on "$A" "$SEED" swarm status relays 2>&1 | grep -q "^[^ ]\+ \(([^)]*) \)\? *registered"; }
wait_for "relay registration" registered
LINK=$(on "$A" "$SEED" swarm link)
REPO=${LINK#swarm://}
REPO=${REPO%%[?&]*}
HINT=$(grep -o 'h=relay:[^&]*' <<<"$LINK" | head -n1)
[[ $HINT == h=relay:$RELAY_NAME:* ]] || swarm_fail "no relay hint by name: $LINK"
cd "$WORK"

# clone NAME: clone from the other NAT; sets LOG and RELAYED (bytes spliced).
clone() {
    local name=$1 before home=$WORK/$1
    HOMES+=("$B $home")
    LOG=$home/daemon.log
    before=$(relayed_bytes)
    on "$B" "$home" git clone -q "swarm://$REPO?$HINT" "$name-out" 2>"$WORK/err" ||
        { cat "$WORK/err" "$LOG"; swarm_fail "clone ($name) failed"; }
    [[ $(git -C "$name-out" rev-parse HEAD) == "$HEAD" ]] || swarm_fail "$name: wrong head"
    on "$B" "$home" swarm daemon stop >/dev/null 2>&1 || true
    sleep 1
    RELAYED=$(($(relayed_bytes) - before))
}

echo "== through ordinary NATs the objects go direct over the punched path"
clone cone
grep -q 'udp path established' "$LOG" || { cat "$LOG"; swarm_fail "no udp path"; }
((RELAYED < 1000000)) || swarm_fail "the objects went through the relay ($RELAYED bytes)"

echo "== through symmetric NATs punching fails and the relay carries them"
both symmetric
clone symmetric
if grep -q 'udp path established' "$LOG"; then swarm_fail "punched through symmetric NATs"; fi
((RELAYED > 2000000)) || swarm_fail "the relay carried only $RELAYED bytes"

echo "== with UDP blocked the relay carries them"
both cone
ip netns exec "$NAT_B" iptables -I FORWARD -p udp -j DROP
clone blocked
if grep -q 'udp path established' "$LOG"; then swarm_fail "punched with UDP blocked"; fi
((RELAYED > 2000000)) || swarm_fail "the relay carried only $RELAYED bytes"

echo "== a punched path that dies mid-transfer falls back to the relay"
both cone
(
    for _ in $(seq 300); do
        grep -q 'udp path established' "$WORK/cut/daemon.log" 2>/dev/null && break
        sleep 0.1
    done
    sleep 1
    ip netns exec "$NAT_B" iptables -I FORWARD -p udp -j DROP
) &
PIDS+=($!)
clone cut
grep -q 'udp path established' "$LOG" || { cat "$LOG"; swarm_fail "no udp path"; }
grep -q 'carrying on over the relay' "$LOG" || { cat "$LOG"; swarm_fail "no fallback"; }
((RELAYED > 2000000)) || swarm_fail "the relay carried only $RELAYED bytes"

echo PASS
