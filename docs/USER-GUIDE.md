# Swarmgit user guide

Swarmgit is a peer-to-peer git remote. A repository has a `swarm://` link; any
peer that can reach one holder of that link can clone it. There is no server to
register with and no account. M2 adds an encrypted wire protocol and the two
commands a seeder needs: `swarm seed` and `swarm unseed`.

This guide starts two peers in Docker so you can try a clone, a push and a pull
without touching your host toolchain.

## 1. Requirements

- Docker with Compose v2.
- Nothing else. The peer image builds the `swarm` binary and installs git.

> The repository pins a toolchain that the host's `cargo` may be too old to
> read. Build and test through Docker; do not run `cargo` on the host.

## 2. Start the two peers

From the repository root:

```bash
docker compose -f compose.peers.yaml up -d --build
```

This creates two containers:

| Service | Host name inside the network | State volume |
| --- | --- | --- |
| `peer-a` | `a` | `a-state` mounted at `/state` |
| `peer-b` | `b` | `b-state` mounted at `/state` |

Each peer listens on TCP `42424` and advertises itself as `a` or `b`. Those
names resolve on the Compose network, so a link printed by one peer works on
the other.

Open a shell on a peer:

```bash
docker compose -f compose.peers.yaml exec peer-a bash
```

Everything a peer owns lives under `/state`: the daemon home in
`/state/home`, and your worktrees in `/state/work`.

To start over and delete both peers' state:

```bash
docker compose -f compose.peers.yaml down -v
```

## 3. Make a repo on peer A and clone it on peer B

### Peer A

```bash
cd /state/work
mkdir src && cd src
git init -b main
swarm init
printf 'hello from A\n' > README.md
git add README.md
git commit -m one
git push swarm main
swarm link
```

The last command prints one line:

```text
swarm://<repo-id>?h=tcp:a:42424@<node-id>
```

Keep it. The hint says where the peer is; the `@<node-id>` part pins the peer's
identity. `swarm init` already added a remote named `swarm` with that link, so
later pushes need only `git push swarm main`.

### Peer B

Open a second shell:

```bash
docker compose -f compose.peers.yaml exec peer-b bash
cd /state/work
git clone '<paste the link from A>' clone
cd clone
git remote rename origin swarm
git log --oneline
```

The clone goes through B's local daemon, which dials A, authenticates it, and
copies the pack into B's bare store before the helper fills your worktree.
`git fsck` should be clean.

> **Pins.** The first clone may omit `@<node-id>`. The daemon then records the
> authenticated node ID in its `peers` table and uses it as a pin next time.
> If the hint carries the wrong node ID, the clone fails with `auth_failed`.

## 4. Push from B and pull on A

Only the root signer may publish. A fresh clone has no signing key, so first
copy A's key to B. The key file is
`/state/home/keys/<repo-id>.key`, mode `0600`.

From the host:

```bash
docker compose -f compose.peers.yaml exec -T peer-a \
  tar -C /state/home/keys -cf - . \
  | docker compose -f compose.peers.yaml exec -T peer-b \
      bash -c 'mkdir -p /state/home/keys && tar -C /state/home/keys -xf -'
```

Now, on **peer B**:

```bash
cd /state/work/clone
printf 'from B\n' > b.txt
git add b.txt
git commit -m two
git push swarm main
```

B's push updates B's daemon store and publishes a new signed manifest. A can
pull it from **B's** listener. On **peer A**, find B's link:

```bash
docker compose -f compose.peers.yaml exec peer-b bash -lc \
  'cd /state/work/clone && swarm link'
```

Then, on **peer A**:

```bash
cd /state/work/src
git fetch '<paste B link>' main
git merge --ff-only FETCH_HEAD
```

Finally, make a change on A and pull it on B:

```bash
# on A
printf 'again from A\n' > a.txt
git add a.txt
git commit -m three
git push swarm main

# on B
cd /state/work/clone
git pull --no-rebase swarm main
```

`--no-rebase` matters on git 2.39: the helper advertises the `HEAD` symref, and
without it git can report divergent branches. `git fetch swarm main` followed
by `git merge --ff-only FETCH_HEAD` is equivalent.

### Why A pulls from B explicitly

`git push` publishes into the **local** daemon's store. The remote URL is used
for the sync that happens before a push, not as the publication target. So the
push from B leaves the new manifest on B. A reads it by fetching B's link. For
a long-lived workflow you can add B as a second remote on A:

```bash
git remote add peer-b '<B link>'
git pull --no-rebase peer-b main
```

`swarm status --json` shows the current `seq` on each side. After the two pushes
above, both peers should report the same sequence number.

## 5. Run the whole tour automatically

