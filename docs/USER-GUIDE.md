# Swarmgit user guide

Swarmgit is a peer-to-peer git remote. A repository has a `swarm://` link; any
peer that can reach one holder of that link can clone it. There is no server to
register with and no account. Peers talk over an encrypted, authenticated
channel and find each other through link hints, peers they already know, LAN
multicast, the Mainline DHT and peer exchange. A push is announced to
connected peers, and seeders keep a repo available after its author goes
offline.

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

### How a push reaches the other peer

`git push` publishes into the **local** daemon's store. The remote URL is used
for the sync that happens before a push, not as the publication target. The
daemon then sends an `Announce` with the new signed manifest on every open
session that shares the repo. A peer that receives it checks the signature,
fetches the missing objects, stores the manifest and forwards the
announcement once to its own other sessions. A push therefore spreads through
connected peers within seconds, with no polling.

Sessions stay open while both daemons run: an idle session is pinged after
30 s and closed only after 90 s without any answer. So after B's clone, B's
push usually lands in A's daemon store on its own, and on A a plain
`git pull --no-rebase swarm main` picks it up. Your worktree never changes by
itself; only the daemon store does.

If no session is open (a daemon restarted, or the peers never connected),
fetch the other peer's link directly, as above. For a long-lived workflow you
can add B as a second remote on A:

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
stops serving one but keeps its store. A seeding daemon:

- syncs every seeded repo every 5 minutes, and at once when a connected peer
  announces a newer manifest;
- announces the repo on the LAN every 60 s and on the DHT every 15 minutes;
- refreshes the repo's DHT head pointer after every publish and every hour,
  when it holds the signing key;
- does not exit when idle (an unseeded daemon stops after
  `SWARM_IDLE_EXIT_MINUTES`).

A seeder can relay: if A seeds, B seeds from A and A goes offline, a new
peer C still clones from B, through the DHT or a hint for B.

`swarm fork <link>` creates a new, independent repo (new key and repo ID) from
the current state of an existing one. Its first manifest records
`forked_from`; the source repo is unchanged.

## 7. How peers find each other

A sync collects candidates from every route at once, drops its own node and
duplicate addresses, and dials up to 8 at a time until 4 sessions carry the
repo:

| Route | Source | Notes |
| --- | --- | --- |
| Hints | `h=tcp:host:port[@node]` in the link | Pinned when `@node` is present. |
| Relay | `h=relay:host:port@node` in the link, or a relay address from peer exchange | For peers that cannot accept inbound connections. See "Relays" below. |
| Known peers | the `peers` table in `swarm.db` | Filled by earlier syncs, best first; keeps trust-on-first-use pins. |
| LAN | UDP multicast on port 42099 | Same network segment only. |
| DHT | Mainline DHT lookup | Works with no hint at all; needs outbound UDP. |
| Peer exchange | `GetPeers` on every open session | Up to 32 addresses per reply, stored pinned when they carry a node ID. |

Peer exchange means a hint for one seeder is enough: the client learns the
others, keeps them in its `peers` table, and still reaches them after the
first seeder goes away.

**Multi-peer fetch.** Missing objects are split across the sessions that
advertise them. If one peer's pack fails or comes back short, that peer is
dropped and its share moves to the others; packs already stored are kept, so
a later sync resumes instead of starting over.

**Newest state wins.** Manifests carry a sequence number signed by the root
key, and a client never moves back to a lower one, even when the only peer it
can reach is stale. The DHT also holds a signed head pointer with the latest
seq. When every peer offers less than the head pointer says, the sync keeps
looking on the DHT (up to 3 more rounds and 4 more sessions) before it
settles.

**NAT.** The daemon asks a UPnP gateway, if one answers, to forward the
listen port (one-hour lease, renewed every 30 minutes). Without UPnP, set
`SWARM_ADVERTISE` and forward the port yourself, or rely on your own
outbound dials. A node takes an external IP as its own only after two
different peers report it.

**Relays.** A node behind NAT, CGNAT or mobile data cannot be dialed, and two
such nodes cannot reach each other at all. A relay is a small public server
that both sides connect out to; it joins the two connections and copies bytes.
The swarm handshake still runs end to end through it, so the relay sees only
ciphertext, and the dialer checks the node ID it asked for: a malicious relay
can only break the connection, not impersonate a peer.

*Run a relay* on any machine with a reachable TCP port (default 42425):

```bash
swarm-relay --listen 0.0.0.0:42425 --name relay.example.org:42425
```

`--name` is the exact `host:port` clients use to reach it (repeat it for
several names). Limits: `--max-listeners`, `--max-splices`, `--idle-secs`.

