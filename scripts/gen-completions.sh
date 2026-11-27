#!/usr/bin/env bash
# Regenerate completions/ and man/ from the CLI definition (run inside the dev
# container).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q -p swarm-cli
mkdir -p completions
bin=${CARGO_TARGET_DIR:-target}/debug/swarm
$bin completions bash >completions/swarm.bash
$bin completions zsh >completions/_swarm
$bin completions fish >completions/swarm.fish
SWARM_WRITE_MAN=1 cargo test -q -p swarm-cli shipped_man_pages >/dev/null
