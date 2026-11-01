# Changelog

All notable changes to Swarmgit are recorded here.

## M5 - peer exchange, seeding, multi-peer fetch (2026-11-24)

### Added

- Peer exchange (6.5). Every sync sends `GetPeers` on each session and keeps
  up to 32 addresses per reply in the `peers` table, pinned when the reply
  carries a node ID. A node serves the healthy peers it knows for the repo.
  The handshake's observed address is counted per reporting node, and an IP
  is used as our own only after two distinct nodes report it.
- `Announce` forwarding (7.4). A fresh manifest triggers a sync over the open
  sessions; once its objects are stored, the envelope is forwarded once to
  the other sessions that share the repo. A push now reaches seeders without
  polling.
- Seeding loop: seeded repos sync every 5 minutes, idle sessions are pinged
  after 30 s and closed after 90 s.
- Multi-peer fetch: up to 8 dials in flight and 4 sessions per repo (DM-2,
  DM-3). Wants are split across the sessions that advertise them; a session
  whose pack fails or comes back short is dropped and its wants move to the
  others. Packs already indexed stay, so a later round resumes.
- Best-effort UPnP IGD mapping of the listen port through `igd-next`, with a
  one-hour lease renewed every 30 minutes. `SWARM_NO_UPNP=1` turns it off.
- `swarm gc [--json] <repo>` repacks a store when no sync or upload is
  running (ST-7) and keeps every object reachable from a stored manifest.

### Changed

- A sync stores manifests only after their objects are in the store, so a
  node never advertises heads it cannot serve.

### Tests

```sh
scripts/ci.sh                               # everything below, in Docker
docker compose run --rm dev bash -c 'cargo build --workspace && tests/e2e/m5.sh'
                                            # relay, PEX, Announce, two seeders, gc
```

## M4 - Mainline DHT (2026-11-24)

### Added

- `swarm-disc::dht`: the `Dht` trait over `mainline` 8. `announce_repo`
  announces the topics for yesterday, today and tomorrow (DHT-2), `find`
  looks up today and yesterday (DHT-1), and `put_head`/`get_head` store and
  read the BEP 44 head pointer (DHT-5). Every call has a timeout and failure
  only logs at debug (DHT-4).
- Nodes that answered are kept in the `dht_nodes` table (64 at most, saved
  every 10 minutes and at shutdown) and given to the library at the next
  start (DHT-3).
- The daemon announces seeded repos every 15 minutes and refreshes the head
  pointer after every publish and every hour. A sync starts a DHT lookup in
  parallel with the other routes. When the head pointer is ahead of every
  manifest the peers offer, it keeps looking on the DHT (up to 3 more
  rounds and 4 more sessions) until it reaches that seq.
- `swarm-dht-testnet [N]` starts an N-node local testnet (default 10) and
  prints its bootstrap list, for the e2e scripts.
- `SWARM_DHT_BOOTSTRAP`, `SWARM_DHT_BIND` and `SWARM_DHT_PORT`.
  `SWARM_NO_DHT=1` turns the route off; the tests set it (INV-5).

### Security

- The head pointer salt is `SALT_HEAD || derive_key("swarmgit/v1/head-salt",
  repo_id)`, so no DHT data carries the repo ID (spec 4.2 and DHT-5 amended).

### Tests

```sh
SWARM_NO_DHT=1 SWARM_NO_UPNP=1 cargo test --workspace   # INV-5 (Docker)
docker compose run --rm dev bash -c 'cargo build --workspace && tests/e2e/dht.sh'
                                            # hintless clone, DHT-3, head pointer
docker compose run --rm -e SWARM_LIVE_DHT=1 dev bash -c \
    'cargo test -p swarm-disc live_dht -- --ignored'      # public DHT, not in CI
```

## M3 - LAN discovery (2026-11-24)

### Added

- `swarm-disc::lan`, route B of spec 6.3.
  - `lan::packet` holds the pure parts: the `SWG1` packet encode and strict
    decode (exact `8 + 16n` length, `n` from 1 to 16, at most 1200 bytes),
    tokens for the previous, current and next hour, LAN-1 matching, and the
    LAN-3 limiter of 20 packets per source per second.
  - `lan::service` binds UDP 42099 with `socket2` and address/port reuse
    (LAN-4, `SWARM_LAN_PORT` overrides it). It joins `239.255.42.99` on every
    IPv4 interface and `ff02::4242` on every IPv6 interface with TTL and hop
    limit 1. It announces seeded repos at start, every 60 s and right after
    `init`, `fork` or `seed`; it sends a query burst at 0, 1 and 3 s when a
    sync starts, and answers matching queries with a unicast announce (LAN-2).
- The sync loop adds LAN candidates to hints and cached peers. When nothing
  else connects it waits for LAN answers: up to `find_timeout` (30 s,
  `SWARM_FIND_TIMEOUT` overrides it) for a repo with no state, or 3.5 s
  otherwise before returning stale.
