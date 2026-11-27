#!/usr/bin/env bash
# Relay design spec milestone R3: three relays that know only the DHT find each
# other, and a client with no `relays` setting and no relay hint reaches a node
# that cannot accept connections. Everything runs on loopback with a local DHT
# testnet.
#
# Making the node unreachable needs root (iptables). In the dev container, as
# scripts/ci.sh does:
#   docker compose -f compose.yaml -f compose.privileged.yaml run --rm \
#       --user root dev tests/e2e/relaynet.sh

set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
export PATH=$PATH:/usr/sbin:/sbin
# This is about reaching a node through relays. A punched UDP path would take
# the transfer off the relay and past the firewall rule below, which only stops
# TCP; punch.sh and m9.sh test that.
export SWARM_NO_UDP=1

[[ -x $SWARM_BIN_DIR/swarm-dht-testnet && -x $SWARM_BIN_DIR/swarm-relay-peer ]] ||
    swarm_fail "missing test binaries; run: cargo build --workspace"

R1=42761 R2=42762 R3=42763 RP=42764
PA=42765 PB=42766 PC=42767 PD=42768 PE=42769 PF=42770
WORK=$(mktemp -d)
A=$WORK/a B=$WORK/b C=$WORK/c D=$WORK/d E=$WORK/e F=$WORK/f
TESTNET=
declare -A RELAY_PID
FIREWALL=0

cleanup() {
    set +e
    for home in "$A" "$B" "$C" "$D" "$E" "$F" "$WORK/g" "$WORK/h"; do
        [[ -f $home/daemon.json ]] && SWARM_HOME=$home swarm daemon stop >/dev/null 2>&1
    done
    kill "${RELAY_PID[@]}" $TESTNET 2>/dev/null
    [[ $FIREWALL == 1 ]] && iptables -D INPUT -p tcp --dport $PB -j REJECT 2>/dev/null
}
trap cleanup EXIT

wait_for() { # wait_for WHAT cmd...
    local what=$1 i
    shift
    for i in $(seq 300); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    swarm_fail "timed out waiting for $what"
}

echo "== start a 10-node DHT testnet"
"$SWARM_BIN_DIR/swarm-dht-testnet" 10 >"$WORK/testnet" &
TESTNET=$!
wait_for "the testnet" test -s "$WORK/testnet"
export SWARM_DHT_BOOTSTRAP=$(head -n1 "$WORK/testnet") SWARM_DHT_BIND=127.0.0.1
export SWARM_RELAY_ALLOW_PRIVATE=1 SWARM_RELAY_ROUND_SECS=2
unset SWARM_NO_DHT

echo "== three relays that know only the DHT find each other"
relay() { # relay PORT [flags...]
    local port=$1
    shift
    env RUST_LOG=debug "$SWARM_BIN_DIR/swarm-relay" --listen "127.0.0.1:$port" \
        --allow-private --round-secs 2 "$@" >"$WORK/relay-$port.log" 2>&1 &
    RELAY_PID[$port]=$!
}
relay $R1 --public
relay $R2
relay $R3
knows() { # knows PORT OTHER...
    local listed other
    listed=$("$SWARM_BIN_DIR/swarm-relay-peer" --key "$WORK/peer.key" relays --relay "127.0.0.1:$1") ||
        return 1
    shift
    for other in "$@"; do grep -qx "127.0.0.1:$other" <<<"$listed" || return 1; done
}
wait_for "relay $R1 to know the others" knows $R1 $R2 $R3
wait_for "relay $R2 to know the others" knows $R2 $R1 $R3
wait_for "relay $R3 to know the others" knows $R3 $R1 $R2

# as HOME PORT cmd...
as() { local home=$1 port=$2; shift 2; env SWARM_HOME="$home" SWARM_LISTEN="127.0.0.1:$port" SWARM_LOG=debug "$@"; }
stop() {
    SWARM_HOME=$1 swarm daemon stop >/dev/null 2>&1 || true
    for _ in $(seq 100); do
        [[ -f $1/daemon.json ]] || return 0
        sleep 0.1
    done
}

