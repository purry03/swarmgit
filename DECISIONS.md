# Decisions

This file records choices that the spec leaves open or that an implementer must
make while the spec is silent. Each entry says what was chosen and why.

## M0 (core types and primitives)

### PQ crates: RustCrypto instead of libcrux

The spec prefers `libcrux-ml-kem` and `libcrux-ml-dsa`, with RustCrypto as the
alternative. At M0 the RustCrypto family was available at coherent released
versions for all three required algorithms:

- `ml-dsa 0.1.2` (FIPS 204 ML-DSA-65)
- `ml-kem 0.3.4` (FIPS 203 ML-KEM-768)
- `slh-dsa 0.2.0-rc.5` (FIPS 205 SLH-DSA-SHAKE-128s)

Using one family for all three avoids mixing key encodings and RNG traits. The
libcrux family did not offer a matching released SLH-DSA implementation at the
same time. This is the alternative the spec explicitly allows.

### Expanded keys in known-answer tests

ACVP supplies ML-DSA-65 secret keys in the 4032-byte expanded form and ML-KEM
decapsulation keys in the 2400-byte expanded form. Production key files store
the 32-byte ML-DSA seed and the 64-byte ML-KEM seed. The KAT tests therefore use
`ExpandedSigningKey::from_expanded` and `DecapsulationKey::from_expanded`
directly; `swarm_core::pq::MlKem768Kem::decaps` accepts both the 64-byte seed
and the 2400-byte expanded form so the same code path can be exercised.

### `object_format` on a maintainer's first manifest

Section 5.1 says key 9 is "root, required when `seq` is 1". A maintainer's own
sequence also starts at 1, but key 9 is root-only (5.4 step 3 rejects it on a
non-root signer). Requiring it unconditionally would make a maintainer's first
manifest impossible to publish.

Decision: `verify_envelope` requires `object_format` only when `seq == 1` and
`signer == repo_id`, which is exactly the root's first manifest. A non-root
manifest may omit it and its object IDs are checked against the stored format
by `accept`.

### Object-ID lengths without `object_format`

If a manifest has no `object_format` (only possible for a non-root manifest or
a later root manifest), every object ID must be 20 or 32 bytes and all IDs in
the same manifest must use one length. This keeps decoding strict without
inventing a default before `accept` has learned the repo format.

### Ancestry trait signature

Section 11.1 sketches
`fn is_ancestor(&self, a: Oid, b: Oid) -> Option<bool>`. `Oid` owns a `Vec<u8>`
because object formats are 20 or 32 bytes. Passing by value would move the
values out of the caller for every query, so the trait uses `&Oid`. The
semantics, including `None` for a missing object, are unchanged.

### Session-key derivation

`session_keys` implements
`blake3_xof(CTX_SESSION, hh || r_i || r_r || ss)[0..64]` with BLAKE3's
derive-key mode. The first 32 bytes are initiator-to-responder and the last 32
are responder-to-initiator, as NET-5 requires. The Python reference uses
`blake3.blake3(material, derive_key_context="swarmgit/v1/session").digest(64)`.

### Canonical CBOR decoding

Encoding is written by hand. Decoding uses `ciborium`, converts to the small
`swarm_core::cbor::Value`, re-encodes with the hand-written encoder and compares
bytes (CE-4). This rejects indefinite lengths, non-shortest integers, tags,
floats, unsorted maps, duplicate keys and trailing bytes in one place.

### Manifest module split

Section 14 caps source files at 600 lines. `manifest.rs` holds the types,
canonical encoding, envelope verification and signing, while `accept.rs` holds
`StoredManifest`, `AcceptState` and the acceptance rules of 5.4. Security and
acceptance tests live in `tests/security.rs` so they exercise the public API
exactly as a downstream crate would. Both files stay well under the cap.

### `Kem::keygen` is fallible

Section 4.7 sketches `fn keygen(&self) -> (EncapKey, DecapKey)`. Key
generation reads the OS RNG, which can fail. Returning `Result` avoids a panic
or a silent weak key and matches the rest of the crate's error handling. The
`encaps` and `decaps` signatures match the sketch.

### Cross-implementation PQ test

The M0 cross-implementation test is conditional on both crate families being
available. Only the RustCrypto family is used here, so there is no second
implementation to test against; the official ACVP vectors cover both signing
and verification for the chosen family. If libcrux is added later, the test
belongs next to the ACVP tests.

### `Envelope` type

`swarm_core::manifest::Envelope` wraps the two byte strings of CE-5 and has
canonical `encode`/`decode`. `encode_envelope` and `verify_envelope` are thin
wrappers so callers can use either the type or the free functions.

