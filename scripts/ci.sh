#!/usr/bin/env bash
# Milestone gate: format, lint, unit and integration tests, then every e2e
# script. Runs inside the privileged dev container as root, so the namespace
# test (tests/e2e/lan.sh) really runs; a SKIP from any e2e script fails CI.
#
#   scripts/ci.sh            # from the host: re-runs itself in Docker
#   SWARM_CI_INSIDE=1 ...    # already inside a suitable container

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ ${SWARM_CI_INSIDE:-} != 1 ]]; then
    exec docker compose -f compose.yaml -f compose.privileged.yaml run --rm -T \
        --user root \
        -e SWARM_CI_INSIDE=1 \
        -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
        dev scripts/ci.sh "$@"
fi

# Root inside the container: keep build output apart from the host's target/
# and hand it back to the host user afterwards.
export CARGO_TARGET_DIR=/workspace/target/ci
export SWARM_BIN_DIR=$CARGO_TARGET_DIR/debug
git config --global --add safe.directory /workspace
trap 'chown -R "${HOST_UID:-1000}:${HOST_GID:-1000}" "$CARGO_TARGET_DIR" 2>/dev/null || true' EXIT

step() { printf '\n== %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all -- --check

step "cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test"
# INV-5: every earlier test passes with the DHT off; the DHT unit tests run
# their own local testnet regardless. No test maps ports on a real gateway.
SWARM_NO_DHT=1 SWARM_NO_UPNP=1 cargo test --workspace -q

step "cargo build"
cargo build --workspace -q

for script in tests/e2e/m1.sh tests/e2e/m2.sh tests/e2e/lan.sh \
    tests/e2e/dht.sh tests/e2e/m5.sh tests/e2e/m6.sh tests/e2e/m7.sh tests/e2e/m8.sh \
    tests/e2e/relaynet.sh tests/e2e/m9.sh tests/e2e/punch.sh tests/e2e/about.sh; do
    step "$script"
    out=$(mktemp)
    bash "$script" 2>&1 | tee "$out"
    if grep -q '^SKIP' "$out"; then
        echo "FAIL: $script skipped; CI must run it" >&2
        exit 1
    fi
done

printf '\nCI PASS\n'
