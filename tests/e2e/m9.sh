#!/usr/bin/env bash
# M9 acceptance from spec 11.10: SOCKS5 with nothing leaving around the proxy,
# onion hints and hidden-service hosting, shaped handshakes, the ban list and
# limits, and UDP hole punching. tests/e2e/faketor.py stands in
# for Tor; nothing touches the real network.
#
# The proxy-leak and ban tests need root (network namespaces, iptables). In the
# dev container, as scripts/ci.sh does:
#   docker compose -f compose.yaml -f compose.privileged.yaml run --rm \
#       --user root dev tests/e2e/m9.sh

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
export PATH=$PATH:/usr/sbin:/sbin

SOCKS=42741 CONTROL=42742 PRELAY=42743 PWEB=42744 ORELAY=42751
PA=42745 PB=42746 PC=42747 PD=42748 PE=42749 PX=42750
WORK=$(mktemp -d)
A=$WORK/a
TORLOG=$WORK/tor.log
NS=swg-m9-$$
VETH_H=swgh$$ VETH_N=swgn$$
HOST_IP=10.232.0.1 NS_IP=10.232.0.2
BAN_IP=198.51.100.7
ROOT_TESTS=1
PIDS=()
HOMES=()

cleanup() {
    set +e
    for home in "${HOMES[@]}"; do
        [[ -f $home/daemon.json ]] && kill "$(jq -r .pid "$home/daemon.json")" 2>/dev/null
    done
    kill "${PIDS[@]}" 2>/dev/null
    if [[ $ROOT_TESTS == 1 ]]; then
        ip netns pids "$NS" 2>/dev/null | xargs -r kill 2>/dev/null
        ip netns del "$NS" 2>/dev/null
        ip link del "$VETH_H" 2>/dev/null
        ip addr del "$BAN_IP/32" dev lo 2>/dev/null
    fi
}
trap cleanup EXIT

if [[ $(id -u) -ne 0 ]] || ! command -v iptables >/dev/null || ! command -v ip >/dev/null; then
    ROOT_TESTS=0
fi

