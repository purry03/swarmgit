#!/usr/bin/env bash
# Fuzz every target in turn (spec 12.2 and 11.10).
#
#   scripts/fuzz.sh                      # 600 s per target, in the dev container
#   FUZZ_SECONDS=60 scripts/fuzz.sh      # the per-commit run
#   scripts/fuzz.sh message_decode       # only the named targets
#
# Needs a nightly toolchain and cargo-fuzz, which the dev container has.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ ${SWARM_FUZZ_INSIDE:-} != 1 && ! -f /.dockerenv && -z ${CI:-} ]]; then
    exec docker compose run --rm -T -e SWARM_FUZZ_INSIDE=1 \
        -e FUZZ_SECONDS="${FUZZ_SECONDS:-600}" dev scripts/fuzz.sh "$@"
fi

seconds=${FUZZ_SECONDS:-600}
targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
    mapfile -t targets < <(cargo +nightly fuzz list)
fi
for target in "${targets[@]}"; do
    printf '\n== fuzz %s for %s s\n' "$target" "$seconds"
    cargo +nightly fuzz run "$target" -- -max_total_time="$seconds" -timeout=10
done
printf '\nFUZZ PASS: %s\n' "${targets[*]}"