*Use a relay* by setting `SWARM_RELAYS=relay.example.org:42425` on the
machine that cannot be reached (the seeder), and on the cloning machine if it
also needs it. The seeder registers with the relay and then adds
`h=relay:relay.example.org:42425@<node>` to its `swarm link` output and to
the link printed by `swarm init`. Anyone cloning with that link reaches it
through the relay when the direct address does not work. `swarm status` shows
`relay <name> registered` or the error while it is not. When a direct address
for the same node is also being tried, the relay dial waits 1.5 seconds so a
working direct path wins. A relay only splices to a node ID the dialer names.

**Storage.** `swarm gc <repo>` repacks a repo's object store. It keeps every
object reachable from a stored manifest, and fails with `busy` while a sync or
upload of that repo is running; run it again a moment later.

## 8. Configuration

Settings can live in a YAML file instead of environment variables. The `.deb`
and `.rpm` install a commented example at `/etc/swarmgit/config.yaml`
(`docs/config.example.yaml` in the source tree). A per-user file at
`~/.config/swarmgit/config.yaml` overrides it, and `SWARM_CONFIG=/path/file.yaml`
reads one specific file. An environment variable that is set always wins.

```yaml
listen: 0.0.0.0:42424
relays: [relay.example.org:42425]
lan: false          # true/false switches replace the SWARM_NO_* variables
```

Each key matches a variable below with the `SWARM_` prefix dropped and the name
lower-cased (`relay_delay_ms` is `SWARM_RELAY_DELAY_MS`); `lan`, `dht` and
`upnp` take `true` or `false`. Unknown keys are reported and ignored.

| Variable | Meaning |
| --- | --- |
| `SWARM_HOME` | Data directory. Defaults to `~/.swarm`. |
| `SWARM_LISTEN` | Peer listen address, default `0.0.0.0:42424`. If busy, the port becomes ephemeral. |
| `SWARM_ADVERTISE` | Host name or IP placed in `swarm link` hints. Set this behind NAT or in Docker. |
| `SWARM_NO_LAN` | `1` turns off LAN multicast discovery (UDP 42099, groups `239.255.42.99` and `ff02::4242`). Other routes keep working. |
| `SWARM_LAN_PORT` | LAN discovery UDP port, default `42099`. |
| `SWARM_FIND_TIMEOUT` | Seconds a first clone waits for LAN answers, default `30`. |
| `SWARM_NO_DHT` | `1` turns off the Mainline DHT route (announce, peer lookup and the head pointer). Other routes keep working. |
| `SWARM_DHT_BOOTSTRAP` | Comma-separated `host:port` list that replaces the built-in DHT bootstrap nodes (tests point it at a local testnet). |
| `SWARM_DHT_BIND` | IP the DHT UDP socket binds to. |
| `SWARM_DHT_PORT` | DHT UDP port; ephemeral by default. Known DHT nodes are kept in `swarm.db` and reused on the next start. |
| `SWARM_RELAYS` | Comma-separated `host:port` relays to register with and to dial through. Empty by default. |
| `SWARM_RELAY_MODE` | `auto` (default) and `always` register with the configured relays; `off` never registers and ignores relay hints. |
| `SWARM_RELAY_DELAY_MS` | Delay before a relay dial when a direct address for the same node is also tried, default `1500`. |
| `SWARM_NO_UPNP` | `1` skips the best-effort UPnP IGD port mapping of the listen port. |
| `SWARM_MAX_PACK_BYTES` | Largest served or accepted pack, default 2 GiB. |
| `SWARM_LOG` | Tracing filter for the daemon, default `info`. |
| `SWARM_IDLE_EXIT_MINUTES` | Idle shutdown for an unseeded daemon, default 30. |

`$SWARM_HOME/daemon.json` records the actual port, PID and listen address. The
daemon starts on demand and stops with `swarm daemon stop`.

## 9. CLI summary

```text
swarm init [--object-format sha1|sha256] [--json]
swarm link [--json] [repo]
swarm status [--json] [repo]
swarm seed <link> [--json]
swarm unseed <link> [--json]
swarm fork <link> [--json]
swarm key backup <repo> -o FILE
swarm gc [--json] <repo>
swarm daemon [--foreground] | daemon stop
```

`swarm gc` repacks a repo's object store and keeps every object reachable from
a stored manifest. It fails with `busy` while a sync or upload is running.

`repo` accepts a base32 repo ID, hex, or a full `swarm://` link. When omitted,
`swarm` uses the `swarm` remote of the current git repository, or the only
repo in the daemon.

