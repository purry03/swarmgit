<p align="center">
  <img src="assets/logo.svg" alt="Swarmgit" width="120">
</p>

<h1 align="center">Swarmgit</h1>

<p align="center">
  <strong>Git repositories, shared directly between peers.</strong><br>
  Clone from a link. Keep a copy. No central host or account.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-ee6352?style=flat-square"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-stable-101012?style=flat-square&logo=rust&logoColor=white">
  <img alt="Status: pre-1.0" src="https://img.shields.io/badge/status-pre--1.0-8b8991?style=flat-square">
</p>

Swarmgit is a peer-to-peer Git remote. A repository has a `swarm://` link, and
anyone who can reach a peer holding that repository can clone it. There's no
server to register with and no account to create. Connections are encrypted and
authenticated, using a hybrid of established and post-quantum cryptography.

You keep the Git commands you already know. Swarmgit handles finding peers,
moving objects and checking what arrives. A repository can be served from a
laptop, a server, a bundle file, a static mirror or a relay.

## Install

To use Swarmgit you need Git. Building the packages also needs Docker with
Compose v2.

Packages are built inside Docker and written to `dist/`:

| Platform | Architectures | Formats |
| --- | --- | --- |
| Linux | x86_64, aarch64 | `.deb`, `.rpm`, `.tar.gz` |
| macOS | x86_64, aarch64 (Apple Silicon) | `.tar.gz` |

From the repository root:

```bash
scripts/package.sh --deb --arch x86_64
sudo apt install ./dist/swarmgit_*_amd64.deb   # Debian / Ubuntu

scripts/package.sh --rpm --arch x86_64
sudo dnf install ./dist/swarmgit-*.x86_64.rpm  # Fedora / RHEL

scripts/package.sh --tar --macos               # macOS archives
```

The Linux packages install `/usr/bin/swarm` and a `git-remote-swarm` symlink,
plus shell completions for bash, zsh and fish. They depend on `git` and
`ca-certificates`. The client daemon starts on demand, so the client package
has no systemd unit. Archives contain `bin/swarm`, `bin/git-remote-swarm` and
`bin/swarm-relay`; put `bin/` on your `PATH`. Linux binaries link against glibc
2.17, so they run on older distributions.

Each run writes `dist/SHA256SUMS`. With `--sign` (or `SIGN_KEY=<gpg key id>`)
you also get `SHA256SUMS.asc` and the public key; check them with
`gpg --verify SHA256SUMS.asc SHA256SUMS && sha256sum -c SHA256SUMS`. The macOS
archives aren't signed or notarized by Apple, so a downloaded binary may need
`xattr -d com.apple.quarantine swarm`.

The same script builds `swarm-relay`, the relay server package with a systemd
unit. No relay is built into the client: it uses the relays you configure, the
relay hints in links and public relays it discovers. Run your own if you don't
want to depend on strangers. See the [relay guide](docs/RELAY-GUIDE.md).

