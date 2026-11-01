# Swarmgit: Agent-Buildable Spec for a Zero-Setup P2P Git

Nov 24, 2026 · @Rohan Sharma

## 1. Overview

Swarmgit lets anyone publish and clone a git repository from a single link, with no server, account or setup step. This spec is written so an LLM coding agent can build the whole system in ten milestones (M0 to M9), each with acceptance tests the agent can run itself.

```bash
git swarm init              # makes a keypair, prints swarm://<repo-id>
git push swarm main         # signs the new ref state, starts seeding
git clone swarm://<repo-id> # finds peers, fetches, verifies, done
```

### 1.1 Goals

- G1. A repo is a key. Creating one needs no registration, no server and no config file.
- G2. Stock git works unchanged. Integration is a git remote helper named `git-remote-swarm`.
- G3. Any peer can serve any object and the receiver verifies it. Trust comes from hashes and signatures, never from the peer.
- G4. Peers are found by several independent routes and any one route is enough. Blocking one route does not stop the system.
- G5. First use needs nothing the user did not already have: a link, the same LAN, or the public BitTorrent DHT.
- G6. Small codebase: target under 8,000 lines of Rust, one binary.
- G7. Quantum resistant by default. Traffic uses a hybrid classical plus post-quantum key exchange, and repo identity and manifests use hybrid signatures. A break of either half alone must not break security, and there is no classical-only mode.

### 1.2 Non-goals (v1)

- No issues, pull requests, wiki, CI or web UI. Later these can be stored as git objects under `refs/meta/*`.
- No global repo search or naming. Links are the only directory.
- No anonymity guarantee. Tor support is optional (M9) and is the user's choice.
- No access control and no private repos. Anyone with the link can read.
- No incentive or payment layer.

### 1.3 What no bootstrapping means here

A new node must learn about at least one peer somehow, so strictly zero is impossible. The design makes that first contact free of any operator we run and of any step the user performs.

| Route | What the user does | Who operates it |
| --- | --- | --- |
| Link hints inside the link | Nothing: the sender already included them | The person who shared the link |
| LAN multicast | Nothing | Nobody |
| Mainline DHT (BitTorrent) | Nothing | Millions of unrelated nodes; default entry nodes ship inside the DHT library |
| Peer exchange | Nothing | Peers already connected |
| File or USB bundle | Copy a file | Nobody |

The project runs no bootstrap server, relay or registry. The DHT's default entry nodes are third party: if they are blocked, the other routes still work.

### 1.4 How an agent should use this spec

1. Read sections 2 to 5 fully before writing code. They define the invariants and byte formats.
2. Build in milestone order (section 11). Do not start milestone N+1 until every acceptance test of milestone N passes.
3. Requirement IDs use RFC 2119 words. MUST and MUST NOT are mandatory, SHOULD needs a written reason to skip, MAY is optional.
4. When the spec is silent, take the value from the Defaults table (section 12). If none applies, pick the simplest option, record it in `DECISIONS.md` with one line of reasoning, and continue. Ask the user only if an acceptance test cannot be written.
5. MUST NOT invent cryptography. Use only the primitives in section 4, through the named crates.
6. Shell out to the `git` binary for pack creation, pack indexing and ref checks. Do not reimplement git internals.
7. Every milestone ends with tests green, `cargo clippy -- -D warnings` clean, and a short entry appended to `CHANGELOG.md`.
8. Keep each source file under 600 lines. Split modules instead of growing files.

## 2. Conventions, invariants and glossary

These rules hold across every module. If code contradicts one, the code is wrong.

### 2.1 Invariants

| ID | Invariant |
| --- | --- |
| INV-1 | Nothing from the network is trusted until verified. Objects are verified by git hash, manifests by signature, peers by nothing. |
| INV-2 | A manifest is accepted only if its signature verifies, its signer is authorized, and its `seq` is strictly greater than the stored `seq` for that signer and repo. |
| INV-3 | The daemon never writes to a user's working repo. Only the remote helper does, and only through `git` commands. |
| INV-4 | Every network read has a size limit and a timeout. Limits are in the Defaults table (section 12). |
| INV-5 | Any one discovery route working is enough to clone. No code path may require a specific route. |
| INV-6 | No clock is trusted. Timestamps are informational and never decide acceptance. |
| INV-7 | Serialization is canonical. Two implementations given the same logical manifest MUST produce identical bytes. |
| INV-8 | Secret keys never leave `$SWARM_HOME/keys` and are never logged. |
| INV-9 | The daemon serves only objects reachable from tips of manifests it has verified. |
| INV-10 | Hybrid rule. A signature is valid only if both its classical and its post-quantum half verify. Every session key mixes a classical and a post-quantum secret. No code path may accept one half alone or fall back to classical-only. |
| INV-11 | No algorithm negotiation. The suite is fixed by the role of the key and by the protocol version. A peer never chooses or downgrades it. |

### 2.2 Code conventions

- Language: Rust, edition 2021, stable toolchain, `#![forbid(unsafe_code)]` in every crate.
- Errors: `thiserror` for library errors, `anyhow` only in `main`. No `unwrap()` or `expect()` outside tests and startup.
- Async: `tokio` multi-thread runtime. All network I/O is async. Calls to the `git` binary use `tokio::process::Command`.
- Logging: `tracing`. Default level `info`; `SWARM_LOG=debug` raises it. Never log key material or full file paths from remote peers.
- Wire integers are big-endian. Hashes and keys are raw bytes on the wire and lowercase hex in logs and file names.
- Every public function that parses untrusted input has a fuzz target (section 12).
- Environment overrides for tests: `SWARM_HOME` (data dir), `SWARM_NO_DHT=1`, `SWARM_NO_LAN=1`, `SWARM_LISTEN` (for example `127.0.0.1:0`).

### 2.3 Glossary

| Term | Meaning |
| --- | --- |
| Repo ID | The 32-byte key ID (a BLAKE3 hash) of the repo's root key bundle. It is the repo's permanent name. |
| Link | `swarm://<repo-id>` with optional hints, see 4.4. |
| Root signer | The holder of the root secret key. It can publish any ref and manage maintainers. |
| Maintainer | A key the root signer authorized for some ref patterns. Optional, from M7. |
| Manifest | A signed snapshot of one signer's refs for one repo. The only mutable state in the system. |
| Envelope | The CBOR bytes of a manifest plus its signature. |
| Store | The bare git repository the daemon keeps per swarm repo. It is the source for serving and the target for fetching. |
| Node key | A random per-install hybrid key (suite 1 plus an X25519 key) that identifies a peer. Its key ID is the node ID. It has no link to any repo. |
| Interest token | A 16-byte keyed hash that lets two peers discover a shared repo without revealing its ID to anyone who does not already know it. |
| Seed | To keep a store and serve it to peers. Every clone seeds by default. |
| Hint | A peer address embedded in a link. |
| Topic | The 20-byte DHT info-hash derived from a repo ID and a day number. |
| Key bundle | The public keys of one signer, concatenated in a fixed order for its suite (4.7). Carried inside every manifest it signs. |
| Key ID | The 32-byte BLAKE3 hash of a key bundle. Names a signer. The root key's ID is the repo ID. |
| Suite | A fixed set of signature algorithms. Suite 1 is Ed25519 plus ML-DSA-65. Suite 2 is SLH-DSA-SHAKE-128s, for recovery keys. |

## 3. Architecture

One Rust workspace builds one binary, `swarm`, that acts as CLI, git remote helper and background daemon depending on how it is invoked. The daemon owns all networking and storage. The CLI and the helper are thin clients that talk to it over a local socket and auto-start it when it is not running.

&#91;embedded content: architecture · 3 clients, 1 daemon, 3 stores\]

Git and the CLI send JSON over a local socket. The daemon finds peers, speaks the encrypted protocol, verifies every manifest, and only then moves refs in the git store.

### 3.1 Process model

| Entry point | Invoked as | Job |
| --- | --- | --- |
| CLI | `swarm <cmd>` or `git swarm <cmd>` (hard link named `git-swarm`) | Init, link printing, seed, status, fork, bundles |
| Remote helper | `git-remote-swarm` (hard link), started by git for any `swarm://` URL | Implements git's remote-helper protocol: `list`, `fetch`, `push` |
| Daemon | `swarm daemon` (auto-spawned, detached) | Listens for peers, runs discovery, syncs stores, signs and publishes manifests |

The binary picks its role from `argv[0]` first, then from the first argument.

### 3.2 Workspace layout

```text
swarmgit/
  Cargo.toml                workspace
  DECISIONS.md CHANGELOG.md
  crates/
    swarm-core/             ids, keys, canonical CBOR, manifest, resolve rules. No I/O.
    swarm-store/            data dir paths, sqlite, git wrapper
    swarm-net/              Noise transport, wire messages, peer sessions
    swarm-disc/             discovery: hints, LAN, DHT, PEX, bundles, mirrors
    swarm-daemon/           orchestration, IPC server, seeding loop
    swarm-cli/              the binary: CLI, remote helper, daemon entry
  tests/
    vectors/                golden bytes (generated in M0, then frozen)
    e2e/                    multi-process integration tests
  fuzz/                     cargo-fuzz targets
```

Dependency rule: `core` imports nothing from the other crates and does no I/O. `net` imports `core`. `disc` imports `core` and `net`. `daemon` imports all of them. `cli` imports `core` and the IPC client only. A violation fails review.

### 3.3 Dependencies

Verify each crate's current version and API when you add it. If an API differs from what is written here, adapt behind the trait in 3.4 and note it in `DECISIONS.md`.

| Purpose | Crate | Note |
| --- | --- | --- |
| Signatures | `ed25519-dalek` 2.x | Use `verify_strict`. |
| Hashing, KDF, MAC | `blake3` | `derive_key`, keyed hash, plain hash. |
| Noise | `snow` | Pattern in 7.1. |
| CBOR decode | `ciborium` | Encode with the hand-written canonical encoder in 5.2, not the crate's. |
| Base32 | `data-encoding` | Lowercase, no padding. |
| Mainline DHT | `mainline` | Peer lookup, announce, mutable items. Hide behind the `Dht` trait. |
| Multicast | `socket2` + `tokio` | LAN discovery. |
| SOCKS5 | `tokio-socks` | Tor and other proxies, M9. |
| Storage | `rusqlite` with `bundled` | Peers and repo metadata. |
| CLI | `clap` 4 | Derive API. |
| Runtime, errors, logs | `tokio`, `thiserror`, `anyhow`, `tracing` |  |
| Tests | `proptest`, `tempfile`, `assert_cmd`, `cargo-fuzz` |  |
| Post-quantum signatures | libcrux-ml-dsa, or RustCrypto ml-dsa | ML-DSA-65, FIPS 204. See 4.7 for crate choice. |
| Hash-based signatures | RustCrypto slh-dsa | SLH-DSA-SHAKE-128s, FIPS 205. Recovery key only, M8. |
| Post-quantum KEM | libcrux-ml-kem, or RustCrypto ml-kem | ML-KEM-768, FIPS 203. Session key exchange, 7.1. |
| Session AEAD | chacha20poly1305 | Frames after the handshake, 7.2. snow is used for the classical handshake only. |

External binary required at runtime: `git` 2.30 or newer on `PATH`.

### 3.4 Core traits

All network edges sit behind traits so tests can replace them with in-memory fakes.