## 10. Troubleshooting

**`auth_failed`** — the hint's `@<node-id>` does not match the peer that
answered, or the peer's key changed. Remove the `@` part to accept a new node
on first contact, or check that you copied the right link.

**`no_shared_repo`** — the peer does not hold the repo in the link. Check the
repo ID and that the other peer is still running `swarm seed` or has a
worktree with that repo.

**`no_peers` / clone hangs** — check the hint host and port:
`docker compose -f compose.peers.yaml exec peer-a ss -tlnp | grep 42424`.
Inside the compose network use `a`/`b`, not `127.0.0.1`.

**Two machines on different networks (home Wi-Fi and mobile data, CGNAT)
cannot clone from each other** — neither can accept inbound connections, so
use a relay (section 7): run `swarm-relay` on a public host, set
`SWARM_RELAYS` on the seeding machine, and clone with the link that
`swarm link` prints afterwards (it contains the `h=relay:` hint). Check
`swarm status` says `relay ... registered`. A `no_peers` error that says
"through a relay" means the relay was tried but the seeder is not registered
with it or is offline.

**A hintless `swarm://<repo>` link finds nothing** — off the local network the
link relies on the Mainline DHT, which needs outbound UDP and a seeder that has
announced in the last 15 minutes; check `SWARM_NO_DHT` is not set on either
side. LAN discovery only reaches
the same network segment (TTL 1) and needs UDP 42099 open on both peers. Check
`swarm status --json` shows `"lan": {"enabled": true}`, that `SWARM_NO_LAN` is
not set, and that the seeder has run `swarm seed` or pushed the repo. Wi-Fi with
client isolation blocks it. Add a hint (`swarm link` prints one) if LAN
is blocked.

**A push did not reach the other peer** — the `Announce` only travels over
open sessions. Check both daemons are running (`swarm status --json`); a
seeder catches up on its next 5-minute sync anyway. To get it now, fetch the
pushing peer's link directly (section 4).

**A clone or fetch ends on an older commit** — every peer it reached was
behind. With the DHT on, the head pointer makes the sync keep looking for a
newer peer; with `SWARM_NO_DHT=1` or no UDP it cannot tell. Add a hint for an
up-to-date peer. A client never moves back to a lower `seq` it already has.

**`busy` from `swarm gc`** — a sync or upload of that repo is running. Retry
when it has finished.

**`no signing key`** — copy the root key as shown in section 4. `swarm fork`
also creates a repo you can push to.

**`git pull` says divergent branches** — add `--no-rebase`, or fetch and merge
`--ff-only` as above.

**Start clean** — `docker compose -f compose.peers.yaml down -v` removes both
state volumes.

## 11. Security notes

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
- The repo ID never appears in DHT data. Lookup topics are derived from it per
  day, and the head pointer's salt is a keyed hash of it, so a DHT observer
  can follow a topic but cannot read the repo ID from it. Treat the repo ID
  (and so the `swarm://` link) as a secret: anyone who has it can find and
  clone the repo.
- Peer exchange and the DHT only suggest addresses. Every session is still
  authenticated, and every manifest and object is verified before it is
  stored or forwarded.

## 12. Testing and development

All tests run in the development image:

```bash
docker compose run --rm dev bash -c 'cargo build --workspace'
docker compose run --rm dev bash -c 'cargo test --workspace'
docker compose run --rm dev bash -c 'cargo clippy --workspace --all-targets -- -D warnings'
docker compose run --rm dev bash -c 'cargo fmt --all -- --check'

# End-to-end: M1 local flow, then M2 clone and hostile-peer paths.
docker compose run --rm dev bash -c 'tests/e2e/m1.sh'
docker compose run --rm dev bash -c 'tests/e2e/m2.sh'

# M4: hintless clone and head pointer on a local 10-node DHT testnet.
docker compose run --rm dev bash -c 'cargo build --workspace && tests/e2e/dht.sh'
# M5: relay over the DHT, peer exchange, Announce, two seeders and gc.
docker compose run --rm dev bash -c 'cargo build --workspace && tests/e2e/m5.sh'

# R1 and R2: relay between isolated Docker networks, including a real clone.
tests/e2e/relay.sh

# Against the public Mainline DHT (needs internet access).
docker compose run --rm -e SWARM_LIVE_DHT=1 dev bash -c \
    'cargo test -p swarm-disc live_dht -- --ignored'

# Everything, as the milestone gate runs it.
scripts/ci.sh
```

The M2 script needs `tcpdump`, `ss`, `sqlite3` and `timeout`; the development
image already contains them.