repo_of() { local l=${1#swarm://}; printf '%s' "${l%%[?&]*}"; }
# as HOME PORT cmd...
as() { local home=$1 port=$2; shift 2; env SWARM_HOME="$home" SWARM_LISTEN="127.0.0.1:$port" "$@"; }
stop() { env SWARM_HOME="$1" swarm daemon stop >/dev/null 2>&1 || true; sleep 0.5; }
hint_of() { grep -o "h=$2:[^&]*" <<<"$1" | head -n1; }
wait_for() { # wait_for WHAT cmd...
    local what=$1 i
    shift
    for i in $(seq 100); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    swarm_fail "timed out waiting for $what"
}
listening() { (exec 3<>"/dev/tcp/$1/$2") 2>/dev/null; }

# Clone with a fresh home and check the head. clone HOME PORT HINTS DIR [env...]
clone() {
    local home=$1 port=$2 hints=$3 dir=$4
    shift 4
    HOMES+=("$home")
    (cd "$WORK" && as "$home" "$port" env "$@" git clone -q "swarm://$REPO?$hints" "$dir" 2>"$WORK/err") ||
        { cat "$WORK/err" "$home/daemon.log"; swarm_fail "clone into $dir failed"; }
    [[ $(git -C "$WORK/$dir" rev-parse HEAD) == "$HEAD" ]] || swarm_fail "$dir has the wrong head"
}

HOST_BIND=127.0.0.1
if [[ $ROOT_TESTS == 1 ]]; then
    ip netns add "$NS"
    ip link add "$VETH_H" type veth peer name "$VETH_N"
    ip link set "$VETH_N" netns "$NS"
    ip addr add "$HOST_IP/24" dev "$VETH_H"
    ip link set "$VETH_H" up
    ip -n "$NS" addr add "$NS_IP/24" dev "$VETH_N"
    ip -n "$NS" link set lo up
    ip -n "$NS" link set "$VETH_N" up
    # A route to everywhere, so a stray direct connection reaches the firewall
    # instead of failing early.
    ip -n "$NS" route add default via "$HOST_IP"
    ip netns exec "$NS" sh -c 'echo 1 >/proc/sys/net/ipv6/conf/all/disable_ipv6' || true
    HOST_BIND=$HOST_IP
fi

# A relay published as an onion service, the way torrc would do it.
RELAY_ONION=$(printf 'r%.0s' $(seq 56)).onion

echo "== a stand-in Tor, a relay and a seeder with a hidden service"
python3 "$ROOT/tests/e2e/faketor.py" --socks "$HOST_BIND:$SOCKS" --control "127.0.0.1:$CONTROL" \
    --map seed.test=127.0.0.1 --map mirror.test=127.0.0.1 --map relay.test=127.0.0.1 \
    --onion "$RELAY_ONION:$ORELAY=127.0.0.1:$ORELAY" \
    --log "$TORLOG" &
PIDS+=($!)
"$SWARM_BIN_DIR/swarm-relay" --listen "127.0.0.1:$PRELAY" \
    --name "127.0.0.1:$PRELAY" --name "relay.test:$PRELAY" >"$WORK/relay.log" 2>&1 &
PIDS+=($!)
wait_for "the fake Tor" grep -q READY "$TORLOG"
wait_for "the relay" listening 127.0.0.1 "$PRELAY"

HOMES+=("$A")
seeder() {
    as "$A" $PA env SWARM_ONION_CONTROL=127.0.0.1:$CONTROL SWARM_ONION_MAX_CONNECTIONS=1 \
        SWARM_RELAYS="127.0.0.1:$PRELAY" SWARM_RELAY_MODE=always \
        SWARM_LOG=debug "$@"
}
mkdir "$WORK/src"
cd "$WORK/src"
seeder git init -q -b main
seeder swarm init >/dev/null
head -c 300000 /dev/urandom >blob
echo one >one
git add .
git commit -q -m one
seeder git push -q swarm main
HEAD=$(git rev-parse HEAD)
registered() { seeder swarm status relays 2>&1 | grep -q "^[^ ]\+ \+registered"; }
wait_for "relay registration" registered
LINK=$(seeder swarm link)
REPO=$(repo_of "$LINK")
ONION=$(hint_of "$LINK" onion)
RELAY=$(hint_of "$LINK" relay)
NODE=${ONION##*@}
[[ -n $ONION && -n $RELAY ]] || swarm_fail "link lacks an onion or relay hint: $LINK"
[[ -f $A/keys/onion.key && $(stat -c %a "$A/keys/onion.key") == 600 ]] ||
    swarm_fail "the onion key was not saved privately"
cd "$WORK"

echo "== the onion address survives a restart"
stop "$A"
seeder swarm daemon start
wait_for "relay registration" registered
[[ $(hint_of "$(seeder swarm link "$REPO")" onion) == "$ONION" ]] || swarm_fail "the onion address changed"
grep -q 'ADD_ONION .* saved' "$TORLOG" || swarm_fail "the saved key was not reused"

echo "== without a proxy an onion hint is not dialed"
B0=$WORK/b0
HOMES+=("$B0")
if as "$B0" $PB env SWARM_FIND_TIMEOUT=3 git clone -q "swarm://$REPO?$ONION" onion-direct 2>"$WORK/err"; then
    swarm_fail "cloned an onion hint without a proxy"
fi
stop "$B0"

echo "== onion visitors have their own configurable cap"
ONION_HOST=${ONION#h=onion:}
ONION_HOST=${ONION_HOST%%:*}
python3 - "$HOST_BIND" $SOCKS "$ONION_HOST" $PA <<'PY'
import socket, struct, sys

def via_proxy(proxy, port, name, target):
    s = socket.create_connection((proxy, port), timeout=5)
    s.sendall(b"\x05\x01\x00")
    assert s.recv(2) == b"\x05\x00"
    s.sendall(b"\x05\x01\x00\x03" + bytes([len(name)]) + name.encode() + struct.pack(">H", target))
    assert s.recv(10)[1] == 0, "the proxy could not reach the service"
    return s

def closed_at_once(s):
    s.settimeout(2)
    try:
        return s.recv(1) == b""
    except socket.timeout:
        return False

proxy, socks, name, port = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
first = via_proxy(proxy, socks, name, port)
assert not closed_at_once(first), "the first onion visitor was turned away"
assert closed_at_once(via_proxy(proxy, socks, name, port)), "the cap of 1 was not applied"
# Local peers are counted apart.
assert not closed_at_once(socket.create_connection(("127.0.0.1", port))), "a local peer was turned away"
PY

if [[ $ROOT_TESTS == 1 ]]; then
    echo "== behind a proxy, a firewall sees nothing but the proxy port"
    (mkdir -p "$WORK/www" && as "$A" $PA swarm mirror export "$REPO" "$WORK/www" >/dev/null)
    (cd "$WORK/www" && exec python3 -m http.server $PWEB --bind 127.0.0.1 >/dev/null 2>&1) &
    PIDS+=($!)
    wait_for "the mirror" listening 127.0.0.1 $PWEB
    fw() { ip netns exec "$NS" iptables "$@"; }
    fw -A OUTPUT -o lo -p tcp -j ACCEPT
    fw -A OUTPUT -p tcp -d "$HOST_IP" --dport $SOCKS -j ACCEPT
    fw -A OUTPUT -j DROP
    dropped() { fw -L OUTPUT -v -n -x | awk '$3 == "DROP" { print $1 }'; }
    # The rule really counts: a direct attempt is caught.
    ip netns exec "$NS" timeout 2 bash -c "exec 3<>/dev/tcp/$HOST_IP/$((SOCKS + 100))" 2>/dev/null || true
    [[ $(dropped) -gt 0 ]] || swarm_fail "the firewall rule catches nothing"
    fw -Z

    # Discovery is deliberately left enabled: the proxy alone must stop it.
    proxied() { # proxied HOME cmd...
        local home=$1
        shift
        HOMES+=("$home")
        ip netns exec "$NS" env -u SWARM_NO_DHT -u SWARM_NO_LAN -u SWARM_NO_UPNP \
            SWARM_HOME="$home" SWARM_PROXY="socks5://$HOST_IP:$SOCKS" \
            SWARM_RELAYS="relay.test:$PRELAY" SWARM_RELAY_MODE=always SWARM_LOG=debug "$@"
    }
    via() { # via NAME HINTS
        local home=$WORK/p-$1
        proxied "$home" git clone -q "swarm://$REPO?$2" "$WORK/p-out-$1" 2>"$WORK/err" ||
            { cat "$WORK/err" "$home/daemon.log"; swarm_fail "proxied clone ($1) failed"; }
        [[ $(git -C "$WORK/p-out-$1" rev-parse HEAD) == "$HEAD" ]] || swarm_fail "$1: wrong head"
        # The daemon registers with its relay through the proxy as well.
        registered_proxied() { proxied "$home" swarm status relays 2>&1 | grep -q "^[^ ]\+ \+registered"; }
        wait_for "proxied relay registration" registered_proxied
        if proxied "$home" swarm link "$REPO" | grep -q 'h=tcp:'; then
            swarm_fail "a proxied link gives away a local address"
        fi
        proxied "$home" swarm daemon stop >/dev/null 2>&1 || true
        sleep 0.5
    }
    via onion "$ONION"
    via name "h=tcp:seed.test:$PA@$NODE"
    via relay "h=relay:relay.test:$PRELAY@$NODE"
    # With a relay configured the seeder would be found by the repo's topic,
    # so the mirror is tried without one.
    M=$WORK/p-mirror
    proxied "$M" env SWARM_RELAYS= git clone -q "swarm://$REPO?h=http:mirror.test:$PWEB" \
        "$WORK/p-out-mirror" 2>"$WORK/err" ||
        { cat "$WORK/err" "$M/daemon.log"; swarm_fail "proxied clone (mirror) failed"; }
    [[ $(git -C "$WORK/p-out-mirror" rev-parse HEAD) == "$HEAD" ]] || swarm_fail "mirror: wrong head"
    proxied "$M" env SWARM_RELAYS= swarm daemon stop >/dev/null 2>&1 || true
    for wanted in "$ONION_HOST:$PA" "seed.test:$PA" "relay.test:$PRELAY" "mirror.test:$PWEB"; do
        grep -q "CONNECT $wanted" "$TORLOG" || swarm_fail "the proxy never saw $wanted"
    done
    [[ $(dropped) == 0 ]] || { fw -L OUTPUT -v -n -x; swarm_fail "something tried to go around the proxy"; }

    echo "== a private relay run as an onion service, both machines behind the proxy"
    echo 'correct horse' >"$WORK/password"
    "$SWARM_BIN_DIR/swarm-relay" --listen "127.0.0.1:$ORELAY" --name "$RELAY_ONION:$ORELAY" \
        --no-network --key-file "$WORK/onion-relay.key" --password-file "$WORK/password" \
        >"$WORK/onion-relay.log" 2>&1 &
    PIDS+=($!)
    wait_for "the onion relay" grep -q 'relay address:' "$WORK/onion-relay.log"
    PINNED=$(awk '/relay address:/ { print $NF; exit }' "$WORK/onion-relay.log")
    # UDP punching is on by default and must stay off behind a proxy.
    torified() { # torified HOME cmd...
        local home=$1
        shift
        proxied "$home" env SWARM_RELAYS="$PINNED" SWARM_RELAY_PASSWORD='correct horse' \
            "$@"
    }
    OS=$WORK/onion-seed
    mkdir "$WORK/onion-src"
    (
        cd "$WORK/onion-src"
        torified "$OS" git init -q -b main
        torified "$OS" swarm init >/dev/null
        echo hidden >file
        git add file
        git commit -q -m hidden
        torified "$OS" git push -q swarm main
    )
    onion_registered() { torified "$OS" swarm status relays 2>&1 | grep -q "^[^ ]*\.onion[^ ]* \+registered"; }
    wait_for "registration with the onion relay" onion_registered
    OLINK=$(cd "$WORK/onion-src" && torified "$OS" swarm link)
    [[ $OLINK == *h=relay:$RELAY_ONION:$ORELAY@* && $OLINK != *'#'* && $OLINK != *h=tcp:* ]] ||
        swarm_fail "unexpected link from behind the proxy: $OLINK"
    torified "$WORK/onion-clone" git clone -q "$OLINK" "$WORK/onion-out" 2>"$WORK/err" ||
        { cat "$WORK/err" "$WORK/onion-clone/daemon.log"; swarm_fail "clone over the onion relay failed"; }
    [[ $(git -C "$WORK/onion-out" rev-parse HEAD) == $(git -C "$WORK/onion-src" rev-parse HEAD) ]] ||
        swarm_fail "onion-out has the wrong head"
    grep -q "CONNECT $RELAY_ONION:$ORELAY" "$TORLOG" || swarm_fail "the proxy never saw the onion relay"
    if grep -q 'udp path established' "$WORK/onion-clone/daemon.log"; then
        swarm_fail "punched from behind a proxy"
    fi
    torified "$OS" swarm daemon stop >/dev/null 2>&1 || true
    torified "$WORK/onion-clone" swarm daemon stop >/dev/null 2>&1 || true
    [[ $(dropped) == 0 ]] || { fw -L OUTPUT -v -n -x; swarm_fail "something tried to go around the proxy"; }
else
    echo "SKIP: the proxy-leak test needs root, ip and iptables"
    via_home=$WORK/p
    clone "$via_home" $PB "h=tcp:seed.test:$PA@$NODE" p-out SWARM_PROXY="socks5://127.0.0.1:$SOCKS"
    stop "$via_home"
fi

echo "== a proxy or limit that cannot be used stops the daemon"
for bad in SWARM_PROXY=http://127.0.0.1:1 SWARM_ONION_MAX_CONNECTIONS=0 SWARM_UPLOAD_KBPS=fast; do
    status=0
    as "$WORK/bad" $PX env "$bad" timeout 10 swarm daemon --foreground >"$WORK/err" 2>&1 || status=$?
    # 124 is the timeout: the daemon came up and kept running.
    [[ $status -ne 0 && $status -ne 124 ]] || swarm_fail "the daemon accepted $bad (status $status)"
done

echo "== shaped and unshaped nodes interoperate"
clone "$WORK/o1" $PB "h=tcp:127.0.0.1:$PA@$NODE" obfs-out SWARM_OBFS=on
clone "$WORK/o2" $PC "h=tcp:127.0.0.1:$PB" plain-out
stop "$WORK/o2"
stop "$WORK/o1"

echo "== two relayed peers punch a UDP path by default"
U=$WORK/u
HOMES+=("$U")
as "$U" $PD env SWARM_LOG=info swarm daemon start
clone "$U" $PD "$RELAY" udp-out
grep -q 'udp path established' "$U/daemon.log" || { cat "$U/daemon.log"; swarm_fail "no udp path"; }
stop "$U"

echo "== with udp off the relayed session is used as it is"
V=$WORK/v
clone "$V" $PE "$RELAY" relay-out SWARM_LOG=info SWARM_NO_UDP=1
if grep -q 'udp path established' "$V/daemon.log"; then swarm_fail "punched with udp off"; fi
stop "$V"

if [[ $ROOT_TESTS == 1 ]]; then
    echo "== three protocol violations ban an address, across restarts"
    ip addr add "$BAN_IP/32" dev lo
    X=$WORK/x
    HOMES+=("$X")
    banner() { env SWARM_HOME="$X" SWARM_LISTEN="$BAN_IP:$PX" SWARM_LOG=debug swarm daemon start; }
    probe() { # probe MODE: "junk" breaks the protocol, "open" expects a wait, "shut" a refusal
        python3 - "$BAN_IP" $PX "$1" <<'PY'
import socket, sys
host, port, mode = sys.argv[1], int(sys.argv[2]), sys.argv[3]
s = socket.create_connection((host, port), timeout=5, source_address=(host, 0))
s.settimeout(2)
if mode == "junk":
    s.sendall(b"\x00\x05hello")
try:
    closed = s.recv(1) == b""
except socket.timeout:
    closed = False
except ConnectionError:
    closed = True
sys.exit(0 if closed == (mode != "open") else 1)
PY
    }
    banner
    probe open || swarm_fail "a fresh address was refused"
    for i in 1 2 3; do probe junk || swarm_fail "junk $i was not rejected"; done
    wait_for "the ban" grep -q 'banned for 10 minutes' "$X/daemon.log"
    probe shut || swarm_fail "a banned address was served"
    stop "$X"
    banner
    probe shut || swarm_fail "the ban did not survive a restart"
    stop "$X"
else
    echo "SKIP: the ban test needs root to add a non-loopback address"
fi

echo PASS