```rust
pub trait Discovery: Send + Sync {
    /// Stream peers that may hold `repo`. Never ends early; the caller drops it.
    fn find(&self, repo: RepoId) -> BoxStream<'static, PeerAddr>;
    /// Tell the network we hold `repo` and listen on `port`. Idempotent.
    async fn announce(&self, repo: RepoId, port: u16) -> Result<()>;
}

pub trait Transport: Send + Sync {
    async fn dial(&self, addr: &PeerAddr) -> Result<Box<dyn Conn>>;
    async fn listen(&self, bind: SocketAddr) -> Result<Box<dyn Listener>>;
}

pub trait Conn: AsyncRead + AsyncWrite + Send + Unpin {
    fn remote(&self) -> &PeerAddr;
}

pub trait Dht: Send + Sync {
    async fn get_peers(&self, topic: [u8; 20]) -> Result<Vec<SocketAddr>>;
    async fn announce_peer(&self, topic: [u8; 20], port: u16) -> Result<()>;
    async fn get_mutable(&self, key: [u8; 32], salt: &[u8]) -> Result<Option<MutableItem>>;
    async fn put_mutable(&self, item: MutableItem) -> Result<()>;
}
```

`PeerAddr` is an enum: `Tcp(SocketAddr)`, `Onion { host: String, port: u16 }`. Add variants only through `DECISIONS.md`.

### 3.5 End-to-end flows

**Clone** (`git clone swarm://<id>`):

1. Git starts `git-remote-swarm`, which asks the daemon `sync <id>` over IPC. The daemon is spawned first if the socket is dead.
2. The daemon creates an empty store, then runs every discovery route in parallel: link hints, LAN, DHT, PEX.
3. For each peer found, it dials, handshakes and asks for heads. It collects manifests and keeps the highest valid `seq` per signer.
4. It resolves canonical refs (5.4), requests packs for missing tips and indexes them into the store.
5. The helper answers git's `list` from the store's refs and answers `fetch` by running `git fetch` from the store path into the user's repo.
6. The daemon registers the repo for seeding and announces itself on the DHT and LAN.

**Push** (`git push swarm main`):

1. The helper receives `push` commands and runs `git push` from the user's repo into the store.
2. It tells the daemon `publish <id>`. The daemon reads the store's refs, builds manifest `seq+1`, signs it with the repo key, stores it and announces it to connected peers.
3. If no secret key exists for this repo, the daemon returns `no_signing_key` and the helper prints how to fork (`git swarm fork`).

## 4. Cryptography and identifiers

Every key, hash, token and link in the system is derived with the fixed primitives and exact formulas below. Implement them once in `swarm-core::ids` and `swarm-core::crypto`, and call them from everywhere else.

### 4.1 Primitives

| Use | Primitive | Rules |
| --- | --- | --- |
| Signatures | Hybrid: Ed25519 (RFC 8032) plus ML-DSA-65 (FIPS 204) | Verify with `verify_strict`. Reject weak public keys. Both signatures must verify (4.7). |
| Hash, KDF, MAC | BLAKE3, 256-bit output | `blake3::hash`, `blake3::derive_key`, `blake3::keyed_hash`. |
| Transport encryption | `Noise_XX_25519_ChaChaPoly_BLAKE2s` | Classical Noise handshake, then an ML-KEM-768 exchange inside it, 7.1. |
| Randomness | OS RNG (`rand::rngs::OsRng`) | No seeded RNG outside tests. |
| Git object IDs | SHA-1 or SHA-256, as the repo's object format | Used only to name git objects. Manifests sign the object IDs. New repos default to sha256 (4.8). |
| DHT info-hash | First 20 bytes of a BLAKE3 output | The DHT needs 20 bytes. |
| Key encapsulation | ML-KEM-768, FIPS 203, hybrid with X25519 | Mixed into the session key with fresh random values, 7.1. Never used alone. |
| Session encryption | ChaCha20-Poly1305, 256-bit keys | Counter nonces, one key per direction, 7.2. |
| Recovery signatures | SLH-DSA-SHAKE-128s, FIPS 205 | Only for the recovery key, M8. Hash-based, so it does not rest on lattice assumptions. |

MUST NOT add any other algorithm without a `DECISIONS.md` entry that names the threat it addresses.

### 4.2 Domain-separation strings

These are constants in `swarm-core::consts`. Never build them dynamically.

| Constant | Value |
| --- | --- |
| `CTX_SIG_MANIFEST` | the bytes `swarmgit/v1/manifest` followed by one zero byte |
| `CTX_TOPIC` | `swarmgit/v1/topic` |
| `CTX_INTEREST` | `swarmgit/v1/interest` |
| `CTX_LAN` | `swarmgit/v1/lan` |
| `SALT_HEAD` | `swarmgit/v1/head` |
| `MAGIC_BUNDLE` | `SWGB1` followed by a newline |
| `MAGIC_LAN` | `SWG1` |
| CTX\_KEYID | swarmgit/v1/keyid |
| CTX\_SESSION | swarmgit/v1/session |
| CTX\_NODEAUTH | swarmgit/v1/nodeauth |
| `CTX_HEAD_SALT` | `swarmgit/v1/head-salt` |

### 4.3 Derived values

```text
key_id(bundle)     = blake3_derive_key(CTX_KEYID, bundle)      # 32 bytes; bundle layout per suite, 4.7
repo_id            = key_id(root key bundle)                   # fixed when the repo is created
node_id            = key_id(node key bundle)                   # the bundle inside node.key, 4.5
sig_input(m)       = CTX_SIG_MANIFEST || m                     # m = manifest bytes
signature(suite 1) = ed25519_sig(64) || mldsa65_sig(3309)      # both over sig_input(m); 3373 bytes
envelope           = cbor_array([ m, signature ])
manifest_id(env)   = blake3(env)                               # 32 bytes

day(t)             = floor(unix_seconds(t) / 86400)            # UTC
topic(repo, d)     = blake3_derive_key(CTX_TOPIC, repo || u64_be(d))[0..20]

interest_key(repo) = blake3_derive_key(CTX_INTEREST, repo)
interest(repo, hh) = blake3_keyed_hash(interest_key(repo), hh)[0..16]
                     # hh = the 32-byte Noise handshake hash of this connection (7.1)

hour(t)            = floor(unix_seconds(t) / 3600)
lan_key(repo)      = blake3_derive_key(CTX_LAN, repo)
lan_token(repo, h) = blake3_keyed_hash(lan_key(repo), u64_be(h))[0..16]

session_okm        = blake3_xof(CTX_SESSION, hh || r_i || r_r || ss)[0..64]   # 7.1
auth_input(role)   = CTX_NODEAUTH || hh || ek || ct || r_i || r_r || role     # role: 1 initiator, 2 responder
```

Why the day number: peers announce under yesterday, today and tomorrow's topic, and lookups try today and yesterday. A passive observer then cannot build a permanent list of which info-hashes map to swarmgit repos without already knowing the repo ID. This hides repos from enumeration, not from someone who holds the link.

### 4.4 Link format

```text
link      = "swarm://" repo-id [ "?" hint *( "&" hint ) ]
repo-id   = 52 characters of [a-z2-7]        ; RFC 4648 base32, lowercase, no padding
hint      = "h=" scheme ":" host ":" port [ "@" node-id ]
node-id   = 52 characters of [a-z2-7]        ; key ID of the peer's node key, used as a pin
scheme    = "tcp" / "onion"
host      = IPv4 / "[" IPv6 "]" / onion-name / dns-name
```

Parsing rules, all MUST:

- Lowercase the input before decoding. Reject any other scheme and any character outside the alphabet.
- The 52 characters decode to 32 bytes. Reject if the last character carries non-zero padding bits.
- Treat the 32 bytes as a key ID, which is a hash. Do not parse them as a public key. They are checked against the key bundle inside the first manifest that arrives (5.4).
- A hint may end with `@` and a 52-character node ID. That is a pin (7.1, NET-11). Ignore a malformed pin and treat the hint as unpinned.
- Accept at most 8 hints and at most 512 bytes in total. Ignore unknown hint schemes instead of failing.
- Print links with the repo ID first and hints after. `git swarm link` adds up to 3 hints, only those it has confirmed reachable.

### 4.5 Key files

| File | Content | Rules |
| --- | --- | --- |
| `$SWARM_HOME/node.key` | 101 bytes: SWK1, suite byte 1, Ed25519 seed (32), ML-DSA-65 seed (32), X25519 secret (32) | Created on first daemon start. Mode 0600. Identifies this install on the wire only. Its key ID is the node ID. |
| `$SWARM_HOME/keys/<repo-id>.key` | 69 bytes: the magic SWK1, one suite byte (1), the 32-byte Ed25519 seed, then the 32-byte ML-DSA-65 seed (the FIPS 204 seed called xi) | Created by `swarm init` and `swarm fork`. Mode 0600. Its presence is what makes this install the root signer. |

On Unix, refuse to load a key file that is readable by group or others, and print the `chmod 600` command to fix it. Write key files with `O_CREAT | O_EXCL` and a temp-file rename.

### 4.6 Test vectors

Generate vectors in M0 with an independent implementation, then freeze them in `tests/vectors/`. Do not derive the expected values from the Rust code under test. A minimal reference in Python:

```python
import blake3, struct
def derive(ctx, material):  # ctx is a str
    return blake3.blake3(material, derive_key_context=ctx).digest()
def keyed(key32, data):
    return blake3.blake3(data, key=key32).digest()
repo = bytes([1]) * 32      # use a real public key in the committed vectors
day = 20416
topic = derive('swarmgit/v1/topic', repo + struct.pack('>Q', day))[:20]
```

Every formula in 4.3 gets at least one vector. Vector files are plain hex, one value per line, with a comment line naming the formula.

### 4.7 Post-quantum suites

Every signature and every session key is hybrid: a classical algorithm and a post-quantum one run together and an attacker must break both. This covers two threats. An attacker who records traffic today and decrypts it later once a quantum computer exists, and forgery of long-lived repo identities after that day.

| Suite | Algorithms | Used for | Key bundle | Signature |
| --- | --- | --- | --- | --- |
| 1 | Ed25519 (RFC 8032) plus ML-DSA-65 (FIPS 204) | Root keys and maintainer keys | 1984 bytes: Ed25519 key (32) then ML-DSA-65 key (1952) | 3373 bytes: Ed25519 (64) then ML-DSA-65 (3309) |
| 2 | SLH-DSA-SHAKE-128s (FIPS 205) | Recovery key only, M8 | 32 bytes | 7856 bytes |

Session key exchange uses ML-KEM-768 (FIPS 203): encapsulation key 1184 bytes, ciphertext 1088 bytes, shared secret 32 bytes. Details are in 7.1.

- SIG-1. Suite 1 requires both halves to verify over the same `sig_input` (4.3). A missing, short or extra byte is `bad_signature`. One valid half is never enough.
- SIG-2. Ed25519 uses `verify_strict`. ML-DSA-65 uses the empty context string and the crate's plain verify function.
- SIG-3. Signing uses ML-DSA's default hedged mode with OS randomness. Verification does not depend on that choice.
- SIG-4. Key generation: Ed25519 from a 32-byte seed, ML-DSA-65 from a 32-byte seed (FIPS 204 `xi`). Both seeds come from the OS RNG and are stored as in 4.5.
- SIG-5. A key ID is `key_id(bundle)` from 4.3. Compare IDs in constant time.
- SIG-6. The suite comes from the manifest and is checked against the key's role: suite 1 for root and maintainers, suite 2 only for recovery manifests. Peers never choose or negotiate it (INV-11). A new suite needs a new `version` number.
- SIG-7. The recovery key is hash-based so that it still stands if the lattice assumptions behind ML-DSA and ML-KEM ever fail. It signs rarely, so its large signature costs nothing.

Why hybrid and not post-quantum alone: ML-KEM and ML-DSA were standardized in 2024. A flaw found in them must not break us, and a quantum computer must not break the classical half.

