<p align="center">
  <img src="assets/logo.svg" alt="Swarmgit" width="120">
</p>

<h1 align="center">Swarmgit</h1>

<p align="center">
  <strong>Git repositories, shared directly between peers.</strong><br>
  Clone from a link. Keep a copy. No central host or account.
</p>

<p align="center">
  <a href="https://t.me/+6a4SrCk6tU8zNWVi"><img alt="Telegram" src="https://img.shields.io/badge/chat-Telegram-26a5e4?style=flat-square&logo=telegram&logoColor=white"></a>
  <a href="https://github.com/purry03/swarmgit/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/purry03/swarmgit/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-ee6352?style=flat-square"></a>
  <img alt="Status: pre-1.0" src="https://img.shields.io/badge/status-pre--1.0-8b8991?style=flat-square">
</p>

Swarmgit is a peer-to-peer Git remote. A repository has a `swarm://` link, and
anyone who can reach a peer holding it can clone it. There is no server to
register with and no account to create. Connections are encrypted and
authenticated with a mix of established and post-quantum cryptography.

You keep the Git commands you already know. Swarmgit finds peers, moves the
objects and checks everything that arrives.

**Contents:** [Get this repository over Swarmgit](#get-this-repository-over-swarmgit) · [Install](#install) ·
[Share a repository](#share-a-repository) · [Clone](#clone-and-update) ·
[Merge requests](#merge-requests) · [More commands](#more-commands) ·
[If a clone cannot connect](#if-a-clone-cannot-connect) · [Relays](#relays) ·
[How it works](#how-it-works) · [Development](#development) · [Documentation](#documentation) · [Community](#community)

## Get this repository over Swarmgit

Swarmgit's own source is shared on Swarmgit. Once Swarmgit is
[installed](#install):

```bash
git clone 'swarm://najdcuvszxvqowm2m4uhprqj6ctime3jhcg3ufczpvsyzazev5qa?h=relay:relay.swarmgit.com:42425@sz7x7keuqjneqlycogdqa2swzzs2z2rmg4pq2ev4xt6nf2dk5uya&h=relay:relay.swarmgit.com:42425@4nhgyz4np4m27mp5g2v6gvhvvjpv2pl5hahfql2lf36jpiquh2tq' swarmgit
```

Keep the quotes, because the link contains characters your shell would
otherwise interpret. Your daemon then serves the copy too, which helps others
get it; `swarm unseed` with the same link stops that.

## Install

You need Git. Linux and macOS are supported.

### From a package

Download from the [releases page](https://github.com/purry03/swarmgit/releases)
or [swarmgit.com](https://swarmgit.com).

| Platform | File | Install |
| --- | --- | --- |
| Debian, Ubuntu | `swarmgit_<version>_amd64.deb` or `_arm64.deb` | `sudo apt install ./<file>` |
| Fedora, RHEL | `swarmgit-<version>-1.x86_64.rpm` or `.aarch64.rpm` | `sudo dnf install ./<file>` |
| Other Linux | `swarmgit-<version>-linux-<arch>.tar.gz` | unpack, put `bin/` on your `PATH` |
| macOS | `swarmgit-<version>-macos-<arch>.tar.gz` | unpack, put `bin/` on your `PATH` |

Each release has a `SHA256SUMS` file. Check your download in the same folder:

```bash
sha256sum --ignore-missing -c SHA256SUMS      # macOS: shasum -a 256 --ignore-missing -c SHA256SUMS
```

The macOS binaries are not notarized by Apple. If macOS blocks them, run
`xattr -dr com.apple.quarantine <unpacked folder>`.

### From source

You need Rust 1.87 or newer and a C compiler.

```bash
git clone https://github.com/purry03/swarmgit.git
cd swarmgit
cargo install --locked --path crates/swarm-cli
ln -s swarm ~/.cargo/bin/git-remote-swarm
```

The last line matters: Git uses `git-remote-swarm` to handle `swarm://` links.
To build the relay server too, run `cargo install --locked --path crates/swarm-relay`.

### Check the install

```bash
swarm --version
swarm status
```

A background daemon starts by itself when a command needs it. After an upgrade,
run `swarm daemon restart` so the new version takes over.

## Share a repository

```bash
mkdir hello && cd hello
git init -b main
swarm init                         # creates the key and a remote called "swarm"
echo hello > README.md
git add README.md
git commit -m 'first commit'
git push swarm main
swarm link                         # prints the link to give to others
```

For an existing project, run `swarm init` inside it, then `git push swarm main`.

Your machine serves the repository for as long as its daemon runs. If it is the
only copy, it must be online when someone clones.

**Back up the repository key.** Without it you can never push again.

```bash
swarm key backup --output hello.key <repo-id>
```

**A link is the permission to read.** Anyone who has it can clone the
repository, so treat the link of a private project like a secret.

Give the repository a name that others see before they clone:

```bash
swarm about <repo-id> --name "hello" --description "a first repository"
```

## Clone and update

```bash
swarm inspect '<link>'             # name, branches and tags, without downloading
git clone '<link>' hello
cd hello
git pull                           # later updates
```

Cloning also makes your daemon serve the repository to others who have its
link. Run `swarm unseed '<link>'` if you would rather it did not.

Pushing needs a signing key. If you do not have one, [open a merge
request](#merge-requests), make your own copy with `swarm fork '<link>'`, or ask
the owner to add you as a maintainer:

```bash
swarm key new <repo-id>                       # you: prints your key ID
swarm maintainers add <repo-id> <key-id>      # the owner
```

## Merge requests

Anyone with the link can propose a branch. No key is needed.

```bash
git checkout -b fix
git commit -am "Fix the thing"
swarm mr create                    # run again after more commits
```

Review and merge it by branch name:

```bash
swarm mr list
swarm mr diff fix
swarm mr comment fix --file src/lib.rs --line 12 -m "Why?"
swarm mr review fix --approve
swarm mr merge fix
```

From a fork, `swarm mr create -R '<link of the original>'` proposes your branch
there. Comments are signed and cannot be edited or deleted.

## More commands

| Task | Command |
| --- | --- |
| See the daemon, your repositories and relays | `swarm status` |
| Keep serving someone's repository | `swarm seed '<link>'` |
| Stop serving it | `swarm unseed '<link>'` |
| Remove it from this machine | `swarm purge <repo-id>` |
| Make an independent copy you can push to | `swarm fork '<link>'` |
| Save a repository to one file | `swarm export '<link>' --output repo.swarmbundle` |
| Load that file elsewhere | `swarm import repo.swarmbundle` |
| Restart the daemon | `swarm daemon restart` |

Every command has `--help` and a manual page, such as `man swarm-mr-create`.

## If a clone cannot connect

`no_peers` means no machine holding the repository could be reached.

1. Check that the sharing machine is on and `swarm status` there shows the
   repository as seeded.
2. Ask for a fresh `swarm link`. Its hints may have changed.
3. Home routers and mobile networks often block incoming connections. Use a
   relay (below), forward TCP port 42424 to the sharing machine, or put both
   machines on one network or VPN.

The [client guide](docs/USER-GUIDE.md) has a fuller troubleshooting section.

## Relays

A relay lets two machines connect when neither can accept an incoming
connection. It only passes encrypted bytes along and cannot read your code. It
does see IP addresses and traffic volume.

No relay is built in. Swarmgit uses the relays you configure, the ones named in
links and public relays it discovers. To use your own, add this to
`~/.config/swarmgit/config.yaml` and restart the daemon:

```yaml
relays:
  - relay.example.org:42425
relay_discovery: off     # optional: use no relays but yours
```

To run a relay, see the [relay guide](docs/RELAY-GUIDE.md).

## How it works

- The repository ID in a link is a hash of the owner's public key. Every update
  is signed, and your machine checks the signature before moving a branch.
- Git objects are checked as they arrive, so a peer, relay or mirror cannot
  alter your code.
- Peers are found through several independent routes, and any one is enough:

| Route | What it does |
| --- | --- |
| Link hints | The link names places to look first |
| Local network | Machines on the same LAN find each other, even offline |
| Mainline DHT | Finds peers without a central directory |
| Peer introductions | A connected peer shares other addresses it knows |
| Relays | Reaches machines behind NAT or a firewall |
| Static mirrors | A signed copy on any file host |
| Bundles | A file you carry across yourself |

Tor and SOCKS5 proxies are supported; see the [Tor guide](docs/TOR-GUIDE.md).

Swarmgit is pre-1.0. The protocol and commands may still change, and it has not
had an external security audit.

## Development

```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
SWARM_NO_DHT=1 SWARM_NO_UPNP=1 cargo test --locked --workspace
tests/e2e/m1.sh                    # end-to-end scripts need a build first
```

GitHub CI runs all of the above and every end-to-end script. The network tests
need Linux and root; [ci.yml](.github/workflows/ci.yml) lists the tools to
install and how the full run ([scripts/ci.sh](scripts/ci.sh)) is started.

After changing the command line, run `scripts/gen-completions.sh` to update the
completions and manual pages.

### Releases

Set `version` in `Cargo.toml`, run `cargo update --workspace` and
`scripts/gen-completions.sh`, date the [changelog](CHANGELOG.md) entry and merge
to `main`. Then push a matching tag:

```bash
git tag v0.2.0
git push origin v0.2.0
```

The [release workflow](.github/workflows/release.yml) tests, builds the packages
and publishes them.

### Layout

| Path | Contents |
| --- | --- |
| `crates/swarm-core` | Keys, signatures, manifests, merge request events |
| `crates/swarm-store` | Data directory, database, bare Git stores, settings |
| `crates/swarm-net` | Encrypted handshake and wire messages |
| `crates/swarm-disc` | Links, LAN and DHT discovery |
| `crates/swarm-daemon` | Background daemon: sync, serve, publish |
| `crates/swarm-cli` | `swarm` and `git-remote-swarm` |
| `crates/swarm-relay` | Relay server and client |
| `crates/swarm-testpeer` | Hostile peer used by tests |
| `fuzz`, `tests` | Fuzz targets, test vectors, end-to-end scripts |

## Documentation

| Document | What it covers |
| --- | --- |
| [Client guide](docs/USER-GUIDE.md) | Installing, daily use, keys, settings and troubleshooting |
| [Relay guide](docs/RELAY-GUIDE.md) | Running a relay and connecting clients to it |
| [Tor guide](docs/TOR-GUIDE.md) | SOCKS5 proxying and onion services |
| [Example settings](docs/config.example.yaml) | Every setting, commented |
| [Changelog](CHANGELOG.md) | What changed in each version |

## Community

Questions, bug reports and ideas are welcome on
[Telegram](https://t.me/+6a4SrCk6tU8zNWVi).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