echo "== a seeder with no relay setting registers with discovered relays"
mkdir "$WORK/src"
cd "$WORK/src"
as "$B" $PB git init -q -b main
as "$B" $PB swarm init >/dev/null
echo one >one
git add .
git commit -q -m one
as "$B" $PB git push -q swarm main
registrations() { [[ $(as "$B" $PB swarm status relays 2>&1 | grep -c "^[^ ]\+ \+registered") -ge 2 ]]; }
wait_for "two relay registrations" registrations
LINK=$(as "$B" $PB swarm link)
REPO=${LINK#swarm://}
REPO=${REPO%%[?&]*}
[[ $LINK == *h=relay:127.0.0.1:427* ]] || swarm_fail "the link carries no relay hint: $LINK"
HINT=$(grep -o 'h=tcp:[^&]*' <<<"$LINK" | head -n1)
NODE=${HINT##*@}
# Only a pinned direct hint: nothing about relays.
DIRECT="swarm://$REPO?h=tcp:127.0.0.1:$PB@$NODE"
cd "$WORK"

echo "== A clones directly and remembers the seeder"
as "$A" $PA git clone -q "$DIRECT" a-clone

if [[ $(id -u) -ne 0 ]] || ! command -v iptables >/dev/null; then
    echo "SKIP: making the seeder unreachable needs root"
    echo PASS
    exit 0
fi

echo "== the seeder becomes unreachable"
iptables -I INPUT -p tcp --dport $PB -j REJECT
FIREWALL=1
(cd src && echo two >two && git add . && git commit -q -m two && as "$B" $PB git push -q swarm main)
HEAD=$(git -C src rev-parse HEAD)

echo "== A pulls through a relay found by node ID alone"
stop "$A"
(cd a-clone && as "$A" $PA git pull -q 2>"$WORK/err") || { cat "$WORK/err" "$A/daemon.log"; swarm_fail "pull failed"; }
[[ $(git -C a-clone rev-parse HEAD) == "$HEAD" ]] || swarm_fail "A did not get the new commit"
grep -q 'connected via relay' "$A/daemon.log" || swarm_fail "A did not use a relay"
[[ -f $A/relays.db && $(stat -c %a "$A/relays.db") == 600 ]] || swarm_fail "relays.db was not saved privately"

echo "== with discovery off there is no way in"
# A seeds too, and the DHT would lead to it; only the seeder may remain.
stop "$A"
if (as "$D" $PD env SWARM_RELAY_DISCOVERY=off SWARM_FIND_TIMEOUT=2 git clone -q "$DIRECT" d-clone) 2>"$WORK/err"; then
    swarm_fail "a clone without discovery reached the firewalled seeder"
fi

echo "== one home relay dies; a fresh client still gets in"
stop "$D"
FIRST=$(as "$B" $PB swarm status relays 2>&1 | sed -n 's/^127\.0\.0\.1:\([0-9]*\) \+registered.*$/\1/p' | head -n1)
kill "${RELAY_PID[$FIRST]}"
(as "$C" $PC git clone -q "$DIRECT" c-clone 2>"$WORK/err") ||
    { cat "$WORK/err" "$C/daemon.log"; swarm_fail "clone after losing a relay failed"; }
[[ $(git -C c-clone rev-parse HEAD) == "$HEAD" ]] || swarm_fail "c-clone has the wrong head"
grep -q 'connected via relay' "$C/daemon.log" || swarm_fail "C did not use a relay"

echo "== a client with only the repo ID finds the seeder by its topic"
stop "$C"
PG=42771 G=$WORK/g
# No DHT peer records for the repo are of use: the seeder's port is closed.
(as "$G" $PG env SWARM_FIND_TIMEOUT=10 git clone -q "swarm://$REPO" g-clone 2>"$WORK/err") ||
    { cat "$WORK/err" "$G/daemon.log"; swarm_fail "clone by repo ID alone failed"; }
[[ $(git -C g-clone rev-parse HEAD) == "$HEAD" ]] || swarm_fail "g-clone has the wrong head"
grep -q 'connected via relay' "$G/daemon.log" || swarm_fail "G did not use a relay"
stop "$G"

echo "== a private relay with a password and a pinned key"
stop "$C"
stop "$B"
echo 'correct horse' >"$WORK/password"
relay $RP --password-file "$WORK/password" --key-file "$WORK/relay.key" --name 127.0.0.1:$RP
pinned() { grep -o "relay address: 127.0.0.1:$RP#[a-z0-9]*" "$WORK/relay-$RP.log" | awk '{print $3}'; }
wait_for "the private relay" pinned
PINNED=$(pinned)
[[ $(stat -c %a "$WORK/relay.key") == 600 ]] || swarm_fail "the relay key is readable by others"
private() { # private HOME PORT cmd...
    local home=$1 port=$2
    shift 2
    as "$home" "$port" env SWARM_RELAYS="$PINNED" SWARM_RELAY_PASSWORD='correct horse' \
        SWARM_RELAY_DISCOVERY=off SWARM_RELAY_MODE=always SWARM_NO_DHT=1 "$@"
}
private "$B" $PB swarm status >/dev/null 2>&1
registered() { private "$B" $PB swarm status relays 2>&1 | grep -q "^[^ ]*$RP[^ ]* \+registered"; }
wait_for "the seeder to register with the private relay" registered
LINK=$(private "$B" $PB swarm link)
[[ $LINK == *h=relay:127.0.0.1:$RP@* ]] || swarm_fail "the link does not name the private relay: $LINK"
[[ $LINK != *"#"* ]] || swarm_fail "the link carries the pinned key: $LINK"
private "$E" $PE git clone -q "$LINK" e-clone 2>"$WORK/err" ||
    { cat "$WORK/err" "$E/daemon.log"; swarm_fail "clone through the private relay failed"; }
[[ $(git -C e-clone rev-parse HEAD) == "$HEAD" ]] || swarm_fail "e-clone has the wrong head"
if private "$F" $PF env SWARM_RELAY_PASSWORD=wrong SWARM_FIND_TIMEOUT=2 git clone -q "$LINK" f-clone 2>"$WORK/err"; then
    swarm_fail "a wrong password got through the private relay"
fi

echo "== a relay that dies mid-transfer is named in the error"
RM=42772
restart_private() { # restart_private [flags...]
    kill "${RELAY_PID[$RP]}" 2>/dev/null || true
    wait "${RELAY_PID[$RP]}" 2>/dev/null || true
    relay $RP --password-file "$WORK/password" --key-file "$WORK/relay.key" --name 127.0.0.1:$RP "$@"
    wait_for "the private relay" pinned
    wait_for "the seeder to register again" registered
}
restart_private --splice-kib-per-sec 32
(cd src && head -c 2000000 /dev/urandom >big && git add big && git commit -q -m big &&
    private "$B" $PB git push -q swarm main)
(cd e-clone && private "$E" $PE git pull 2>"$WORK/err") &
PULL=$!
wait_for "the download to start" grep -q 'downloading objects' "$WORK/err"
kill "${RELAY_PID[$RP]}"
if wait $PULL; then swarm_fail "a pull survived its relay dying"; fi
grep -q "relay 127.0.0.1:$RP" "$WORK/err" || { cat "$WORK/err"; swarm_fail "the error does not name the relay"; }

echo "== a reachable seeder is used directly and the relay carries nothing"
iptables -D INPUT -p tcp --dport $PB -j REJECT
FIREWALL=0
restart_private --metrics 127.0.0.1:$RM
BOTH="swarm://$REPO?h=tcp:127.0.0.1:$PB@$NODE&h=relay:127.0.0.1:$RP@$NODE"
private "$WORK/h" 42773 env SWARM_RELAY_MODE=auto git clone -q "$BOTH" h-clone 2>"$WORK/err" ||
    { cat "$WORK/err"; swarm_fail "direct clone failed"; }
METRICS=$(curl -fsS "http://127.0.0.1:$RM/metrics")
grep -Eq '^swarm_relay_bytes_total 0$' <<<"$METRICS" || swarm_fail "bytes went through the relay: $METRICS"

echo PASS