### Test and dev profile optimization

SLH-DSA-SHAKE-128s signing is slow enough that a debug `cargo test` takes many
minutes per case. Generic hash loops are monomorphized into the caller's crate,
so optimizing only the dependency is insufficient. `[profile.test]`
`opt-level = 3` keeps the acceptance command `cargo test -p swarm-core`
practical.

The same effect makes an unoptimized `cargo run` unusable for the suite 2
recovery path (SLH-DSA key generation went from 54 s to about 1 s, and signing
from 396 s to about 8 s). `[profile.dev] opt-level = 2` fixes this while keeping
debug assertions. The three PQ dependencies are pinned to `opt-level = 3` in
`[profile.dev.package.*]`.

Measured on the dev container, release profile:

| Operation | Time |
| --- | --- |
| ML-KEM-768 key generation | ~0.2 ms |
| Suite 1 key generation (Ed25519 + ML-DSA-65) | ~1.3 ms |
| Suite 2 key generation (SLH-DSA-SHAKE-128s) | ~0.43 s |
| Suite 1 signing | ~0.7 ms |
| Suite 2 signing (SLH-DSA-SHAKE-128s) | ~3.2 s |

SLH-DSA is deliberately slow to sign and fast to verify; it is only used for
recovery keys, so steady-state publishing uses suite 1.

### Fuzz crate layout

`fuzz/` is a separate workspace so `cargo test` never tries to build libFuzzer
targets. It depends on `swarm-core` by path and contains the three M0 targets
from 11.1: `link_parse`, `envelope_decode` and `refname_check`.

### Key-file permissions

`swarm-core::keyfile::check_mode` is a pure permission-bit check. Opening files
with `O_EXCL`, `0600` and atomic rename is I/O and belongs to `swarm-store`
(M1). Core returns `LoosePermissions` so the CLI can print the `chmod 600`
command without duplicating the rule.

### Link scheme handling

`onion` is parsed and printed at M0 even though the SOCKS5 transport is M9.
Unknown schemes are ignored rather than failing the link, as 4.4 requires.
Malformed pins are ignored and leave the hint unpinned.

### Glob semantics

`*` never crosses `/`, `**` does, and the pattern must match the whole ref
name. There is no escape character in v1; a literal `*` cannot be expressed.
This follows 5.3 literally and is recorded because a future escape syntax
would change the manifest language.

## M1 (local repo, daemon, CLI, helper)

### Git through the binary, not libgit2

Section 8.1 says the store is an ordinary bare repository and that no git
protocol is reimplemented. `swarm-store::git` therefore shells out to `git` for
every operation. It is async because the daemon calls it from tokio, and it
sets `--git-dir=<store>` or `-C <worktree>` explicitly instead of relying on
the process environment. The helper has separate `*_inherited` functions that
let git's own `GIT_DIR` select the destination repository.

### Store paths and the short socket

`$SWARM_HOME/repos/<repo-id>/store.git` and
`$SWARM_HOME/repos/<repo-id>/manifests/<signer-hex>.cbor` follow 9.4 exactly.
When `$SWARM_HOME/ipc.sock` would exceed 100 bytes, the socket moves to
`$XDG_RUNTIME_DIR/swarmgit-<first 8 hex of blake3(SWARM_HOME)>.sock` (or the
system temp directory when `XDG_RUNTIME_DIR` is unset). Hashing the home path
rather than truncating it keeps distinct homes apart.

### Autostart uses the `swarm` binary, not `current_exe`

The helper is invoked through a `git-remote-swarm` symlink and dispatches on
argv[0]. Spawning `current_exe()` from inside the helper would run the helper
again with `daemon --detach` as its URL. The client therefore looks for
`$SWARM_BIN`, then a `swarm` sibling of the current executable, then `swarm` on
`PATH`. This keeps argv[0] dispatch honest and works from both the CLI and the
helper.

### One manifest file per signer, atomically replaced

The store keeps only the latest accepted envelope per signer, as 9.4 specifies.
Each write goes to a hidden `0600` temporary file in the same directory and is
renamed into place. The daemon removes leftover `.tmp` files at startup. A kill
during `publish` can therefore leave the old file or the new file, never a
partially written one. The manifest file mode is private because a manifest
contains the signer's public bundle, not because it is secret.

### `sync` is local-only at M1 and returns `ref_list` as an extension

The 9.3 table defines `sync` as returning `state`, `refs` (a count) and `head`.
The remote helper also needs the ref-to-object map. `sync` keeps `refs` as the
count and adds `ref_list`; the helper prefers `ref_list` and accepts an object
under `refs` for forward compatibility. Nothing in M1 contacts a peer.

