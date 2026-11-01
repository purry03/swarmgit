# Shared setup for Swarmgit end-to-end tests.
#
# Source this from a test script. It puts the freshly built binaries on PATH,
# makes `git-remote-swarm` available under its argv[0] name, and isolates git
# from the developer's global configuration.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SWARM_BIN_DIR=${SWARM_BIN_DIR:-$ROOT/target/debug}

if [[ ! -x "$SWARM_BIN_DIR/swarm" ]]; then
    echo "missing $SWARM_BIN_DIR/swarm; run: cargo build --workspace" >&2
    exit 1
fi

# The helper is dispatched by argv[0], so give it its protocol name.
HELPER_DIR=$(mktemp -d)
ln -sf "$SWARM_BIN_DIR/swarm" "$HELPER_DIR/git-remote-swarm"
export PATH="$HELPER_DIR:$SWARM_BIN_DIR:$PATH"

export GIT_CONFIG_GLOBAL=${GIT_CONFIG_GLOBAL:-/dev/null}
export GIT_CONFIG_NOSYSTEM=${GIT_CONFIG_NOSYSTEM:-1}
export GIT_AUTHOR_NAME=${GIT_AUTHOR_NAME:-swarm test}
export GIT_AUTHOR_EMAIL=${GIT_AUTHOR_EMAIL:-swarm@example.invalid}
export GIT_COMMITTER_NAME=${GIT_COMMITTER_NAME:-swarm test}
export GIT_COMMITTER_EMAIL=${GIT_COMMITTER_EMAIL:-swarm@example.invalid}

# M1 has no network transport; keep future discovery code out of the way.
export SWARM_NO_DHT=1
export SWARM_NO_LAN=1
export SWARM_NO_UPNP=1

swarm_fail() {
    echo "FAIL: $*" >&2
    exit 1
}

swarm_assert() {
    if ! "$@"; then
        swarm_fail "assertion failed: $*"
    fi
}
