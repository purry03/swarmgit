#!/usr/bin/env bash
# Regenerate completions/ from the CLI definition (run inside the dev container).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q -p swarm-cli
mkdir -p completions
bin=${CARGO_TARGET_DIR:-target}/debug/swarm
$bin completions bash >completions/swarm.bash
$bin completions zsh >completions/_swarm
$bin completions fish >completions/swarm.fish