To build from source, see [Build and test](#build-and-test).

## Create and share a repository

```bash
mkdir hello && cd hello
git init -b main
swarm init
printf 'hello\n' > README.md
git add README.md
git commit -m 'first commit'
git push swarm main
swarm link
```

`swarm init` creates the repository key and a remote called `swarm`.
`swarm link` prints the link to share:

```text
swarm://<repo-id>?h=tcp:host:42424@<node-id>
```

Anyone who can reach the peer named in the link can clone the repository.

The owner or a maintainer can also give the repository a name and description,
which `swarm inspect` and the web explorer show:

```bash
swarm about <repo> --name "hello" --description "a first repository"
```

`swarm status` shows what the daemon is serving. To look at a repository before
cloning it, `swarm inspect '<link>'` checks its signed metadata without
downloading any objects.

## Clone a repository

```bash
git clone 'swarm://<repo-id>?h=tcp:host:42424@<node-id>' hello-copy
cd hello-copy
git remote rename origin swarm
```

After that, `git fetch`, `git pull` and `git push swarm main` work as usual.

> Pushing from a clone needs the repository signing key. To share write access,
> copy the key or authorize a maintainer key. The
> [client guide](docs/USER-GUIDE.md) covers both.

## Try the two-peer demo

If you have Docker, you can try Swarmgit without installing the client. This
builds two peers and runs a clone, push and pull between them:

```bash
docker compose -f compose.peers.yaml up -d --build
./scripts/peers-demo.sh
```

Open a shell on a peer with:

```bash
docker compose -f compose.peers.yaml exec peer-a bash
```

The [client guide](docs/USER-GUIDE.md) has the full walkthrough, including key
copying, seeding and configuration.

## Why Swarmgit

- **A link is enough.** No registry or company assigns the name; the link
  identifies the repository.
- **Any copy can serve it.** A laptop, a server, a bundle, a mirror or a relay
  all work. One copy is enough.
- **You can check what you get.** Swarmgit verifies objects and signed updates
  before they reach your repository, and connections are encrypted.
- **Git stays Git.** Your editor, hooks, history and CI keep working. There's
  no new workflow to learn.

## How it finds a copy

| Route | What it does |
| --- | --- |
| Link hints | The link names a few places to look first. |
| Local network | Peers on the same LAN find each other, even without internet. |
| Global lookup | The Mainline DHT searches for a peer without a central directory. |
| Peer introductions | A connected peer points you to other copies it knows. |
| Offline bundles | Export the repository to one file, move it, then import and clone. |
| Static mirrors | Host a signed copy on any file host; clients check what they download. |
| Relays | Connect out through a relay when NAT or a firewall blocks direct connections. |

## Build and test

Some host `cargo` versions can't read the toolchain this repo pins, so build
through the development image:

```bash
docker compose run --rm dev bash -c 'cargo build --workspace'
docker compose run --rm dev bash -c 'cargo test --workspace'
docker compose run --rm dev bash -c 'cargo clippy --workspace --all-targets -- -D warnings'
docker compose run --rm dev bash -c 'cargo fmt --all -- --check'

docker compose run --rm dev bash -c 'tests/e2e/m1.sh'
docker compose run --rm dev bash -c 'tests/e2e/m2.sh'
docker compose run --rm dev bash -c 'tests/e2e/dht.sh'   # M4, local DHT testnet
docker compose run --rm dev bash -c 'tests/e2e/m5.sh'    # M5, relay, PEX, seeders
```

To run everything at once (fmt, clippy, tests and all the e2e scripts), use the
privileged dev container. The LAN test needs root because it sets up network
namespaces:

```bash
scripts/ci.sh
```

## Documentation

| Document | What it covers |
| --- | --- |
| [Client guide](docs/USER-GUIDE.md) | Docker demo, seeding, bundles, mirrors, maintainers, configuration and troubleshooting. |
| [Relay guide](docs/RELAY-GUIDE.md) | Installing and running a relay, client registration, capacity and privacy. |
| [Tor guide](docs/TOR-GUIDE.md) | SOCKS5 proxying and onion services. |
| [Relay design](docs/RELAY-DESIGN.md) | Relay protocol and network discovery design. |
| [Changelog](CHANGELOG.md) | What changed in each milestone. |
| [Protocol spec](Swarmgit%20Agent-Buildable%20Spec%20for%20a%20Zero-Setup%20P2P%20Git.md) | The full protocol and implementation spec. |

## Project status

Swarmgit is pre-1.0. The tree includes milestones M0 through M9 and relay
support: core types, the daemon with its CLI and remote helper, encrypted
transport, LAN and DHT discovery, peer exchange, multi-peer fetch, bundles,
maintainers, static mirrors, key recovery and rotation, relays, SOCKS5 and Tor,
traffic shaping, inbound limits and experimental UDP hole punching. The
protocol and CLI may still change before 1.0.

## Layout

- `crates/swarm-core`: types, canonical CBOR, manifests, resolve, hybrid PQ.
- `crates/swarm-store`: `$SWARM_HOME`, sqlite, bare git stores.
- `crates/swarm-net`: Noise + ML-KEM handshake, ChaCha framing, messages.
- `crates/swarm-disc`: link-hint, LAN multicast and Mainline DHT discovery,
  plus the `swarm-dht-testnet` helper.
- `crates/swarm-daemon`: IPC daemon, peer listener, sync, publish.
- `crates/swarm-cli`: the `swarm` binary and `git-remote-swarm` helper.
- `crates/swarm-relay`: relay server and client for peers that can't accept
  inbound connections.
- `crates/swarm-testpeer`: hostile peer fixtures for the acceptance tests.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