- `swarm status --json` reports `lan.enabled` and `lan.port`.
- `SWARM_NO_LAN=1` turns off only this route.
- Fuzz target `lan_packet`: every accepted packet re-encodes to its input.
- `scripts/ci.sh`, the milestone gate. It re-runs itself in the privileged
  dev container as root, so the namespace test runs, and fails if any e2e
  script prints SKIP.

### Tests

```sh
cargo test -p swarm-disc                    # packet, magic, size, rate limit
scripts/ci.sh                               # fmt, clippy, tests, all e2e (Docker)
docker compose -f compose.yaml -f compose.privileged.yaml run --rm \
    --user root -e SWARM_BIN_DIR=/workspace/target/docker/debug dev \
    tests/e2e/lan.sh                        # needs a container build first
(cd fuzz && cargo +nightly fuzz run lan_packet -- -max_total_time=60)
```

`tests/e2e/lan.sh` joins two network namespaces with a veth pair and gives
them no default or multicast route. A pushes, then B clones and pulls
`swarm://<repo>` with no hint. A tcpdump of UDP 42099 must show announces and
queries and must not contain the repo ID as raw bytes, base32 or hex.

## M2 - encrypted transport and clone from a peer (2026-11-24)

### Added

- `swarm-net` crate: the three-phase handshake and the wire protocol.
  - Phase A is `Noise_XX_25519_ChaChaPoly_BLAKE2s` with the `swarmgit/v1`
    prologue and a random-padded `u16` length prefix (NET-2, NET-3).
  - Phase B carries ML-KEM-768 `PqInit`/`PqResp` (types 14 and 15) inside the
    Phase A transport frames, derives the directional ChaCha20-Poly1305 keys
    with `blake3::derive_key`, and zeroizes the decapsulation key and secrets
    (NET-4 to NET-7).
  - Phase C sends `PqAuth` (type 16) as the first frame under the new keys.
    The hybrid signature covers the Phase A handshake hash and the ML-KEM
    public values, and `Hello` follows only after both signatures verify
    (NET-10, NET-11).
  - `u16` ChaCha20-Poly1305 frames with a `u64` per-direction counter, `u32`
    logical-message lengths, the 8 MiB message cap and the 64-frame limit
    (7.2).
  - Every message of 7.3, a session actor with request/response matching,
    pack streams, request timeouts and protocol limits (7.4).
  - `PeerAddr` wire encoding for `Peers`, and node-ID pins from `@<node>`.
- `swarm-disc` crate: route A, link-hint parsing and resolution (6.2), with
  literal IPv4/IPv6 and DNS hosts.
- Daemon network side: TCP listener, `daemon.json` with `port`, `pid` and
  `listen`, `SWARM_LISTEN`, `SWARM_ADVERTISE` and ephemeral-port fallback
  (NET-1). Server handlers for `GetHeads`, `GetManifest`, `Want`, `Announce`
  and `GetPeers`, including WANT-1 to WANT-5 (tip-only wants, 4096/1024
  limits, streamed chunks, one Want per connection and four concurrent
  uploads).
- Link-hint sync loop (8.2): candidates from hints plus cached peers, TOFU
  node pins in the `peers` table, `Heads` acceptance, missing-object wants,
  pack indexing, fallback to the next session after a bad pack, canonical ref
  updates and stale results when the store already has refs.
- `swarm seed`, `swarm unseed` and `swarm fork`, plus the link-with-hint form
  of `swarm link`. A new clone creates its bare store only after the root
  manifest reveals `object_format`.
- `swarm-testpeer` fixture: probes for `HelloFirst`, `SkipPhaseB`,
  `DoublePqInit` and `HelloBeforeAuth`, and SEC-16 coverage in
  `crates/swarm-daemon/tests/handshake_modes.rs`.
- `crates/swarm-net/tests/handshake.rs`: shared-repo success, wrong pin,
  `no_shared_repo`, and the SEC-18 relay case (pinned rejection plus the
  documented unpinned success).
- `tests/e2e/m2.sh`: clone through a link hint, TOFU pin storage, wrong pin,
  right pin, wrong repo without hanging, B push after copying A's root key,
  A pulling from B, A push and B pull, a pcap assertion that no repo ID or
  base32 form appears in the handshake, a second seeder, and a 50 MiB
  kill-mid-pack failover clone.

### Docker and docs

- `Dockerfile.peers` and `compose.peers.yaml`: two runnable peers on a private
  network, advertising `a` and `b`, with persistent state volumes.
- `scripts/peers-demo.sh`: automated clone plus two-way push/pull tour.
- `docs/USER-GUIDE.md`: the two-peer walkthrough, key copying, seeding,
  configuration, troubleshooting and test commands.
- `Dockerfile.dev` grants `tcpdump` file capabilities so the non-root `dev`
  user can run the packet-capture acceptance checks.

### Fixed

- The frame round-trip unit test no longer deadlocks: it reads the large
  message while the writer task is still blocked on the duplex buffer.

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