```rust
pub trait Signer: Send + Sync {
    fn suite(&self) -> u8;
    fn bundle(&self) -> Vec<u8>;              // public key bundle, 4.7 table
    fn sign(&self, msg: &[u8]) -> Vec<u8>;    // full signature, both halves
}
/// Checks key_id, suite, lengths, then BOTH halves. Any failure is an error.
pub fn verify(suite: u8, bundle: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), CryptoError>;

pub trait Kem: Send + Sync {
    fn keygen(&self) -> (EncapKey /* 1184 B */, DecapKey);
    fn encaps(&self, ek: &EncapKey) -> Result<(Ciphertext /* 1088 B */, [u8; 32])>;
    fn decaps(&self, dk: &DecapKey, ct: &Ciphertext) -> Result<[u8; 32]>;
}
```

Crate choice: prefer `libcrux-ml-kem` and `libcrux-ml-dsa` (formally verified implementations), with RustCrypto `ml-kem`, `ml-dsa` and `slh-dsa` as the alternative. Check current versions when adding them. If one family is only pre-release for an algorithm, use the other and record it in `DECISIONS.md`. Use liboqs bindings only if both Rust options fail, because a C dependency complicates builds.

Tests for this subsection:

- Official known-answer tests for ML-KEM-768, ML-DSA-65 and SLH-DSA-SHAKE-128s, stored in `tests/vectors/pq/`.
- Cross-implementation: sign with one crate, verify with the other, both directions, over 1,000 random messages, when both families are available.
- Hybrid tamper tests (SEC-14).

### 4.8 Hashes, symmetric keys and git object IDs

Symmetric primitives only lose half their strength to a quantum computer, so 256-bit keys and hashes stay.

| Component | Quantum status | Decision |
| --- | --- | --- |
| BLAKE3-256 for IDs, MACs and key derivation | Preimage search costs about 2^128 quantum work | Keep |
| ChaCha20-Poly1305 with 256-bit keys | About 2^128 quantum work | Keep |
| Noise with BLAKE2s | Same | Keep |
| Interest and LAN tokens, 16 bytes | They only match values derived from a 256-bit repo ID | Keep |
| DHT 20-byte topic | A lookup key, not a secret | Keep |
| BEP 44 head pointer, Ed25519 | Classical only, and the DHT requires it | Allowed because the pointer is only a hint (6.4) |
| Git object IDs | SHA-1 already has practical collision attacks | New repos default to sha256 |

