# Changelog

All notable changes to Swarmgit are recorded here.

## M1 - local repo, daemon, CLI, helper (2026-11-24)

### Added

- `swarm-store` crate: `Home` paths from 9.4, sqlite schema version 1,
  `RepoStore` (bare store, manifests, signing key) and a `git` binary wrapper
  for init, refs, update-ref, rev-list, merge-base, fetch, push, pack-objects
  and index-pack. No libgit2.
- Private and atomic file I/O: key files and manifests are written through a
  `0600` temporary file and `rename`, never in place.
- `swarm-daemon` crate: `flock` lockfile, node key, newline-delimited JSON IPC
  server and client, autostart, `hello` version check, `init_repo`, local-only
  `sync`, `publish`, `repo_info`, `list_repos`, `status`, `seed`, `unseed` and
  `stop`. SIGINT/SIGTERM shutdown, idle-exit timer and stale-ref cleanup at
  startup.
- Accept and publish against the store: root manifest `seq` 1 from the user's
  refs, canonical ref updates through `resolve`, no-op detection, and ST-6 ref
  restoration when a publish fails.
- `swarm-cli`: `swarm init`, `link`, `status`, `key backup`, `daemon` and
  `daemon stop`, plus `--json` output for `init`, `link` and `status`. New
  repositories default to sha256 when git supports it and fall back to sha1
  with a warning. A second `swarm init` fails with exit code 1.
- `git-remote-swarm` through argv[0] dispatch: `list`, `fetch` and `push`
  against the local store, `:object-format` negotiation for sha1 and sha256
  clones, HELP-3 no-key errors and all human output on stderr.
- `tests/e2e/lib.sh` and `tests/e2e/m1.sh`: the 11.2 acceptance block plus
  second-init, key-backup, no-key-push, SEC-10 log and interrupted-publish
  checks.
- `Dockerfile.dev` bakes the `stable` toolchain selected by
  `rust-toolchain.toml` into the image, so containers no longer auto-install it
  on first use.

### Security tests

- SEC-8: helper fetches pass `fetch.fsckObjects` and `transfer.fsckObjects`.
- SEC-10: `m1.sh` greps the daemon log for the repo key in hex and base32.
- ST-1/ST-2 store config, ST-3/ST-4 canonical ref updates after a complete
  history check, and ST-5 temporary-ref cleanup.

## M0 - core types and primitives (2026-11-24)

### Added

- `swarm-core` crate with `#![forbid(unsafe_code)]`, no I/O and no network
  dependencies.
- Protocol constants from section 4.2: signature, topic, interest, LAN, key ID,
  session and node-auth contexts; bundle, LAN and key-file magics; limits.
- `ids`: `RepoId`, `KeyId`, `NodeId`, `Oid`, lowercase base32 with strict
  padding-bit rejection, hex helpers, `swarm://` link parse and print, IPv4,
  IPv6 and DNS hints, node pins, eight-hint and 512-byte limits.
- `crypto`: BLAKE3 hash, derive-key, keyed hash, key IDs, repo and node IDs,
  signature input, manifest ID, day and hour numbers, DHT topics, interest and
  LAN tokens, NET-5 session keys and node-auth transcript input.
- `cbor`: hand-written canonical encoder and strict decoder with shortest
  integers, definite lengths, byte-sorted map keys, duplicate-key rejection and
  CE-4 re-encode comparison.
- `manifest`: every field of section 5.1, canonical encode and decode,
  `verify_envelope` with the 5.4 error order, envelope encoding, `accept`,
  `accept_batch`, stale and equivocation handling, and the root/maintainer
  authorization rules.
- `refname` and `glob`: native `git check-ref-format` rules and `*`/`**`
  matching, cross-checked against git over 5,500 generated names.
- `resolve`: pure 5.5 resolution behind `Ancestry`, including tombstones,
  root precedence, fast-forward chains, conflicts, pending refs and `HEAD`.
- `pq`: suite 1 Ed25519 + ML-DSA-65 sign and verify, suite 2 SLH-DSA-SHAKE-128s
  recovery signer, ML-KEM-768 encapsulation and decapsulation, key bundles and
  key IDs.
- `keyfile`: 69-byte repo key and 101-byte node key formats, suite handling,
  zeroizing drops and permission-bit checks.
- `error`: every lowercase snake-case code from 5.7.
- `scripts/gen_vectors.py`: independent Python/BLAKE3 reference with `--check`
  mode for all 4.3 formulas, canonical manifest bytes and IDs.
- Committed vectors in `tests/vectors/`, plus vendored ACVP subsets for
  ML-KEM-768, ML-DSA-65 and SLH-DSA-SHAKE-128s in `tests/vectors/pq/`.
- Property tests for manifest round trips, parser robustness and resolve order
  independence.
- Fuzz targets `link_parse`, `envelope_decode` and `refname_check`.
- `Dockerfile.dev` and compose files for reproducible builds, clippy, rustfmt,
  cargo-fuzz, cargo-deny and Python BLAKE3.

### Security tests

- SEC-1: every byte of a signed envelope is flipped and verification fails.
- SEC-14: either hybrid signature half is required; zeroing or flipping one
  half fails.
- SEC-15: wrong key bundle length, wrong key ID and unknown suite are rejected
  with `bad_keys` and `unknown_suite`.
- SEC-17: session keys match the Python vector and change when any of `hh`,
  `r_i`, `r_r` or `ss` changes.
