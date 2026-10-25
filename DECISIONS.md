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
