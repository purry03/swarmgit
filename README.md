# Swarmgit

Peer-to-peer git over an encrypted, post-quantum transport. A repository is
identified by a `swarm://` link; any peer that can reach a holder of that link
can clone it. No server registration and no accounts.

This tree currently implements milestones M0 to M5: core types, the local
daemon/CLI/remote helper, the encrypted wire protocol with clone, push and
pull between peers, LAN multicast discovery, the Mainline DHT (a bare
`swarm://<repo>` link clones from any reachable seeder), and peer exchange,
seeding, `Announce` forwarding and multi-peer fetch.

## Try two peers in Docker

```bash
docker compose -f compose.peers.yaml up -d --build
./scripts/peers-demo.sh
```

`peers-demo.sh` creates a repo on `peer-a`, clones it on `peer-b`, pushes from
B, pulls back on A, and checks the HEADs. Shell into a peer with:

```bash
docker compose -f compose.peers.yaml exec peer-a bash
```

See **[docs/USER-GUIDE.md](docs/USER-GUIDE.md)** for the interactive walkthrough,
key copying, seeding, configuration and troubleshooting.

## Build and test

The repository pins a toolchain that some host `cargo` versions cannot read.
Use the development image:

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

The full milestone gate (fmt, clippy, tests and every e2e script, including
the network-namespace LAN test, which needs root) runs in the privileged dev
container:

```bash
scripts/ci.sh
```

## Packages

`scripts/package.sh` builds `.deb` and `.rpm` files inside a dedicated Docker
image and writes them to `dist/` (gitignored):

```bash
scripts/package.sh                 # both formats
scripts/package.sh --deb           # only the .deb
scripts/package.sh --rpm -o /tmp/pkgs
VERSION=0.2.0 scripts/package.sh   # override the Cargo.toml version
```

Both packages install `/usr/bin/swarm` plus the `git-remote-swarm` symlink, and
depend on `git` and `ca-certificates`. The daemon is started on demand, so no
systemd unit is shipped.

## Layout

- `crates/swarm-core` - types, canonical CBOR, manifests, resolve, hybrid PQ.
- `crates/swarm-store` - `$SWARM_HOME`, sqlite, bare git stores.
- `crates/swarm-net` - Noise + ML-KEM handshake, ChaCha framing, messages.
- `crates/swarm-disc` - link-hint, LAN multicast and Mainline DHT discovery,
  plus the `swarm-dht-testnet` helper.
- `crates/swarm-daemon` - IPC daemon, peer listener, sync, publish.
- `crates/swarm-cli` - the `swarm` binary and `git-remote-swarm` helper.
- `crates/swarm-testpeer` - hostile peer fixtures for the acceptance tests.
