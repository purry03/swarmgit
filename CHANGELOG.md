# Changelog

All notable changes to Swarmgit are recorded here.

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