Object format rule: `swarm init` runs `git init --object-format=sha256` when the local git supports it, and otherwise falls back to sha1 with a printed warning. The flag `--object-format` overrides. In M1, verify that the remote helper works with sha256 repos (git's helper `object-format` capability). If it does not work with the installed git, record that in `DECISIONS.md` and default to sha1 with the warning.

## 5. Data model

Git objects carry all content and need no new format. The only new mutable state is the manifest: a signed, numbered snapshot of one signer's refs. Everything in this section lives in `swarm-core` and does no I/O except where it says it calls `git`.

### 5.1 Manifest fields

A manifest is a CBOR map with unsigned-integer keys. Unknown keys MUST be rejected in v1.

| Key | Name | CBOR type | Required | Meaning |
| --- | --- | --- | --- | --- |
| 0 | `version` | uint | always | Must equal 1. |
| 1 | `repo_id` | bstr(32) | always | The repo this manifest belongs to. |
| 2 | `signer` | bstr(32) | always | Key ID (4.3) of the key bundle that signs it. Root or a maintainer. |
| 3 | `seq` | uint | always | Starts at 1, strictly increases by at least 1 per signer. |
| 4 | `prev` | bstr(32) | always | `manifest_id` of this signer's previous manifest, or 32 zero bytes when `seq` is 1. |
| 5 | `ts` | uint | always | Unix seconds when signed. Informational only (INV-6). |
| 6 | `refs` | map(tstr to bstr) | always | Ref name to object ID. Only `refs/heads/*` and `refs/tags/*`. Object ID is 20 bytes for sha1 repos or 32 for sha256. May be empty. |
| 7 | `head` | tstr | root only, optional | Name of the default branch, for example `refs/heads/main`. |
| 8 | `maintainers` | array of maps | root only, optional | Each entry is a map: key 0 is a bstr(32) key ID, key 1 is an array of tstr globs. Replaces the previous list entirely. |
| 9 | `object_format` | tstr | root, required when `seq` is 1 | Either `sha1` or `sha256`. Never changes after seq 1. |
| 10 | `tombstones` | array of tstr | root only, optional | Ref names that are deleted for everyone. |
| 11 | `forked_from` | bstr(32) | root, `seq` 1 only, optional | Repo ID this repo was forked from. Informational. |
| 12 | `recovery` | bstr(32) | root only, optional, M8 | Key ID of a backup key (suite 2) that may rotate the root key. |
| 13 | `new_root` | bstr(32) | only when `signer` equals the current `recovery` key, M8 | Key ID of the new root key bundle. The new root's own manifests carry the bundle in key 14. |
| 14 | signer\_keys | bstr | always | The signer's public key bundle (4.7): 1984 bytes for suite 1, 32 for suite 2. Its key ID (4.3) must equal key 2. Every manifest carries it, so a manifest verifies with no other input. |
| 15 | suite | uint | always | 1 for the hybrid Ed25519 plus ML-DSA-65 suite. 2 only on a recovery manifest (M8). |

Diagnostic notation of a valid manifest, `seq` 2, sha256 repo:

```text
{ 0: 1,
  1: h'<32 bytes repo id>',
  2: h'<32 bytes signer key id>',
  3: 2,
  4: h'<32 bytes manifest_id of seq 1>',
  5: 1795600000,
  6: { "refs/heads/main": h'<32 bytes>', "refs/tags/v1": h'<32 bytes>' },
  7: "refs/heads/main",
  9: "sha256",
  14: h'<1984 bytes key bundle>',
  15: 1 }
```

### 5.2 Canonical encoding

The encoder is written by hand in `swarm-core::cbor` (about 150 lines). Do not use a general CBOR crate to encode.

- CE-1. Definite lengths only. No indefinite-length items, no tags, no floats, no simple values other than the ones the schema needs.
- CE-2. Integers use the shortest form.
- CE-3. Map keys are sorted by the bytewise order of their encoded form (RFC 8949 section 4.2.1). Duplicate keys are an error.
- CE-4. Decoding is strict: decode with `ciborium`, build the typed struct, re-encode with the canonical encoder, and compare bytes. A mismatch is `non_canonical`.
- CE-5. The envelope is a CBOR array of two byte strings: the manifest bytes and the signature (3373 bytes for suite 1: the 64-byte Ed25519 signature, then the 3309-byte ML-DSA-65 signature). The signature covers `sig_input(manifest_bytes)` from 4.3. Signing the bytes, not a re-encoding, means verification never depends on re-serialization.

### 5.3 Limits and ref-name rules

| Rule | Value |
| --- | --- |
| Maximum envelope size | 1 MiB |
| Maximum refs per manifest | 4096 |
| Maximum ref name length | 255 bytes |
| Maximum maintainers | 64 |
| Maximum globs per maintainer | 16 |
| Allowed ref prefixes | `refs/heads/`, `refs/tags/` |

A ref name MUST satisfy `git check-ref-format` rules: no `..`, no `@{`, no ASCII control characters, no space, none of `~ ^ : ? * [ \`, no component starting with `.` or ending with `.lock`, no leading, trailing or doubled `/`, no trailing `.`. Implement it natively in `swarm-core::refname` and cross-check it against `git check-ref-format` in an end-to-end test over a few thousand generated names.

Glob matching on whole ref names: `*` matches any characters except `/`, `**` matches any characters including `/`, everything else matches literally.

### 5.4 Verification and acceptance

`verify_envelope(bytes) -> Manifest` is pure and returns the first failing code from this list:

1. `too_large`: more than 1 MiB.
2. `non_canonical` or `malformed`: fails CE-4 or the schema.
3. `bad_version`: `version` is not 1.
4. `unknown_suite` or `bad_keys`: `suite` is not allowed for this manifest (1 normally, 2 only on a recovery manifest), `signer_keys` has the wrong length for the suite, or `key_id(signer_keys)` differs from `signer`.
5. `bad_signature`: any half of the signature fails to verify (SIG-1, SIG-2).
6. `bad_refname` or `too_many_refs`: breaks 5.3.
7. `bad_oid_len`: an object ID length does not match `object_format`, or the format is unknown.

`accept(state, manifest)` then applies, in order. It is the only code that changes stored manifests.

1. `wrong_repo`: `repo_id` differs from the repo being synced.
2. `unauthorized`: the signer's key ID is neither the current root key ID nor listed in the current root manifest's `maintainers`. The root key ID is the repo ID until a rotation (M8) changes it. For a repo with no stored state, a manifest is accepted only if `signer` equals `repo_id`, which makes it the root's own manifest, and its `signer_keys` then fixes the root bundle. When the root manifest arrives in the same batch as others, process it first.
3. `forbidden_field`: a non-root signer sets any of keys 7 to 13.
4. `object_format_changed`: differs from the stored value.
5. `out_of_scope`: a non-root signer lists a ref matching none of its globs.
6. `stale`: `seq` is less than the stored `seq` for this signer. Equal `seq` with the same `manifest_id` is a harmless duplicate. Equal `seq` with a different `manifest_id` is `equivocation`: keep the first, record both IDs, surface a warning in `swarm status`.
7. `bad_prev`: only checked when `seq` equals the stored `seq` plus 1, then `prev` must equal the stored `manifest_id`. Gaps are allowed because a peer may only hold a later manifest.
8. On success: write the envelope atomically to `repos/<repo-id>/manifests/<signer-hex>.cbor` (temp file, fsync, rename), update the `signers` row in sqlite, and mark the repo dirty so the sync loop re-resolves and fetches missing objects.

### 5.5 Resolving canonical refs

`resolve(state, git)` returns the ref table that `git ls-remote` shows. It needs `git merge-base --is-ancestor` on the store.

```text
for each ref name N in the union of names in all authorized latest manifests:
    if N is in the root manifest's tombstones: skip N
    cands = [(signer, oid)] from signers whose manifest lists N and whose scope allows N
    vals  = distinct oids in cands
    if len(vals) == 1:            canonical[N] = vals[0]
    elif root in cands:           canonical[N] = root's oid            # root wins
    else:
        d = the oid in vals such that every other oid in vals is an ancestor of d
        if d exists:              canonical[N] = d                      # fast-forward chain
        else:                     conflict[N] = vals; keep the previous canonical[N]
if any object needed for an ancestor test is missing: status[N] = pending, omit N from the list
HEAD = root.head if it is in canonical, else refs/heads/main if present, else the first sorted branch
```

With a single signer this reduces to copying that signer's refs. Delegation (M7) is the only reason the rest exists, so build and test the single-signer path first.

### 5.6 Publishing

```text
publish(repo):
    refs = every ref under refs/heads and refs/tags in the store, filtered by 5.3
    prev = this signer's stored manifest, or none
    if prev exists and prev.refs == refs and root fields unchanged: return (no-op)
    m = { version: 1, repo_id, signer, seq: prev.seq + 1, prev: prev.id or 32 zero bytes,
          ts: now, refs, plus copies of root-only fields from prev when signer is root }
    env = sign(encode(m), signing_key)
    accept(env)                       # run it through the same path as remote manifests
    announce(env) to connected peers; put the DHT head item (M4)
```

### 5.7 Error codes

All codes are lowercase snake case, used in logs, IPC replies and wire `Error` messages: `too_large`, `non_canonical`, `malformed`, `bad_version`, `unknown_suite`, `bad_keys`, `bad_signature`, `bad_refname`, `too_many_refs`, `bad_oid_len`, `wrong_repo`, `unauthorized`, `forbidden_field`, `object_format_changed`, `out_of_scope`, `stale`, `equivocation`, `bad_prev`, `no_signing_key`, `no_peers`, `timeout`, `pack_invalid`, `pq_handshake`, `auth_failed`, `limit_exceeded`.

## 6. Discovery

Discovery turns a repo ID into candidate peer addresses. Six independent routes run at the same time, and the first peer that completes a handshake and serves a valid manifest is enough to start. The routes live in `swarm-disc`, each behind the `Discovery` trait from 3.4.

### 6.1 Discovery manager

- DM-1. `find(repo)` merges the streams of all enabled routes, removes duplicate addresses, and yields each address once per sync attempt.
- DM-2. The sqlite `peers` table is a route of its own, called the cache route. It is tried first, which makes second and later starts need no network discovery at all.
- DM-3. Dial at most 8 candidates at the same time per repo. Stop dialing new ones once 4 sessions are healthy, but keep the streams open to refill when a session drops.
- DM-4. Record each outcome in `peers`: on success set `last_ok` and clear `fails`, on failure increment `fails`. Retry a failing address after `2^fails` seconds, capped at one hour. Delete it after 8 consecutive failures.
- DM-5. Disabling a route (`SWARM_NO_DHT`, `SWARM_NO_LAN`) removes only that route. Nothing else may depend on it (INV-5).

### 6.2 Route A: link hints

Parse the hints (4.4) and dial each one immediately with the connect timeout from the Defaults table. This is the fastest route and works with no DHT and no LAN. It is the reason a shared link should carry the sender's address when it is known.

### 6.3 Route B: LAN multicast

Two peers on the same network find each other with no internet at all.

| Item | Value |
| --- | --- |
| IPv4 group and port | `239.255.42.99`, UDP `42099` |
| IPv6 group and port | `ff02::4242`, UDP `42099` |
| TTL and hop limit | 1 |
| Announce interval | 60 seconds for each seeded repo, plus once at daemon start |
| Query burst | 3 packets, at 0 s, 1 s and 3 s after a sync starts |

Packet layout, all integers big-endian, 1200 bytes at most:

```text
offset  size  field
0       4     magic            MAGIC_LAN (the bytes SWG1)
4       1     kind             1 = announce, 2 = query
5       2     tcp_port         the sender's listening TCP port
7       1     n                number of tokens, 1 to 16
8       16*n  tokens           lan_token(repo, hour) for hour in {previous, current, next}
```

- LAN-1. On receipt, compute `lan_token` for every locally known repo for the previous, current and next hour. On a match, record the address `(source IP, tcp_port)` as a candidate for that repo.
- LAN-2. On a query that matches a repo we seed, send a unicast announce to the query's source address on UDP 42099.
- LAN-3. Ignore packets from our own addresses, packets with a wrong magic or size, and sources above 20 packets per second.
- LAN-4. Bind with address reuse so several local processes can share the port. Tests override the port with `SWARM_LAN_PORT`.

Tokens are keyed hashes of the repo ID, so a listener who does not know a repo ID learns nothing about which repo is being announced.

### 6.4 Route C: Mainline DHT

This route uses the public BitTorrent DHT as a peer rendezvous. The project runs nothing.

- DHT-1. Lookup: `get_peers(topic(repo, d))` for `d` in {today, yesterday}. Treat every returned address as `PeerAddr::Tcp`. Repeat every 60 seconds while a sync is active and every 10 minutes while only seeding.
- DHT-2. Announce: `announce_peer(topic(repo, d), tcp_port)` for `d` in {yesterday, today, tomorrow}, repeated every 15 minutes while the repo is seeded.
- DHT-3. Entry nodes: use the library's built-in list plus any in `config.toml` under `dht.bootstrap`. Persist nodes that answered into the `dht_nodes` table and give them to the library at the next start, so later runs do not depend on the default list.
- DHT-4. All DHT calls have a timeout and failure is not an error: log at debug level and carry on.
- DHT-5. Head pointer, optional, M4: after each publish, store a mutable item (BEP 44) so readers can learn the newest `seq` without finding a peer first.

| Field | Value |
| --- | --- |
| Item key | the Ed25519 public key inside the signer's key bundle (the root bundle for the root signer) |
| Salt | `SALT_HEAD` followed by `derive_key(CTX_HEAD_SALT, repo_id)` (48 bytes, under the 64-byte limit). Never the raw repo ID: BEP 44 sends the salt in the clear to every storing node, and the repo ID is secret |
| BEP 44 `seq` | the manifest `seq` |
| Value | bencoded dictionary with `m` = `manifest_id` (32 bytes) and `s` = manifest `seq` |
| Refresh | every hour, because items expire after about two hours |

A reader that sees a DHT head with a higher `seq` than any manifest a peer has served treats that peer as stale, keeps looking for a fresher one (bounded: up to 3 more DHT lookups and 4 sessions beyond the DM-3 limit, after which it keeps the highest `seq` it found), and never accepts the head alone as proof. The pointer holds no data and no refs.

The repo ID is a secret: knowing it is enough to find and clone the repo, so it never goes onto the DHT. Topics and the head salt are BLAKE3 derivations of it, and the item key is the root's Ed25519 key, which does not reveal the ID.

A link holds only a key ID, so a first-time reader cannot compute the item key. Heads are used from the second sync on, after a manifest has revealed the bundle. BEP 44 requires an Ed25519 signature, so this item is classical only. That is acceptable because it is a hint and never proof (4.8).

### 6.5 Route D: peer exchange

After each handshake, ask the peer for other peers of the same repo (message `GetPeers`, section 7.3). Keep up to 32 returned addresses per session as untrusted candidates. An address that comes with a node ID becomes a pin (NET-11). Every peer reports the address it observed for us in `Hello`; when two different peers report the same address, remember it as our external address, and use it in `swarm link` hints.

### 6.6 Route E: bundles (offline)

A bundle is one file that carries a repo and its signed state over any channel: USB stick, email, a file share.

```text
byte 0..5     MAGIC_BUNDLE                the bytes SWGB1 and a newline
u32           n_envelopes
repeat n:     u32 length, then envelope bytes
u64           git_len                     0 means no git data
git bundle    git_len bytes, as made by git bundle create - --all, run in the store
```

- BUN-1. `swarm export <link> -o repo.swarmbundle` writes this file from the store.
- BUN-2. `swarm import repo.swarmbundle` calls `accept` on every envelope, runs `git bundle verify`, fetches the bundle into `refs/swarm/in/*` in the store, then runs `resolve` and moves canonical refs. It deletes `refs/swarm/in/*` afterwards.
- BUN-3. Import never trusts the bundle's refs. Only signed manifests decide which refs exist.
- BUN-4. Import creates the repo entry if it is new, and the user can then `git clone swarm://<id>` fully offline.

### 6.7 Route F: static mirrors (M8)

Any dumb file host, such as an HTTP server, S3, a git forge's raw files or an IPFS gateway, can serve as an extra route. The layout is plain files:

```text
<base>/<repo-id>/manifests/<signer-hex>.cbor
<base>/<repo-id>/repo.swarmbundle        optional, rebuilt on publish
```

- MIR-1. Add the hint scheme `https` meaning a mirror base URL, and a config list `mirrors`.
- MIR-2. The daemon fetches the manifest files with a 1 MiB cap each, runs them through `accept`, and imports the bundle with BUN-2 when present.
- MIR-3. `swarm mirror export <dir>` writes the layout into a local directory. The user uploads it with whatever tool they like. The project never uploads anywhere itself.
- MIR-4. A mirror is only a transport. It cannot forge anything, because every file is verified like any other input.

## 7. Transport and wire protocol

Peers talk over TCP wrapped in a hybrid post-quantum encrypted channel (Noise XX plus ML-KEM-768), then exchange length-prefixed CBOR messages. A peer proves nothing about who it is. It only shows that it knows a repo ID, and it learns which repos both sides share without revealing any repo ID to a peer that does not already know it. Code lives in `swarm-net`.

### 7.1 Handshake

- NET-1. Transport is TCP. The default listen address is `0.0.0.0:42424`, falling back to an ephemeral port when busy. The chosen port is written to `$SWARM_HOME/daemon.json` as `{"port": N}`.
- NET-2. Phase A, classical: `Noise_XX_25519_ChaChaPoly_BLAKE2s`, prologue the bytes `swarmgit/v1`. The static key is the X25519 secret in `node.key`. Use `snow` for this phase only.
- NET-3. Each Phase A handshake message is sent as a `u16` big-endian length followed by the Noise message. The Noise payload of every Phase A message is random padding of 0 to 255 bytes. The receiver ignores its content. This varies message sizes and gives the obfuscation work in M9 a place to grow.
- NET-4. Phase B, post-quantum, runs inside the encrypted Phase A channel, before any other message. The initiator makes a fresh ML-KEM-768 key pair and sends `PqInit` (type 14): the encapsulation key (1184 bytes), 32 random bytes `r_i`, and random padding. The responder encapsulates to it and replies `PqResp` (type 15): the ciphertext (1088 bytes), 32 random bytes `r_r`, and random padding. Both sides now hold the 32-byte shared secret `ss`. Both messages are logical messages as in 7.2, protected by the Phase A keys. The initiator zeroizes its decapsulation key after use.
- NET-5. Session keys. Let `hh` be the 32-byte Phase A handshake hash. Compute 64 bytes of output from `blake3::Hasher::new_derive_key(CTX_SESSION)` fed `hh || r_i || r_r || ss`, read as extendable output. The first 32 bytes key initiator-to-responder traffic and the last 32 bytes key responder-to-initiator traffic. Then drop the Phase A cipher states, zeroize `ss`, `r_i` and `r_r`, and switch to the framing in 7.2.
- NET-6. Why this is hybrid. If X25519 is broken later, a recorded session reveals `r_i` and `r_r` but not `ss`, so the keys stay safe. If ML-KEM is broken, the attacker cannot read `r_i` and `r_r`, which travelled under the Phase A keys, so the keys stay safe. Both must fail for a recording to be decrypted.
- NET-7. There is no classical-only mode (INV-10). A peer whose first message after Phase A is not the expected `PqInit` or `PqResp` is dropped without a reply. A bad length or a decapsulation error closes the connection and counts as a `pq_handshake` violation toward the ban rule in 7.4.
- NET-8. The whole handshake, Phase A and Phase B together, must finish within the handshake timeout (Defaults). On failure close the socket without sending anything else.
- NET-9. Interest tokens (7.2) use `hh`, the Phase A handshake hash, so a token cannot be replayed on another connection.

* NET-10. Phase C, authentication. Right after NET-5, both sides send `PqAuth` (type 16) at once, as the first message under the new session keys. It holds the sender's node key bundle (1984 bytes) and a suite 1 signature over `auth_input(role)` from 4.3, where `role` is one byte: 1 for the initiator, 2 for the responder. The signature covers the Phase A handshake hash and the ML-KEM public values, so it fits only this connection and cannot be relayed to another. The receiver checks `key_id(bundle)`, verifies both halves, and otherwise closes with `auth_failed`. `Hello` follows only after both `PqAuth` messages are checked.
* NET-11. Pins. A dial may carry an expected node ID, a pin, from a link hint (`@<node-id>`), from a `Peers` entry, or from the `peers` table. If the authenticated node ID differs from the pin, close with `auth_failed`, forget that address for this repo, and do not retry it for 10 minutes. Without a pin, accept any node ID, store it with the address in `peers`, and use it as the pin next time (trust on first use). Add a `node_id` column to the `peers` table.
* NET-12. What this gives. A path attacker cannot read or change a pinned connection, even with a quantum computer, because the transcript signature is hybrid. An unpinned first contact can be intercepted by an active attacker who relays two separate handshakes. That needs no quantum computer and is true of any key exchange with no prior identity. The damage is limited to traffic privacy and blocking, because content is verified by hashes and signatures (INV-1). Discovery routes with no node ID (DHT, LAN) are unpinned on first contact.

### 7.2 Framing and interest matching

Phase A and Phase B messages travel in Noise transport frames: a `u16` big-endian length plus ciphertext of at most 65535 bytes including the 16-byte tag. After NET-5 every frame uses ChaCha20-Poly1305 with the key of its direction. The layout is the same (`u16` big-endian length, then ciphertext of at most 65535 bytes including the 16-byte tag). The 12-byte nonce is 4 zero bytes followed by a `u64` big-endian frame counter that starts at 0 in each direction. There is no associated data. Close the connection without a reply if a counter would pass 2^48 or any frame fails to decrypt. On top of the frame stream, each logical message is a `u32` big-endian length followed by one CBOR array. The length may not exceed the maximum message size (Defaults), otherwise close the connection with `limit_exceeded`.

Right after the handshake each side sends `Hello` at once, without waiting for the other.

1. Each side builds the list of repos it holds or wants (at most 16 at once) and sends `interest(repo, handshake_hash)` for each.
2. For every repo it knows, each side computes the same token and checks whether the peer sent it. The set of matches is the shared set.
3. Sort the shared set by token bytes, ascending. The position in that list is `repo_ix`. Both sides get the same numbering, and later messages use `repo_ix` instead of the repo ID.
4. If the shared set is empty, send `Error no_shared_repo` and close.

### 7.3 Messages

Every message is a CBOR array whose first element is the type number. `rid` is a request ID chosen by the requester, unique on this connection. A response repeats the `rid`. A peer MUST answer unknown types with `Error` and MUST NOT close for them.

| Type | Name | Fields after the type | Reply |
| --- | --- | --- | --- |
| 0 | `Hello` | `version` (1), `caps` (array of tstr), `observed` (bstr, how this side sees the peer's address), `interests` (array of bstr(16)) | none |
| 1 | `Error` | `rid` (0 if none), `code` (tstr, from 5.7 or the codes below), `msg` (tstr, 200 bytes at most) | none |
| 2 | `GetHeads` | `rid`, `repo_ix` | `Heads` |
| 3 | `Heads` | `rid`, array of envelopes, the latest per signer, 65 at most | none |
| 4 | `GetManifest` | `rid`, `repo_ix`, `signer` (bstr 32), `min_seq` | `Manifest` |
| 5 | `Manifest` | `rid`, one envelope or null | none |
| 6 | `Announce` | `repo_ix`, one envelope | none, unsolicited push |
| 7 | `GetPeers` | `rid`, `repo_ix` | `Peers` |
| 8 | `Peers` | `rid`, array of address strings, 32 at most | none |
| 9 | `Want` | `rid`, `repo_ix`, `wants` (array of object IDs), `haves` (array of object IDs) | `PackHeader` then `PackData` |
| 10 | `PackHeader` | `rid`, `size_hint` (uint or null) | none |
| 11 | `PackData` | `rid`, `chunk` (bstr, 256 KiB at most), `final` (bool) | none |
| 12 | `Ping` | `nonce` (uint) | `Pong` |
| 13 | `Pong` | `nonce` | none |
| 14 | PqInit | ek (bstr, 1184 bytes), r\_i (bstr, 32 bytes), pad (bstr, 0 to 255 bytes). Handshake phase B only, never valid later (7.1). | PqResp |
| 15 | PqResp | ct (bstr, 1088 bytes), r\_r (bstr, 32 bytes), pad (bstr, 0 to 255 bytes). Handshake phase B only (7.1). | none |
| 16 | PqAuth | bundle (bstr, 1984 bytes), sig (bstr, 3373 bytes). Handshake phase C only, first message under the session keys, sent by both sides (7.1). | none |

An address in `Peers` is a byte string: one kind byte, then the address bytes, then a big-endian `u16` port, then a 32-byte node ID only for kinds 5 and 7. Kind `4` is IPv4 and `6` is IPv6 without a node ID. Kind `5` is IPv4 and `7` is IPv6 with a node ID. The extra error codes are `no_shared_repo`, `unknown_repo` and `busy`.

`Want` rules, the server side of INV-9:

- WANT-1. Every object ID in `wants` MUST be a tip listed in a manifest the server has accepted. Otherwise reply `Error unauthorized`.
- WANT-2. At most 4096 wants and 1024 haves. Drop haves the server does not have: check with `git cat-file --batch-check`.
- WANT-3. Run `git pack-objects --revs --stdout --thin --delta-base-offset` in the store. Feed it one `<want>` line per want and one `^<have>` line per usable have. Stream its stdout as `PackData` chunks. Abort with `limit_exceeded` if the output passes `max_pack_bytes`.
- WANT-4. One `Want` in flight per connection. A second one gets `Error busy`.
- WANT-5. At most 4 concurrent uploads across all peers, and the configured `upload_kbps` limit when set.

### 7.4 Session behaviour

- Send `Ping` after 30 seconds without traffic. Close after 90 seconds with none.
- A request with no answer within the request timeout (Defaults) fails the request. Two timeouts in a row close the session.
- Accept at most 64 inbound connections and 4 per source IP. After 3 protocol violations from one address, ignore it for 10 minutes.
- Any peer may be both requester and server on one connection. Handle requests concurrently, except `Want`, which is serialized by WANT-4.
- On `Announce`, check the envelope with `accept` (5.4) against the stored state without storing it. When it is newer, sync the repo: fetch its objects, then store the manifest (a node never advertises heads it cannot serve). When that sync succeeds, forward the envelope once to the other healthy sessions that share the repo. Do not forward duplicates.

### 7.5 Reachability

- Outbound dialing always works from behind NAT, so any client can clone from a reachable seeder. Two peers that are both behind NAT cannot connect directly in v1.
- Best effort port mapping with `igd-next` (UPnP or NAT-PMP) at daemon start, M5. Failure is silent.
- A reachable always-on seeder, such as `swarm seed <link>` on a cheap VPS, is the supported fix for the both-behind-NAT case.
- UDP hole punching and relay-through-peer are out of scope until M9. Keep them behind the `Transport` trait so they can be added without touching sync code.

## 8. Sync with git

The daemon keeps one bare git repository, the store, per swarm repo. Network code fills the store, the remote helper copies between the store and the user's repo with ordinary `git` commands, and no git protocol is reimplemented. Code lives in `swarm-store::git` and `swarm-daemon::sync`.

### 8.1 Store rules

- ST-1. Path: `$SWARM_HOME/repos/<repo-id>/store.git`, created with `git init --bare --object-format=<sha1|sha256>`.
- ST-2. Config set at creation: `uploadpack.allowAnySHA1InWant=true`, `receive.denyCurrentBranch=ignore`, `gc.auto=0`, `core.logAllRefUpdates=false`, `transfer.fsckObjects=true`.
- ST-3. `refs/heads/*` and `refs/tags/*` in the store always equal the canonical refs from `resolve` (5.5), nothing else. The daemon alone moves them for remote state. The one exception is the root signer's own push, ST-6.
- ST-4. Objects arrive first, refs second. A ref is moved only after `git rev-list --objects --missing=error <oid>` succeeds for it, which proves the full history is present.
- ST-5. Temporary refs live under `refs/swarm/`. The daemon deletes them at the end of a sync and at startup.
- ST-6. For the root signer, a user push lands directly in `refs/heads/*` and `refs/tags/*` of the store, and `publish` (5.6) then signs what is there. If publish fails, restore the refs from the last stored manifest.
- ST-7. Run `git gc --auto` off by default. Provide `swarm gc <repo>`, which runs `git gc --prune=now` only on a store with no sync or upload in progress.

### 8.2 Sync loop

The daemon runs `sync(repo)` when the helper asks for it, when a manifest arrives through `Announce`, and every 5 minutes for seeded repos.

```text
sync(repo):
    peers   = DiscoveryManager.find(repo)                 # streams, 6.1
    for each session that reaches Ready with repo in the shared set:
        send GetHeads; for each envelope in Heads: accept()   # 5.4, errors only logged
    state   = resolve()                                        # 5.5, may report pending
    tips    = canonical oids not present in the store
    for each tip batch:
        pick the healthiest session that listed these tips
        haves = tips of canonical refs already in the store, capped at 1024
        send Want{wants: tips, haves}
        pipe PackData chunks into: git index-pack --stdin --fix-thin --strict   (cwd: the store)
        on pack_invalid or timeout: mark the session failed, try the next one
    re-run resolve() now that objects are present
    for each canonical ref whose objects pass ST-4: git update-ref <name> <oid>
    drop refs in the store that are no longer canonical
    set last_sync, emit event "synced"
```

Rules:

- SYNC-1. Never wait for all peers. Start fetching as soon as one valid manifest is accepted. Keep collecting heads in the background and re-run if a newer `seq` appears.
- SYNC-2. Sync is resumable. Objects already indexed stay in the store, and a failed pack leaves nothing behind except its temp file, which `git index-pack` removes.
- SYNC-3. Never accept a pack larger than `max_pack_bytes`. Never accept a manifest with a lower `seq` than stored, whichever peer sends it.
- SYNC-4. If a peer's `Heads` carries a `seq` lower than one already stored for the same signer, that peer is stale. Do not drop it, but prefer other peers for `Want`.
- SYNC-5. Run at most one sync per repo at a time. A new trigger during a sync sets a dirty flag and causes one more pass.
- SYNC-6. If no peer is found within `find_timeout` and the store has no refs, fail with `no_peers`. If the store has refs, return success with `state: stale` so offline reads still work.

### 8.3 Remote helper protocol

`git-remote-swarm` is invoked as `git-remote-swarm <remote-name> <url>` with `GIT_DIR` set to the user's repo. It speaks git's line protocol on stdin and stdout.

| Git sends | Helper does | Helper answers |
| --- | --- | --- |
| `capabilities` | nothing | `option`, `fetch`, `push`, then an empty line |
| `option verbosity N` and similar | store the value | `ok`, or `unsupported` for options it ignores |
| `list` or `list for-push` | IPC `sync <repo>`; read the store's refs | one line per ref: `<oid> <refname>`, then `@<branch> HEAD` for the default branch, then an empty line |
| `fetch <oid> <refname>` lines, ended by an empty line | run `git fetch --quiet --no-tags <store-path> <oid>` once for the whole batch, with `GIT_DIR` set | an empty line when done |
| `push <src>:<dst>` lines, ended by an empty line | for each, run `git push <store-path> <src>:<dst>` (force when the line starts with `+`), then IPC `publish <repo>` | `ok <dst>` or `error <dst> <reason>` per ref, then an empty line |

Notes:

- HELP-1. `list` prints refs sorted by name. When the store has no refs yet, print only an empty line, and git treats the repo as empty.
- HELP-2. `git fetch` from a local path needs the tip OIDs to be advertised. The store's refs equal the canonical refs (ST-3), so they are.
- HELP-3. Push to a repo without a signing key returns `error <dst> no signing key (use: git swarm fork)` and exits 0. Do not crash.
- HELP-4. The helper prints progress to stderr only. Anything else on stdout breaks git.
- HELP-5. The helper never opens a network socket. Only the daemon does.

### 8.4 Fork and ownership transfer

`swarm fork <link>` makes a new, independent repo with its own root key from someone else's repo.

1. Sync the source repo into its store.
2. Generate a new root key. Create a store for the new repo ID by copying the source store's objects (`git clone --bare` from the source store path, with the same object format).
3. Publish manifest `seq` 1 with `forked_from` set to the source repo ID and `refs` copied from the source's canonical refs.
4. Print the new link and start seeding it.

To hand a repo to someone else, the current owner adds the new key to `maintainers` or, with M8, rotates the root with the recovery key. Do not design more than that for v1.

## 9. CLI, daemon and storage

The user-facing surface is five commands and a `git` remote. Everything else is plumbing the user never sees. This section fixes the commands, the daemon lifecycle, the local IPC protocol and the on-disk layout.

### 9.1 CLI commands

| Command | Effect | Milestone |
| --- | --- | --- |
| `swarm init` | In the current git repo (create one if none): generate the root key, create the store, publish manifest `seq` 1 from existing refs, add remote `swarm`, print the link. Fails if remote `swarm` exists. | M1 |
| `swarm link [<repo>]` | Print the link, with up to 3 confirmed hints, each pinned to this node's ID. | M1 |
| `swarm status [--json]` | List repos with `seq`, canonical ref count, peers, seeding flag, conflicts and equivocations. | M1 |
| `swarm seed <link>` | Sync, then mark the repo as seeded so it is served and announced after restarts. | M2 |
| `swarm unseed <link>` | Stop serving. Keep the store. | M2 |
| `swarm fork <link>` | See 8.4. Prints the new link. | M2 |
| `swarm export <link> -o FILE` | Write a bundle (6.6). | M6 |
| `swarm import FILE` | Read a bundle. | M6 |
| `swarm key backup <repo> -o FILE` | Copy the key file to FILE with mode 0600. The user is responsible for storing it. | M1 |
| `swarm maintainers add\|remove <repo> <pubkey-base32> [globs]` | Edit the root manifest's maintainers, then publish. | M7 |
| `swarm accept <repo> <ref> <signer-hex>` | Resolve a ref conflict locally by choosing a signer's value. | M7 |
| `swarm mirror export <dir>` | Write the static mirror layout (6.7). | M8 |
| `swarm gc <repo>` | See ST-7. | M5 |
| `swarm daemon [--foreground]`, `swarm daemon stop` | Run or stop the daemon. Users rarely need these. | M1 |

Plain `git clone swarm://<id>`, `git fetch`, `git pull` and `git push swarm <branch>` need no swarm command at all. swarm purge \<repo> deletes a repo's store, manifests and key after confirmation (SEC-13).

Exit codes: `0` success, `1` generic error, `2` usage error, `3` no peers, `4` no signing key, `5` daemon unreachable, `6` verification failure. Machine output: every command that prints data takes `--json` and prints one JSON object on stdout. Human text goes to stderr.

### 9.2 Daemon lifecycle

- DMN-1. One daemon per `SWARM_HOME`. Hold an exclusive `flock` on `$SWARM_HOME/daemon.lock` for the process lifetime. A second instance exits at once with code 0.
- DMN-2. Autostart: the CLI and the helper connect to the IPC socket. On connection refused or missing socket, spawn `swarm daemon --detach` (new session, stdio to `$SWARM_HOME/daemon.log`), then retry the connection up to 20 times, 100 ms apart. If that fails, exit with code 5 and print the log path.
- DMN-3. IPC socket: `$SWARM_HOME/ipc.sock` with mode 0600 on Unix. If the path is over 100 bytes, use `$XDG_RUNTIME_DIR/swarmgit-<first 8 hex of blake3(SWARM_HOME)>.sock`. On Windows use the named pipe `\\.\pipe\swarmgit-<username>`.
- DMN-4. The first IPC call is `hello`. If the daemon's version differs from the client's, the client runs `daemon stop` and autostarts again.
- DMN-5. On start: open sqlite, remove `refs/swarm/*` in every store, start the listener, start discovery for every seeded repo, then announce.
- DMN-6. The daemon exits by itself after `idle_exit` (Defaults) when there are no seeded repos and no connected clients. With seeded repos it keeps running until stopped.
- DMN-7. Handle `SIGTERM` and `SIGINT`: stop accepting, finish in-flight manifest writes, close sessions, exit within 5 seconds.

### 9.3 IPC protocol

Newline-delimited JSON over the local socket. Each request is one line `{"id": N, "cmd": "...", "args": {...}}`. Each reply is one line `{"id": N, "ok": true, "result": {...}}` or `{"id": N, "ok": false, "error": {"code": "...", "msg": "..."}}`. Long commands may first emit lines `{"id": N, "event": "progress", "data": {...}}`. Error codes are those in 5.7 and 7.3.

| `cmd` | `args` | `result` |
| --- | --- | --- |
| `hello` | `client_version` | `daemon_version`, `home`, `port` |
| `init_repo` | `git_dir` | `repo_id`, `link` |
| `sync` | `repo`, `hints` (optional array) | `state` (`fresh` or `stale`), `refs` (count), `head` |
| `publish` | `repo` | `seq`, `manifest_id` or `noop: true` |
| `repo_info` | `repo` | `store_path`, `seq`, `has_key`, `seeding`, `conflicts` |
| `list_repos` | none | array of repo summaries |
| `seed` and `unseed` | `repo` | `seeding` |
| `fork` | `repo` | `repo_id`, `link` |
| `export` and `import` | `repo`, `path` | `bytes` or `repo_id` |
| `status` | `repo` (optional) | peers, sessions, discovery route states |
| `stop` | none | `ok` |

Example:

```text
> {"id": 1, "cmd": "sync", "args": {"repo": "<52-char id>"}}
< {"id": 1, "event": "progress", "data": {"peers": 2, "phase": "fetching"}}
< {"id": 1, "ok": true, "result": {"state": "fresh", "refs": 12, "head": "refs/heads/main"}}
```

### 9.4 Data directory

`SWARM_HOME` defaults to `$XDG_DATA_HOME/swarmgit`, else `~/.local/share/swarmgit` on Linux, `~/Library/Application Support/swarmgit` on macOS and `%APPDATA%\swarmgit` on Windows.

```text
$SWARM_HOME/
  node.key                         hybrid node key, 101 bytes, mode 0600
  config.toml                      optional; absent means all defaults
  daemon.lock  daemon.log  daemon.json  ipc.sock
  swarm.db                         sqlite, schema below
  keys/<repo-id>.key               root signing key seeds
  repos/<repo-id>/
    store.git/                     bare git repo (8.1)
    manifests/<signer-hex>.cbor    latest envelope per signer
```

```sql
PRAGMA user_version = 1;
CREATE TABLE repos(
  id BLOB PRIMARY KEY, object_format TEXT NOT NULL,
  seeding INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, last_sync INTEGER);
CREATE TABLE signers(
  repo BLOB NOT NULL, signer BLOB NOT NULL, seq INTEGER NOT NULL, manifest_id BLOB NOT NULL,
  PRIMARY KEY(repo, signer));
CREATE TABLE equivocations(
  repo BLOB NOT NULL, signer BLOB NOT NULL, seq INTEGER NOT NULL,
  id_a BLOB NOT NULL, id_b BLOB NOT NULL, seen INTEGER NOT NULL);
CREATE TABLE peers(
  repo BLOB NOT NULL, addr TEXT NOT NULL, node_id BLOB, last_ok INTEGER, fails INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(repo, addr));
CREATE TABLE dht_nodes(addr TEXT PRIMARY KEY, last_ok INTEGER NOT NULL);
```

Schema changes need a new `user_version` and a forward migration. Never edit version 1 in place after M1 ships.

### 9.5 Config keys

`config.toml` is optional and every key has a default in section 12. Unknown keys are ignored with a warning.

| Key | Type | Meaning |
| --- | --- | --- |
| `listen` | string | Listen address. |
| `upload_kbps` | integer | Upload cap, 0 for unlimited. |
| `max_pack_bytes` | integer | Largest accepted or served pack. |
| `idle_exit_minutes` | integer | See DMN-6. |
| `dht.enabled` | bool | Mainline DHT route. |
| `dht.bootstrap` | array of string | Extra DHT entry nodes. |
| `lan.enabled` | bool | LAN multicast route. |
| `mirrors` | array of string | Static mirror base URLs, M8. |
| `proxy` | string | SOCKS5 URL for all outbound connections, M9. |

## 10. Threat model and security rules

The system is safe against any peer that lies, because nothing a peer sends is trusted without a hash or a signature. It does not hide who you are from the peers you talk to, and it cannot prove that a validly signed state is the newest one. This section lists who we defend against, the rules that implement it, and the limits we accept.

### 10.1 Adversaries and responses

| Adversary | What it can do | Response |
| --- | --- | --- |
| Malicious peer | Send bad objects, bad manifests, stall, flood | Hash and signature checks (INV-1, INV-2), size and time limits (INV-4), per-IP caps and a 10-minute ban after 3 violations |
| Passive network observer | See IPs, ports, packet sizes, DHT queries | Noise encryption, randomised handshake padding, rotating DHT topics, interest tokens. Cannot hide which IPs talk to the DHT. |
| Active censor | Block IPs, ports, DNS, known entry nodes, fingerprint traffic | Six independent discovery routes (INV-5), no domain name anywhere, bundles for offline transfer, mirrors on any file host, obfuscation and Tor in M9 |
| DHT Sybil or eclipse | Surround a topic and return only attacker peers | Link hints, LAN and PEX bypass the DHT. Attacker peers still cannot forge manifests or objects. |
| Rollback or freeze | Serve an old but validly signed manifest | Stored `seq` only moves up (INV-2). DHT head and several peers expose it. A brand-new client served only by attackers can still be fed an old state: known limit, 10.4. |
| Equivocating signer | Sign two different manifests with the same `seq` | Keep the first, record both, show a warning in `swarm status` (5.4 step 6) |
| Stolen root key | Publish any state | File mode 0600 (4.5), recovery-key rotation in M8. History already signed stays valid. |
| Malicious maintainer | Push junk within granted globs | Scope check, fast-forward-only unless root (5.5). The root can remove the key at any time. |
| Hostile packfile | Decompression bomb, crafted `.git` paths, huge deltas | `--max-input-size`, `transfer.fsckObjects`, `index-pack --strict`, timeouts. Details in SEC-6 to SEC-8. |
| Local user on the same machine | Read keys, talk to the daemon | Key and socket mode 0600, refuse loose key permissions |
| Quantum-capable adversary | Records traffic today and breaks X25519 and Ed25519 later, or forges signatures once a quantum computer exists | Hybrid session keys (ML-KEM-768 mixed with X25519, NET-5) defeat recording. Hybrid signatures (Ed25519 and ML-DSA-65, both required) keep manifests unforgeable if either family falls. The recovery key is hash-based. Symmetric keys are 256-bit. |
| Active man in the middle | Sits on the path of a first contact, relays two separate handshakes, reads and drops traffic. Needs no quantum computer. | Pins plus PqAuth, a hybrid signature over the handshake transcript, detect the relay (NET-10, NET-11). Unpinned first contact stays open to it, as in any key exchange without prior identity. Content stays safe because hashes and manifests are verified. |

### 10.2 Security requirements

Each item is a test in the end-to-end suite (section 12).

- SEC-1. A manifest with one flipped byte anywhere is rejected with `bad_signature` or `non_canonical`. Test every byte offset of a sample envelope.
- SEC-2. A manifest signed by an unlisted key is rejected with `unauthorized`.
- SEC-3. A manifest with lower `seq` than stored is never applied, from any source, including bundles and mirrors.
- SEC-4. Two different manifests with the same signer and `seq` produce a recorded equivocation and keep the first.
- SEC-5. A `Want` for an object ID that is not a verified tip is rejected with `unauthorized`.
- SEC-6. Incoming packs are indexed with `git index-pack --stdin --fix-thin --strict --max-input-size=<max_pack_bytes>` in the store. The store sets `transfer.fsckObjects=true`.
- SEC-7. The store is created with an empty template directory (`git init --bare --template=`), so it has no hooks. The daemon never runs a hook, a filter or any command taken from repo content.
- SEC-8. When the helper fetches into the user's repo it passes `-c fetch.fsckObjects=true -c transfer.fsckObjects=true`, which blocks malicious `.git` and `.gitmodules` paths. The daemon never checks out a working tree.
- SEC-9. Every length read from the network is checked against its limit before any allocation. Parsers never panic on malformed input, and a fuzz target proves it for 10 minutes without a crash.
- SEC-10. Secret keys are never logged, never sent over IPC and never appear in error text. A test greps logs of a full run for the key bytes in hex and base32.
- SEC-11. The daemon opens no listening socket other than the peer port, the LAN UDP port and the local IPC socket. It binds IPC to a local path only.
- SEC-12. Path inputs from IPC (`git_dir`, bundle paths) are canonicalised and used only for the operation requested. Reject paths with NUL bytes.
- SEC-13. `swarm purge <repo>` deletes a store, its manifests and its key (after confirmation) and stops serving it. Operators decide what they seed, and there is no global takedown mechanism by design.
- SEC-14. Both signature halves are required. For a valid suite 1 envelope, flipping one byte in the Ed25519 half fails and flipping one byte in the ML-DSA-65 half fails, each with `bad_signature`. A valid Ed25519 half with a zeroed ML-DSA half is rejected, and so is the reverse.
- SEC-15. A manifest whose `signer_keys` hash differs from `signer`, or whose bundle has the wrong length, is rejected with `bad_keys`. An unknown `suite` is rejected with `unknown_suite`.
- SEC-16. No downgrade. A test peer that skips Phase B, sends `Hello` first, or sends `PqInit` twice is dropped. No option and no code path allows a classical-only session.
- SEC-17. Session keys depend on both halves. With fixed test inputs the derivation in NET-5 matches a vector from the independent Python script, and changing one bit of any of `hh`, `r_i`, `r_r` or `ss` changes both keys. A fake peer that uses a random `ss` fails on the first encrypted frame.
- SEC-18. Authentication. A test peer whose `PqAuth` signs a different transcript, or whose node ID differs from the pin, is rejected with `auth_failed`. A relay fixture that runs two separate handshakes between a pinned client and a server is detected. The same relay against a client with no pin succeeds, and the test asserts that outcome so the limit in NET-12 stays documented.

### 10.3 What each party learns

| Party | Learns |
| --- | --- |
| A peer you connect to | Your IP and port, that you hold or want the repos you share with it, and every object you request |
| Someone who knows the repo ID | Which IPs announce it on the DHT or LAN, because they can compute the topic or token |
| Someone who does not know the repo ID | That an IP uses a DHT and speaks an encrypted protocol. Nothing about repos. |
| DHT nodes | Topic hashes and IPs. Not repo IDs. |
| Mirror hosts | Who downloads which files, and the content, which is public anyway |

### 10.4 Known limits, stated plainly

- Freshness: a client that has never seen a repo cannot prove that the newest state it receives is the newest that exists. Mitigate by asking several peers and checking the DHT head. A link that embeds the latest `manifest_id` would fix it and is a possible v2 option.
- Metadata: peers see your IP. Use Tor (M9) when that matters.
- Unpinned first contact: an active attacker on the path can relay the handshake and read or drop that session's traffic. It needs no quantum computer. Pinned connections are protected (NET-10 to NET-12). Content stays verified either way.
- Young code: ML-KEM and ML-DSA implementations are newer than the classical ones. That is why the hybrid rule (INV-10) and the known-answer tests exist.
- No private repos: encryption of content for chosen readers is out of scope.
- One root key per repo: loss means no more publishing unless M8 recovery was set up. `swarm key backup` exists for this reason.
- Content cannot be removed from the network once other peers hold it.
- Two peers both behind NAT cannot connect directly in v1 (7.5).

## 11. Milestones

Build in this order. Each milestone ends in a working, testable system, and no milestone needs a later one. M0 to M5 deliver the product. M6 to M9 add reach and hardening. Sizes are rough Rust line counts, tests excluded.

| Milestone | Delivers | Size | Needs |
| --- | --- | --- | --- |
| M0 | Core types, canonical CBOR, hybrid post-quantum crypto, vectors, fuzzing | 1,500 | none |
| M1 | Local repo, daemon, CLI, remote helper, single-machine clone | 1,500 | M0 |
| M2 | Hybrid post-quantum encrypted TCP, wire protocol, clone from a peer given by hint | 1,900 | M1 |
| M3 | LAN discovery | 400 | M2 |
| M4 | Mainline DHT discovery and head pointer | 600 | M2 |
| M5 | Peer exchange, seeding, multi-peer fetch, resume, port mapping | 700 | M3, M4 |
| M6 | Bundles for offline transfer | 400 | M2 |
| M7 | Maintainers, scoped refs, conflicts | 700 | M5 |
| M8 | Static mirrors, recovery key | 500 | M6, M7 |
| M9 | SOCKS5 and onion, obfuscation, hardening | 800 | M5 |

For every milestone: write the tests first from the acceptance list, make them pass, then run `scripts/ci.sh` (format, clippy with warnings denied, all tests). All e2e tests use a fresh `SWARM_HOME` per simulated machine and loopback addresses, and set `SWARM_NO_DHT=1 SWARM_NO_LAN=1` unless the test is about those routes.

### 11.1 M0: core types and primitives

Build in `swarm-core` only. No network, no daemon.

1. `consts`, `ids` (base32, `RepoId`, link parse and print per 4.4), `crypto` (sign, verify\_strict, the formulas of 4.3).
2. `cbor`: the canonical encoder and strict decoder of 5.2, `Manifest` and `Envelope` types for every field of 5.1.
3. `refname` and `glob` per 5.3, `verify_envelope` per 5.4, the pure part of `resolve` per 5.5 behind a `trait Ancestry { fn is_ancestor(&self, a: Oid, b: Oid) -> Option<bool>; }` so tests use an in-memory DAG.
4. `scripts/gen_vectors.py` that produces `tests/vectors/*.hex` independently of Rust, with `--check` mode that recomputes and compares.
5. Fuzz targets `link_parse`, `envelope_decode`, `refname_check`.

Post-quantum scope of M0 (4.7, 4.8): add `swarm-core::pq` with the `Signer` and `Kem` traits, suite 1 hybrid sign and verify, key bundles and `key_id`, and the 69-byte key file format. Add the session key derivation of NET-5 as a pure function with its own test vector. Check in the official known-answer tests.

Acceptance:

```bash
cargo test -p swarm-core
python3 scripts/gen_vectors.py --check      # Rust output equals the Python reference
cargo fuzz run envelope_decode -- -max_total_time=60   # no crash
```

- Property: for random valid manifests, `decode(encode(m)) == m` and `encode(decode(b)) == b`.
- Property: random bytes never panic any parser.
- SEC-1: flipping any single byte of a signed envelope fails verification.
- Generated ref names agree with `git check-ref-format` in at least 5,000 cases.

Post-quantum acceptance for M0:

- Known-answer tests pass for ML-KEM-768, ML-DSA-65 and SLH-DSA-SHAKE-128s.
- The cross-implementation test (4.7) passes in both directions when both crate families are available.
- SEC-14, SEC-15 and SEC-17 pass.
- `python3 scripts/gen_vectors.py --check` also covers `key_id` and the session key derivation.

### 11.2 M1: local repo, daemon, CLI, helper

1. `swarm-store`: data directory, sqlite with the schema of 9.4, a `Git` wrapper that runs the `git` binary (init, update-ref, for-each-ref, rev-list, merge-base, index-pack, pack-objects).
2. `swarm-daemon`: lockfile, IPC server with `hello`, `init_repo`, `sync` (local only), `publish`, `repo_info`, `list_repos`, `status`, `stop`. Implement `accept` and `publish` of 5.4 and 5.6 against the store. Key files per 4.5.
3. `swarm-cli`: `init`, `link`, `status`, `key backup`, `daemon`, the autostart of DMN-2, and the `argv[0]` dispatch.
4. The remote helper of 8.3 against the local store, without any network.

Acceptance (save as `tests/e2e/m1.sh`):

```bash
export SWARM_HOME=$(mktemp -d) SWARM_NO_DHT=1 SWARM_NO_LAN=1
SRC=$(mktemp -d); cd $SRC && git init -q -b main && swarm init
ID=$(swarm link)
git commit -q --allow-empty -m one && git push -q swarm main
swarm status --json | jq -e '.repos[0].seq == 2'
cd $(mktemp -d) && git clone -q $ID copy
test $(git -C copy rev-parse HEAD) = $(git -C $SRC rev-parse HEAD)
swarm daemon stop; git ls-remote $ID | grep -q refs/heads/main     # autostart works
cd $SRC && git push -q swarm main                                  # no change
swarm status --json | jq -e '.repos[0].seq == 2'                   # still 2, publish was a no-op
```

- A second `swarm init` in the same repo fails with exit code 1 and a clear message.
- Push from a copy that has no key prints the `no signing key` error (HELP-3) and does not crash.
- SEC-10: a grep of the daemon log for the key in hex and base32 finds nothing.
- Killing the daemon during `publish` leaves either the old or the new manifest, never a torn file.

### 11.3 M2: encrypted transport and clone from a peer

1. `swarm-net`: Noise handshake (7.1), framing (7.2), all messages of 7.3, a session actor with request and response matching, timeouts and limits of 7.4.
2. Server side: serve `GetHeads`, `GetManifest`, `Announce`, `Want` per WANT-1 to WANT-5.
3. Client side: the sync loop of 8.2 using only link hints (6.2) as discovery. Add `seed`, `unseed`, `fork`.
4. Daemon listener, `daemon.json`, `SWARM_LISTEN`.

Post-quantum scope of M2: implement Phase B of the handshake (NET-4 to NET-7), Phase C authentication (NET-10 to NET-12), the ChaCha20-Poly1305 framing of 7.2, and message types 14, 15 and 16 in the codec. Create the hybrid node key file on first daemon start, add `@<node-id>` pins to hints, and store node IDs in `peers`. Phase A alone must never carry application messages.

Acceptance:

```bash
# machine A
export SWARM_HOME=/tmp/a SWARM_LISTEN=127.0.0.1:42501 SWARM_NO_DHT=1 SWARM_NO_LAN=1
# ...init, commit, push as in M1, then: ID=$(swarm link)
# machine B
SWARM_HOME=/tmp/b SWARM_NO_DHT=1 SWARM_NO_LAN=1 git clone "swarm://$ID?h=tcp:127.0.0.1:42501" out
```

- The cloned HEAD equals the source HEAD, and `git fsck` in `out` is clean.
- Hostile test peer, a small Rust test fixture that speaks the protocol: it flips one pack byte, so B rejects with `pack_invalid` and succeeds from the honest peer. It sends a manifest with a bad signature, a lower `seq`, an unlisted signer, and a `Want` for a non-tip. Each is rejected with the code named in 5.7.
- Kill A halfway through a large pack (generate 50 MB of random blobs), then clone from a second seeder C that has the data. The result is correct.
- A fresh client that never saw the repo and is given a wrong repo ID in the hint gets `no_shared_repo`, not a hang.
- Interest tokens: a packet capture of the handshake never contains the repo ID bytes or its base32 form.

Post-quantum acceptance for M2:

- A packet capture of a session shows no recognizable ML-KEM structure, because `PqInit` and `PqResp` travel inside encrypted frames.
- SEC-16 passes. Flipping one bit of the ciphertext makes the connection fail with `pq_handshake`.
- A build that skips Phase B cannot connect to a normal build, in either direction. There is no fallback.

Authentication acceptance for M2:

- A client given a hint with the wrong node ID refuses the connection and reports `auth_failed`. The right node ID connects.
- SEC-18 passes: the relay fixture is detected when the client has a pin, and the test records that it is not detected when the client has no pin.
- After one successful unpinned clone, a second clone from the same address uses the stored node ID as a pin.

### 11.4 M3: LAN discovery

1. Implement 6.3 with `socket2`. Put the packet encoding and token matching in pure functions.
2. Add the route to the discovery manager.

Acceptance:

- Unit tests for packet encode, decode, wrong magic, oversize and rate limiting.
- `tests/e2e/lan.sh` creates two network namespaces joined by a veth pair (needs root, skipped otherwise), runs A and B with no hints, and checks B clones from A.
- A packet capture shows no repo ID in any LAN packet.

### 11.5 M4: Mainline DHT

1. Implement the `Dht` trait over the `mainline` crate. Persist nodes (DHT-3).
2. Add the route (DHT-1, DHT-2) and the head pointer (DHT-5).

Acceptance:

- A local DHT of at least 10 nodes, started with the library's test network helper or hand-rolled with nodes bootstrapped to each other. Unit tests run it in process; the e2e test runs it in its own process (`swarm-dht-testnet`) so that real daemons share it. A announces, B finds A without hints and clones.
- The head pointer test: A publishes `seq` 3, a fake stale peer serves `seq` 2, B notices the pointer is newer and keeps looking until it gets `seq` 3.
- A live smoke test against the real DHT, `SWARM_LIVE_DHT=1 cargo test live_dht -- --ignored`: announce a random repo, find it from a second process within 60 seconds. It is not part of CI because some networks block it.
- With the DHT route disabled, every earlier test still passes (INV-5).

### 11.6 M5: peer exchange, seeding, multi-peer fetch

1. `GetPeers` and `Peers` handling, our observed address logic (6.5).
2. Periodic sync and seeding loop, `Announce` forwarding (7.4), request splitting across sessions, resume.
3. Port mapping with `igd-next`. `swarm gc`.

Acceptance:

- A seeds and B clones from A and seeds. A stops. C, with the DHT testnet, clones from B.
- PEX: C is given only a hint for B. B knows D. C ends up with D in its `peers` table and can fetch from D after B stops.
- A new push on A reaches B within 10 seconds by `Announce` with no polling.
- Two seeders with different content states: the client ends on the highest `seq` and never moves backwards.

### 11.7 M6: bundles

1. `export` and `import` per 6.6 and BUN-1 to BUN-4.

Acceptance:

- Export on A, copy the file, run B inside `unshare -n` (no network), import, then clone works.
- A bundle with one flipped byte in an envelope, in the git part, or in the length fields is rejected and leaves the store unchanged.
- A bundle whose manifests have lower `seq` than stored does not change refs.

### 11.8 M7: maintainers and conflicts

1. Everything in 5.4 steps 2 to 5 for non-root signers, the full `resolve` of 5.5, tombstones.
2. `swarm maintainers add|remove`, `swarm accept`, and publishing by maintainers who hold a key listed in the root manifest.
3. Sync collects `Heads` from all signers.

Acceptance:

- Property test with random commit DAGs and random arrival orders: `resolve` gives the same table for any order of the same manifests.
- Root wins any conflict. Two maintainers pushing diverging commits to the same ref produce `conflict`, and the canonical ref keeps its old value.
- A maintainer pushing outside its globs is rejected with `out_of_scope`. Removing a maintainer makes its refs disappear unless another signer lists them.
- Tombstones win over all signers.

### 11.9 M8: mirrors and recovery

1. Static mirror export and fetch (6.7).
2. Recovery key: manifest fields 12 and 13, effective-root logic in `accept`, `swarm key rotate`.

Post-quantum scope of M8: the recovery key is suite 2 (SLH-DSA-SHAKE-128s, 4.7). Its key file uses the magic `SWK1`, suite byte 2, then the secret key bytes the crate exports. A rotation manifest is signed by the recovery key, carries its 32-byte bundle in key 14 and the new root's key ID in key 13. The new root then publishes its own manifest with its suite 1 bundle.

Acceptance:

- A plain `python3 -m http.server` over an exported directory lets a client with only the `https` hint clone, with the other routes disabled.
- A mirror serving tampered files is rejected.
- Rotation: a manifest signed by the recovery key moves the root. The old root key's later manifests are rejected with `unauthorized`. A rotation signed by anyone else is rejected.

### 11.10 M9: hardening and reach

1. SOCKS5 outbound transport for every connection when `proxy` is set. The onion hint scheme (dial through the proxy). Optional hidden-service hosting through the Tor control port.
2. `ObfsTransport<T: Transport>` wrapper interface and a first implementation: randomised padding and timing in the handshake and a variable-size frame policy. Keep the interface open for pluggable transports.
3. Rate limiting, ban list persistence, fuzzing in CI for 10 minutes each target.
4. Experimental UDP hole punching behind `--experimental-udp`.
5. Documentation: systemd and launchd unit examples, a Tor setup guide.

Acceptance:

- With `proxy` set to a local SOCKS5 test server, no direct connection leaves the process: assert with a firewall rule that drops everything except the proxy port.
- The size distribution of the first 3 handshake messages over 1,000 runs is not constant (a chi-square test against a single value rejects).
- All earlier suites still pass.

## 12. Test plan, defaults and open questions

This section is the single source for numbers, test layers and the decisions an agent should take without asking. When another section names a value from the Defaults table, change it here only.

### 12.1 Defaults

| Name | Value | Used in |
| --- | --- | --- |
| `connect_timeout` | 10 s | 6.2, DM-3 |
| `handshake_timeout` | 10 s | NET-4 |
| `request_timeout` | 30 s | 7.4 |
| `find_timeout` | 30 s | SYNC-6 |
| `max_message_bytes` | 8 MiB | 7.2 |
| `pack_chunk_bytes` | 256 KiB | 7.3 |
| `max_pack_bytes` | 2 GiB | WANT-3, SEC-6 |
| `max_envelope_bytes` | 1 MiB | 5.3 |
| `listen` | `0.0.0.0:42424`, ephemeral if busy | NET-1 |
| `upload_kbps` | 0 (unlimited) | WANT-5 |
| `idle_exit_minutes` | 30 | DMN-6 |
| Inbound connection caps | 64 total, 4 per IP | 7.4 |
| Concurrent dials per repo | 8, target 4 healthy sessions | DM-3 |
| Ping and idle close | 30 s and 90 s | 7.4 |
| Ban after violations | 3 violations, 10 minutes | 7.4 |
| Seeded repo sync interval | 300 s | 8.2 |
| Peer record removal | after 8 consecutive failures | DM-4 |
| LAN port and announce interval | UDP 42099, 60 s | 6.3 |
| DHT lookup, announce, head refresh | 60 s while syncing, 15 min, 1 h | 6.4 |
| Concurrent uploads | 4 | WANT-5 |
| Log level | `info` | 2.2 |
| License of the project | MIT OR Apache-2.0 | repo root |
| Post-quantum parameter sets | ML-KEM-768, ML-DSA-65, SLH-DSA-SHAKE-128s | 4.7 |

### 12.2 Test layers

| Layer | Tool | What it covers | Runs |
| --- | --- | --- | --- |
| Unit | `cargo test` | Pure functions: ids, links, CBOR, refname, glob, resolve, LAN packets | Every commit |
| Property | `proptest` | Round trips, order independence of `resolve`, no panics on random bytes | Every commit, 256 cases; 10,000 nightly |
| Fuzz | `cargo-fuzz` | `link_parse`, `envelope_decode`, `refname_check`, `message_decode`, `lan_packet` | 60 s per target on commit, 10 min nightly |
| End to end | shell and `assert_cmd` | Real binaries, real `git`, loopback sockets, separate `SWARM_HOME` per machine | Every commit |
| Adversarial | `swarm-testpeer` fixture | The hostile peer cases of 11.3 and all SEC items | Every commit |
| Namespace | `unshare`, veth | LAN route, offline import, proxy leak check | Where root is available |
| Live | env-gated `#[ignore]` tests | Real public DHT | Manual |

### 12.3 Harness rules

- Provide `tests/e2e/lib.sh` with `new_machine NAME`, which sets `SWARM_HOME`, an ephemeral `SWARM_LISTEN` and the flags of section 11, and `wait_for CMD TIMEOUT`.
- Set `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` and `GIT_CONFIG_GLOBAL=/dev/null` in every test so results do not depend on the developer's git config.
- Put time behind a `Clock` trait in `swarm-core` so tests can move the day number, hour number and timeouts.
- `swarm-testpeer` is a small crate in the workspace that speaks the wire protocol and can be told to misbehave: flip bytes, lie about `seq`, stall, send oversize frames, answer `Want` for non-tips.
- Tests never touch the real network unless gated by `SWARM_LIVE_*`. Tests never write outside their temp directories.
- A flaky test is a bug. Fix the cause or delete the test, and never add retries.

### 12.4 Definition of done for the project

- All milestone acceptance tests pass on Linux and macOS in CI.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `cargo deny check` pass.
- Every SEC item in 10.2 has a named test.
- A new user on a clean machine can run `git swarm init`, push, share the link, and a second clean machine can `git clone` it, with no configuration file and no other command.
- `README.md` fits on one screen: install, the three commands, the one-paragraph explanation of what no bootstrapping means (1.3).

### 12.5 Open questions and the default to use

Do not stop for these. Use the default, record it in `DECISIONS.md`, and move on.

| Question | Default |
| --- | --- |
| Should the link embed the latest `manifest_id` to defeat freshness attacks? | No in v1. Revisit in v2. |
| Support sha256 repos? | Format field is supported from M0. Run the e2e suite for sha1 until M5, then add sha256 runs. |
| Windows? | Build and unit-test in CI from M1. End-to-end support is best effort after M5. |
| Tags? | Treated as refs like branches (5.5). A moved annotated tag by a non-root signer becomes a conflict. |
| Shallow or partial clones, large-file handling? | Not in v1. Full clones only. |
| Issues and pull requests? | Out of scope. Reserve the `refs/meta/` prefix, and do not allow it in manifests yet. |
| Project name? | `swarmgit` is a placeholder. Keep every name in one `consts` module so it is easy to change. |
| Use QUIC instead of TCP? | No. TCP first. A QUIC transport can implement the `Transport` trait later. |
| Allow a classical-only or post-quantum-only mode? | No. INV-10 and INV-11 forbid both, and there is no option to turn the hybrid off. |
| Which post-quantum crates? | libcrux first, RustCrypto second (4.7). Record the choice in DECISIONS.md. |
| ML-DSA-65 or a higher level? | ML-DSA-65 (NIST category 3). A higher level needs a new suite number, so a version bump. |

### 12.6 Final checklist for the agent

1. `DECISIONS.md` lists every place where the spec was silent or a crate API differed.
2. `CHANGELOG.md` has one entry per milestone with its test commands.
3. No file over 600 lines, no `unsafe`, no `unwrap` outside tests and startup.
4. Run the M1 script on a clean container with only `git`, the `swarm` binary and `jq` installed, and confirm it passes.
5. Run the M2 clone with the network namespace blocking everything except the one loopback port, and confirm it passes.