### Publish no-op detection and ST-6 restoration

`publish` compares the store's current refs and root fields with the last
accepted root manifest. If nothing changed it returns the stored `seq` with
`noop: true`, which is what makes a second `git push` leave `seq` at 2. On any
failure after the user's push has moved refs, including a missing key, a bad
key, too many refs, a signing error, an acceptance error or a manifest write
error, it restores the refs from the last stored manifest. This implements the
second sentence of ST-6.

### Object format for new repos and clones

A new `swarm init` asks git for sha256 and falls back to sha1 with a warning on
older git. An existing repository keeps its format. The remote helper
advertises the `object-format` capability and emits `:object-format sha1|sha256`
in its `list` reply, so `git clone` of a sha256 repo creates a sha256
repository. This was verified against git 2.39.5 and git 2.53.

### No-op push versus no signing key

The helper asks `repo_info` for `has_key` before touching the store. Without a
key it emits `error <dst> no signing key (use: git swarm fork)` for every ref
and exits 0, with no refs moved. Checking first is what keeps HELP-3 from
leaving unpublished refs in the store. The daemon still returns
`no_signing_key` from `publish` for callers that skip the helper.

### Key backup

`swarm key backup <repo> -o FILE` copies the 69-byte key file with the daemon
out of the loop: it reads through the same permission check as the daemon and
writes atomically with mode `0600`. The user owns storing the result; the key
is never printed and never sent over IPC.

### Idle exit during tests

DMN-6 exits after 30 idle minutes when no repo is seeded. M1 seeds the repo
created by `init`, so the timer rarely fires, and the e2e test uses a normal
short run. `SWARM_IDLE_EXIT_MINUTES` overrides the default for tests and
manual experiments without adding `config.toml` parsing early.

## M2 decisions

### Phase B and C live inside the Noise frames

`PqInit`, `PqResp` and `PqAuth` are ordinary logical messages protected by the
Phase A transport, not a second transport. This keeps one length-prefix format
for the whole handshake, makes a dump of the socket show no recognizable
ML-KEM structure, and lets the session actor switch to ChaCha20-Poly1305 keys
only after `PqAuth` has been checked. `PqAuth` is deliberately the first frame
under the new keys: no application message can precede authentication.

### One pin source, deduped by address

Hints, the `peers` table and `Peers` replies all produce the same `Candidate`.
`dedupe` merges candidates with the same socket address and prefers a cached
node ID over an unpinned hint. That is the trust-on-first-use rule from NET-11
without a second code path. A failed pin is recorded as a peer failure and the
address is not retried for the same sync.

### `swarm link` prints exactly one hint

The M1 acceptance block captures `swarm link` into a shell variable and also
uses it after `?`. Printing one `h=tcp:host:port@node` hint keeps that block
running under `git clone` without shell quoting tricks. Extra cached peers are
still used by `sync`; they are not printed.

### A failed pack moves to the next session

`sync_repo` marks a session failed on a bad pack, a timeout or a connection
error, then loops while missing objects remain and picks another session. This
is what makes the kill-mid-pack test pass when a second seeder holds the data.
If every session has failed it returns `pack_invalid`; a single peer error is
returned unchanged.

### Publish is local; pulls choose the peer explicitly

`git push` copies objects and refs into the **local** daemon store and then
publishes there. The remote URL is used for the pre-push sync, not as the
publication target. A pull therefore has to point at whichever peer holds the
new manifest. The guide shows both an explicit `git fetch <peer-link>` and a
second remote, which is honest about the model and works without waiting for
the five-minute seeded sync.

### `git pull` needs `--no-rebase` on git 2.39

The helper advertises the `HEAD` symref so `git clone` can pick the default
branch. On git 2.39 that makes a plain `git pull` see divergent branches. The
documented form is `git pull --no-rebase`, or `git fetch` followed by
`git merge --ff-only FETCH_HEAD`; both are exercised by the scripts.

### Two peers run from a dedicated image

`Dockerfile.peers` builds the binary in a builder stage and copies it to a slim
runtime with git. The compose services share a private network, set
`SWARM_ADVERTISE` to their network alias, and keep `/state` in named volumes.
This is the same binary and the same hint format as a real deployment; only
the DNS name and the volume differ.

### Packet capture in an unprivileged dev container

The development image runs as uid 1000, so `cap_add` alone does not give
`tcpdump` an effective `CAP_NET_RAW`. `Dockerfile.dev` installs
`libcap2-bin` and sets file capabilities on `/usr/bin/tcpdump`, which keeps the
test run unprivileged while still allowing the interest-token capture.