The script runs the same flow and checks every HEAD:

```bash
./scripts/peers-demo.sh          # build, start and run
./scripts/peers-demo.sh --reset  # wipe both volumes first
```

It prints `PASS` when clone, push from B and pull on A all match.

## 6. Seeding an existing repo

`swarm seed` syncs a link and marks the repo as served. Use it on a machine
that should keep the data available without a worktree:

```bash
SWARM_HOME=/state/home SWARM_LISTEN=0.0.0.0:42424 SWARM_ADVERTISE=seeder.example \
  swarm seed 'swarm://<repo-id>?h=tcp:source:42424@<source-node>'
```

The daemon serves every seeded repo whose store exists. `swarm unseed <link>`
stops serving one but keeps its store. A newly seeded repo is synced again
every five minutes.

`swarm fork <link>` creates a new, independent repo (new key and repo ID) from
the current state of an existing one. Its first manifest records
`forked_from`; the source repo is unchanged.

## 7. Configuration

| Variable | Meaning |
| --- | --- |
| `SWARM_HOME` | Data directory. Defaults to `~/.swarm`. |
| `SWARM_LISTEN` | Peer listen address, default `0.0.0.0:42424`. If busy, the port becomes ephemeral. |
| `SWARM_ADVERTISE` | Host name or IP placed in `swarm link` hints. Set this behind NAT or in Docker. |
| `SWARM_NO_DHT`, `SWARM_NO_LAN` | Disable discovery routes that do not exist yet; harmless at M2. |
| `SWARM_MAX_PACK_BYTES` | Largest served or accepted pack, default 2 GiB. |
| `SWARM_LOG` | Tracing filter for the daemon, default `info`. |
| `SWARM_IDLE_EXIT_MINUTES` | Idle shutdown for an unseeded daemon, default 30. |

`$SWARM_HOME/daemon.json` records the actual port, PID and listen address. The
daemon starts on demand and stops with `swarm daemon stop`.

## 8. CLI summary

```text
swarm init [--object-format sha1|sha256] [--json]
swarm link [--json] [repo]
swarm status [--json] [repo]
swarm seed <link> [--json]
swarm unseed <link> [--json]
swarm fork <link> [--json]
swarm key backup <repo> -o FILE
swarm daemon [--foreground] | daemon stop
```

`repo` accepts a base32 repo ID, hex, or a full `swarm://` link. When omitted,
`swarm` uses the `swarm` remote of the current git repository, or the only
repo in the daemon.

## 9. Troubleshooting

**`auth_failed`** — the hint's `@<node-id>` does not match the peer that
answered, or the peer's key changed. Remove the `@` part to accept a new node
on first contact, or check that you copied the right link.

**`no_shared_repo`** — the peer does not hold the repo in the link. Check the
repo ID and that the other peer is still running `swarm seed` or has a
worktree with that repo.

**`no_peers` / clone hangs** — check the hint host and port:
`docker compose -f compose.peers.yaml exec peer-a ss -tlnp | grep 42424`.
Inside the compose network use `a`/`b`, not `127.0.0.1`.

**`no signing key`** — copy the root key as shown in section 4. `swarm fork`
also creates a repo you can push to.

**`git pull` says divergent branches** — add `--no-rebase`, or fetch and merge
`--ff-only` as above.

**Start clean** — `docker compose -f compose.peers.yaml down -v` removes both
state volumes.

## 10. Security notes

- The TCP channel is Noise XX plus ML-KEM-768, then ChaCha20-Poly1305 frames.
  There is no classical-only fallback.
- Both sides sign the handshake transcript with a hybrid key and prove a node
  identity. A pinned peer cannot be impersonated, even by a future quantum
  computer.
- An unpinned first contact can be relayed by an active attacker. The relay
  still cannot forge content, because every manifest and object is verified.
  Store the pin after the first contact to close that window.
- Interest tokens are keyed hashes of the repo ID and handshake hash. The repo
  ID never appears on the wire during a handshake.

## 11. Testing and development

All tests run in the development image:

```bash
docker compose run --rm dev bash -c 'cargo build --workspace'
docker compose run --rm dev bash -c 'cargo test --workspace'
docker compose run --rm dev bash -c 'cargo clippy --workspace --all-targets -- -D warnings'
docker compose run --rm dev bash -c 'cargo fmt --all -- --check'

# End-to-end: M1 local flow, then M2 clone and hostile-peer paths.
docker compose run --rm dev bash -c 'tests/e2e/m1.sh'
docker compose run --rm dev bash -c 'tests/e2e/m2.sh'
```

The M2 script needs `tcpdump`, `ss`, `sqlite3` and `timeout`; the development
image already contains them.
