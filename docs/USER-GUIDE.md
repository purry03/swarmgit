# Swarmgit client guide

Swarmgit is a peer-to-peer git remote. A repository has a `swarm://` link, and
anyone who can reach a peer holding that repository can clone it. There is no
server to register with and no account. You keep using `git`; the `swarm`
command handles what git does not, such as links, keys and merge requests.

This guide covers installing the client and using it day to day. To run a
server that connects peers behind NAT, see the [relay guide](RELAY-GUIDE.md).

## 1. How it works

| Term | Meaning |
| --- | --- |
| Repo ID | The permanent name of a repository, derived from its first signing key. |
| Link | `swarm://<repo-id>`, usually with hints saying where to find a copy. |
| Daemon | A background process on your machine. It keeps its own copy of each repo and talks to peers. |
| Node ID | The identity of one machine's daemon. |
| Seeding | Serving a repo to other peers. |
| Root key | The key that may publish to a repo. It is created by `swarm init`. |
| Maintainer key | A second key that the root authorizes to publish. |
| Seq | The version number of a repo's signed state. Each publish raises it. |

When you `git push` to a `swarm://` remote, the objects go to your **own**
daemon, which signs the new state and tells connected peers. When you
`git clone` or `git fetch`, your daemon finds peers, downloads from them,
checks every signature and object, and then hands the result to git.

Two things follow from this:

- A repository is available only while some machine that has it is online and
  reachable. Keep one running if others depend on it (section 9).
- Anyone who has the link can read the repository. There is no read password.
  Treat the link as a secret if the code is private.

## 2. Install

You need `git`. Packages are at `https://swarmgit.com/downloads/`.

| Platform | File (x86_64) | File (ARM64) |
| --- | --- | --- |
| Debian, Ubuntu | `swarmgit_0.2.0_amd64.deb` | `swarmgit_0.2.0_arm64.deb` |
| Fedora, RHEL | `swarmgit-0.2.0-1.x86_64.rpm` | `swarmgit-0.2.0-1.aarch64.rpm` |
| Other Linux | `swarmgit-0.2.0-linux-x86_64.tar.gz` | `swarmgit-0.2.0-linux-aarch64.tar.gz` |
| macOS | `swarmgit-0.2.0-macos-x86_64.tar.gz` | `swarmgit-0.2.0-macos-aarch64.tar.gz` |

### Check the download

```bash
curl -O https://swarmgit.com/downloads/swarmgit_0.2.0_amd64.deb
curl -O https://swarmgit.com/downloads/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
```

On macOS use `shasum -a 256 <file>` and compare it with the line in
`SHA256SUMS`. This catches a damaged download. The checksums are not signed.

### Debian and Ubuntu

```bash
sudo apt install ./swarmgit_0.2.0_amd64.deb
```

### Fedora and RHEL

```bash
sudo dnf install ./swarmgit-0.2.0-1.x86_64.rpm
```

### Archive (any Linux, or macOS)

```bash
tar xzf swarmgit-0.2.0-macos-aarch64.tar.gz
cd swarmgit-0.2.0-macos-aarch64
sudo cp -R bin/swarm bin/git-remote-swarm bin/swarm-relay /usr/local/bin/
```

Both `swarm` and `git-remote-swarm` must be on your `PATH`; git finds the
second one by name. The macOS binaries are not notarized, so a browser download
may need `xattr -d com.apple.quarantine /usr/local/bin/swarm`. Windows is not
supported.

### What the packages add

The `.deb` and `.rpm` install `/usr/bin/swarm`, the `git-remote-swarm` link,
shell completions for bash, zsh and fish, manual pages (`man swarm`) and an
example settings file at `/etc/swarmgit/config.yaml`. With an archive, generate
completions yourself:

```bash
swarm completions bash > ~/.local/share/bash-completion/completions/swarm
```

### Upgrade

Install the newer package, then restart the daemon:

```bash
swarm daemon restart
```

A daemon that was already running keeps using the old program until it is
restarted, so new fixes do not take effect before this.

### Uninstall

```bash
swarm daemon stop
sudo apt remove swarmgit        # or: sudo dnf remove swarmgit
```

Your keys and repository copies stay in the data directory (section 15).
Deleting that directory deletes your signing keys for good.

### Build from source

You need Rust 1.87 or newer and a C compiler. From the source tree:

```bash
cargo install --locked --path crates/swarm-cli
ln -s swarm ~/.cargo/bin/git-remote-swarm   # Git needs this name
```

## 3. Quick start

### Publish a repository

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

`swarm init` creates the signing key and adds a git remote called `swarm`. It
also works inside a repository that already has history: run it there, then push
the branches and tags you want to publish.

```bash
swarm init
git push swarm --all
git push swarm --tags
```

`swarm link` prints the link to share:

```text
swarm://<repo-id>?h=tcp:192.0.2.10:42424@<node-id>
```

### Clone it elsewhere

```bash
git clone 'swarm://<repo-id>?h=tcp:192.0.2.10:42424@<node-id>' hello
cd hello
git remote rename origin swarm
```

Renaming the remote is optional. It lets `swarm link` and similar commands know
which repository you mean when you leave the argument out.

The daemon starts by itself the first time it is needed. If the clone cannot
reach the first machine, see section 10.

### Check that it is being served

```bash
swarm status
```

The repository should show `SEEDING yes` and `STATE ok`.

## 4. Links

A link has the repo ID and any number of hints:

```text
swarm://<repo-id>?h=tcp:host:42424@<node-id>&h=relay:relay.example.org:42425@<node-id>
```

| Hint | Means |
| --- | --- |
| `h=tcp:host:port` | Connect straight to this address. |
| `h=relay:host:port` | Reach the peer through this relay. Needs `@<node-id>`. |
| `h=onion:name.onion:port` | A Tor onion service. Needs `proxy`. |
| `h=https:host:port` | A static mirror (section 13). |

- **Quote the link** in a shell. It contains `&`, which would otherwise cut the
  command in two. `swarm link` adds the quotes when printing to a terminal.
- `@<node-id>` pins the machine's identity. A different machine answering at
  that address is refused. Without it, the first identity seen is remembered
  and expected from then on.
- Hints only suggest where to look. A bare `swarm://<repo-id>` can still work
  through the local network or the DHT.
- A link does not update itself. After your address or relay changes, run
  `swarm link` again and share the new one.

`swarm link` takes a repo ID, a link, or nothing. With nothing it uses the
`swarm` remote of the current directory, or the only repo the daemon has.

## 5. Daily work with git

Everything you normally do works:

```bash
git fetch swarm
git pull swarm main
git push swarm main
git push swarm my-branch v1.0          # a branch and a tag
git push swarm --delete my-branch
git push -f swarm main                 # rewrite history
```

Notes:

- **Only a key holder can push.** In a clone without the root or a maintainer
  key, a push is rejected with `no signing key`. Open a merge request instead
  (section 7), fork (section 8), or ask the owner for access (section 12).
- A push is complete when your own daemon has it. Other peers receive it if
  they are connected, or at their next sync. Keep your machine online until
  someone else has it.
- Git prints `To swarm://<node-id>` after a push. That is git shortening the
  link for display, not a different repository.
- Your working directory never changes on its own. Run `git pull` to get new
  commits.
- If `git pull` complains about divergent branches although you have no local
  commits, use `git pull --no-rebase`, or `git fetch` followed by
  `git merge --ff-only FETCH_HEAD`.
- To follow a second peer, add its link as another remote:
  `git remote add laptop '<link>'`.

New repositories use SHA-256 object names when your git supports it. Use
`swarm init --object-format sha1` for SHA-1. An existing git repository keeps
its own format.

## 6. Name, description and inspecting

### Name and description

The owner or a maintainer can name a repository:

```bash
swarm about <repo> --name "my project" --description "what it is"
swarm about <repo>                     # show them
```

The name is at most 64 bytes and the description 512 bytes, each on one line.
`--name ""` removes it. Both are signed. Other peers pick them up from a
connected peer or at their next sync. Bundles and mirrors do not carry them.

### Looking without cloning

```bash
swarm inspect '<link>'
```

This asks a peer for the signed metadata and checks it, without downloading any
git objects:

```text
name: demo
description: first words
forked from: swarm://<original> (name)
fork: swarm://<a fork on this machine>
branches:
  main (default)
  side
tags:
  v1
<repo-id> seq=3 refs=3 peers=1
```

`forked from` is what the fork's owner signed into it. `fork` lines list only
forks held on this machine, because the network does not record who forked a
repository. `--json` adds the refs with their commit IDs, `HEAD`, the object
format and peer node IDs. `--progress` also streams the lookup steps as JSON
lines.

## 7. Merge requests

A merge request asks for the commits on a branch to be merged, with a
conversation around them. Anyone who has the repository's link can open one,
comment and review. No repo key is needed. Only the owner or a maintainer can
merge.

### Propose a branch

In a clone:

```bash
git checkout -b fix
git commit -am "Fix the thing"
swarm mr create                        # into the default branch
```

Commit more and run `swarm mr create` again to add the commits to the same
request. The title and description come from your commits, or give them with
`--title` and `-m`. `--base <branch>` picks another branch to merge into, and
`swarm mr create <branch>` proposes a branch you do not have checked out.

From a fork, name the original repository:

```bash
swarm mr create -R '<link of the original>'
```

Plain git works too: `git push origin HEAD:refs/for/main` does the same, with
`-o title=...` and `-o body=...` for the text. A push updates the open request
for the same branch. `-o mr=<id>` names another one to update, and a push from
a detached `HEAD` always opens a new one.

### Review and merge

Name a merge request by the branch it proposes, or by its ID. `show`, `diff`,
`comment` and `review` use the current branch if you name none.

```bash
swarm mr list                          # open ones; --all adds merged and closed
swarm mr show fix                      # description, commits and conversation
swarm mr diff fix
swarm mr checkout fix                  # try it, on a branch mr/<id>
swarm mr comment fix -m "Why?"         # without -m your editor opens
swarm mr comment fix --file a.c --line 12 -m "Off by one."
swarm mr comment fix --reply-to <comment> -m "Fixed."
swarm mr review fix --approve          # or --request-changes, with -m
swarm mr merge fix                     # root or maintainer key
swarm mr close fix -m "Not needed"     # its author or a maintainer
swarm mr reopen fix
```

Run these inside a clone, or add `-R <link or ID>`. Each command syncs first;
`--offline` skips that. `swarm status` shows how many requests are open.

- `swarm mr merge` needs the target branch checked out, up to date and clean.
  It makes a merge commit and pushes it. `--no-push` stops before the push.
  After resolving a conflict, commit and push as usual.
- A request shows as `merged` once the target branch holds its latest commit,
  however it got there. A squash or rebase does not, so close such a request by
  hand.
- An approval counts for the commits it was given on. After the author adds
  more, it is shown as outdated.
- When two requests propose branches of the same name, yours and then the open
  one is meant. Otherwise the command lists their IDs and you give one.

### What is kept

Every action is signed and added to the history of the request. Nothing in it
can be edited or removed: peers refuse a rewritten history. The proposed
commits are kept with it. A command says how many peers have what you just
wrote; with none reachable it stays on your machine and is sent later.

### Identity and limits

Your actions are signed with the repo's root or maintainer key if you hold one,
and otherwise with a key made for this machine, `keys/identity.key` in the data
directory. The name shown is your `git config user.name`; your e-mail address
is not published. Names are free to choose, so go by the role and key shown
next to them.

- Anyone with the link can write, and there is no way to delete anything or to
  block an author. Do not put secrets in a request or a comment.
- One comment or description is at most 64 KiB, a repo holds at most 2000
  request and participant pairs (200 per participant), and one transfer is at
  most 64 MiB.
- Bundles, mirrors and forks do not carry merge requests.

## 8. Forks

```bash
swarm fork '<link of the original>'    # prints the new link
```

A fork is a new, independent repository with its own key and repo ID, made from
the current state of the original. You can push to it. The original is
unchanged. The fork records `forked_from`, and starts with the original's name
and description; later changes to those do not follow.

To work on it from an existing clone of the original:

```bash
git remote add mine '<link of your fork>'
git push mine my-branch
```

Or clone the new link into a fresh directory. To send changes back, open a
merge request against the original with `swarm mr create -R '<original link>'`.
A fork is not required for that; it is for publishing your own version.

## 9. Keeping a repository online

### Who seeds

A machine seeds a repository once it has created, cloned, forked or imported
it. `swarm inspect` does not. To hold a copy without a working directory, use:

```bash
swarm seed '<link>'                    # download and serve
swarm unseed '<link>'                  # stop serving, keep the copy
```

A seeding daemon:

- syncs every seeded repo every 5 minutes, and at once when a connected peer
  announces something newer;
- announces the repo on the local network every 60 seconds and on the DHT every
  15 minutes;
- stays running. A daemon that seeds nothing exits after 30 idle minutes.

If one peer seeds a repo from you, others can still clone it after you go
offline.

### Start at boot

The daemon is started by the first `swarm` or `git` command that needs it, not
at boot. After a restart of the computer, nothing is served until then. On a
machine that should always seed, run it as a service.

Linux, as a per-user systemd service. Save this as
`~/.config/systemd/user/swarmgit.service`:

```ini
[Unit]
Description=Swarmgit daemon
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/bin/swarm daemon --foreground
Environment=SWARM_IDLE_EXIT_MINUTES=0
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Then:

```bash
swarm daemon stop
systemctl --user daemon-reload
systemctl --user enable --now swarmgit
loginctl enable-linger "$USER"         # keep it running after logout
```

Manage that daemon with `systemctl --user`, including after upgrades:
`systemctl --user restart swarmgit`. For macOS, the source tree has
`contrib/launchd/org.swarmgit.daemon.plist` with instructions inside it.

### A dedicated seeding server

On a server with a public address:

```bash
# ~/.config/swarmgit/config.yaml
#   advertise: git.example.org
swarm seed '<link>'
swarm link <repo-id>                   # share this link too
```

Open TCP port 42424 in its firewall, and run the daemon as a service as above.
Seeding needs no signing key, so the server cannot change the repository.

## 10. Making your machine reachable

For someone to clone **from** you, their daemon has to connect to yours. A
clone only needs one side to be reachable.

| Your situation | What to do |
| --- | --- |
| Same local network | Nothing. Open TCP 42424 if you run a host firewall. |
| Public IP address | Open TCP 42424. Set `advertise` if the link shows the wrong address. |
| Home router with UPnP | Usually nothing: the daemon asks the router to forward the port. |
| Router without UPnP | Forward TCP 42424 yourself and set `advertise` to your public address. |
| Mobile data, CGNAT, strict firewall | Use a relay. |

### Relays

A relay passes encrypted bytes between two peers that cannot connect directly.
It cannot read or change the repository, but it does see both IP addresses and
how much is sent.

With `relay_discovery` on, which is the default, a machine that cannot be
reached also looks for public relays through the DHT and registers with them.
Nothing needs to be configured, but it depends on public relays existing. To
use a relay you choose:

```yaml
# ~/.config/swarmgit/config.yaml
relays:
  - relay.example.org:42425
```

```bash
swarm daemon restart
swarm status relays                    # wait for "registered"
swarm link                             # share the new link
```

The link now has an `h=relay:` hint. The person cloning needs no relay setting.
At most two configured relays are used. A private relay also needs its key in
the address and a password:

```yaml
relays:
  - relay.example.org:42425#<key>
relay_password: <secret>
```

`relay_mode` is `auto` (register unless peers have recently reached you
directly), `always`, or `off`. Running a relay is covered in the
[relay guide](RELAY-GUIDE.md).

`swarm discover-relays` lists the public relays that can be found now.

## 11. How peers find each other

A sync tries every route at once and dials up to 8 addresses at a time until 4
connections carry the repo.

| Route | Source | Notes |
| --- | --- | --- |
| Hints | The link | Pinned when `@node` is present. |
| Known peers | Remembered from earlier syncs | Best first. |
| Local network | UDP multicast on port 42099 | Same network segment only. |
| DHT | BitTorrent Mainline DHT | Works with no hint; needs outbound UDP. |
| Peer exchange | Connected peers | One good hint is enough to learn the others. |
| Relay | `h=relay:` hints and discovered relays | For peers that cannot accept connections. |
| Mirror | `h=https:` hints and `mirrors` | Used only when no peer answers. |

- Missing objects are split across the peers that have them. If one fails, its
  share moves to the others, and a later sync resumes where it stopped.
- A client never goes back to an older seq than it already has, even if the
  only reachable peer is behind. The DHT holds a signed pointer to the newest
  state, so a sync keeps looking when every peer found is behind it.

## 12. Keys and shared access

Keys live in `keys/` in the data directory, readable only by you. Nobody can
recover them for you.

### Back up the root key

```bash
swarm key backup <repo> -o myrepo.key
```

Keep the file somewhere safe and private. To restore it on a machine that
already has the repository (clone or seed it first):

```bash
install -m 600 myrepo.key ~/.local/share/swarmgit/keys/<repo-id>.key
```

On macOS the directory is `~/Library/Application Support/swarmgit/keys/`.

### Add a maintainer

A maintainer publishes with a key of their own, so the root key is never
shared.

```bash
# the maintainer, after cloning
swarm key new <repo>                   # prints a key ID; send it to the owner

# the owner
swarm maintainers add <repo> <key-id> 'refs/heads/*' 'refs/tags/*'
swarm maintainers list <repo>
swarm maintainers remove <repo> <key-id>
```

The globs limit what the key may publish. Without any, it is `refs/heads/*`, so
add `refs/tags/*` if the maintainer should push tags. A push outside the globs
is rejected (`out_of_scope`). `swarm key id <repo>` prints the key ID again.

### Conflicts

If two key holders publish different values for one branch without seeing each
other's change, `swarm status` shows `conflict` and names the ref. Settle it on
your machine by taking one signer's value:

```bash
swarm accept <repo> refs/heads/main <signer-key-id>
```

Do not publish with the **same** key from two machines. Two different states
signed by one key with one seq is reported as `equivocation`, which peers treat
as a sign of a stolen key. Give each person or machine its own maintainer key.

### Recovery key and rotation

A recovery key can move the repository to a new root key if the root key is
lost or stolen. Create it **before** you need it.

```bash
swarm key recovery <repo> -o recovery.key        # on the root; keep it offline
swarm key rotate <repo> --recovery recovery.key  # later, on a machine with the file
```

Rotation installs a new root key on the machine that runs it. States signed by
the old root are rejected afterwards. The repo ID and links stay the same.

### Moving to a new computer

Stop the daemon, copy the whole data directory to the same place on the new
computer, and do not run the old one again. That keeps your keys, repositories
and node ID.

## 13. Offline copies

### Bundles

A bundle is one file with a repository and its signed state, for moving by USB
stick or similar.

```bash
swarm export <repo> -o repo.swarmbundle
swarm import repo.swarmbundle          # prints the link
git clone 'swarm://<repo-id>'
```

Nothing in the file is trusted: every signature is checked on import.

### Static mirrors

A mirror is any web host. It only stores bytes; everything is verified by the
client, so it cannot tamper with the contents.

```bash
swarm mirror export <repo> ./www       # writes ./www/<repo-id>/repo.swarmbundle
```

Serve `./www` over HTTPS, then share
`swarm://<repo-id>?h=https:mirror.example.org:443`, or set `mirrors` in the
settings. Export again after each publish. Mirrors are tried only when no peer
can be reached.

Neither bundles nor mirrors carry names, descriptions or merge requests.

## 14. Reading `swarm status`

```text
Daemon    running (pid 207, version 0.2.0)
Listen    0.0.0.0:42424
Node      <node-id>

Repositories (1)
  REPO       SEQ  REFS  SEEDING  SEEDERS  CHECKED  STATE
  <repo-id>  3    2     yes      2        0s ago   ok
    ↳ ~/code/hello

Relays (0)
  none in use
```

| Column | Meaning |
| --- | --- |
| SEQ | Version of the repo's signed state here. |
| REFS | Branches and tags. |
| SEEDING | This machine has the repo and serves it. |
| SEEDERS | This machine, if seeding, plus other nodes that had it at the last check. `checking` or `?` means no answer yet. |
| CHECKED | Age of that count. |
| STATE | `ok`, `conflict`, `equivocation`, `folder deleted` or `files missing`. |

A `↳` line is a project folder on this machine that uses the repo.

```bash
swarm status <repo>                    # link, root key, folders, open merge requests
swarm status repos                     # only the repository table
swarm status relays                    # relay registrations
swarm status peers                     # known addresses per repo
swarm status --json                    # for scripts
```

In `swarm status peers`, `connected` has an open connection, `failing (n)`
failed the last `n` tries, and `idle` is neither.

`folder deleted` means every folder that used the repo is gone; it is still
seeded. `files missing` means the daemon's own copy is gone: it stops seeding
until you run `swarm seed` again.

## 15. Storage and cleanup

The data directory is `~/.local/share/swarmgit` on Linux and
`~/Library/Application Support/swarmgit` on macOS. `home` or `SWARM_HOME`
changes it.

| Path | Contents |
| --- | --- |
| `keys/` | Your signing keys. Back these up. |
| `node.key` | This machine's identity. |
| `repos/<repo-id>/` | The daemon's copy: git store, signed states, name, merge requests. |
| `swarm.db` | Repository list, known peers, settings the daemon learned. |
| `daemon.log` | The daemon's log. |
| `daemon.json` | Port and process ID of the running daemon. |

The daemon keeps its own copy of every repository, so deleting a working folder
does not remove it.

```bash
swarm unseed <link>                    # stop serving, keep the copy
swarm gc <repo>                        # repack to save space
swarm purge <repo>                     # delete the copy from this machine
```

`swarm purge` asks before deleting. If you hold the signing key, it asks
separately about that and keeps it by default. `--yes` skips the questions and
keeps the key unless you add `--delete-key`. A deleted key cannot be recreated.
Other peers keep their copies: there is no way to delete a repository from the
network.

`swarm gc` keeps every object reachable from a signed state. It fails with
`busy` while a sync or upload of that repo is running.

## 16. Settings

Settings come from, in order of priority:

1. `SWARM_*` environment variables;
2. `~/.config/swarmgit/config.yaml` (your own);
3. `/etc/swarmgit/config.yaml` (the whole machine).

The two files are merged setting by setting. If `SWARM_CONFIG` is set, only
that file is read, and it must exist. Unknown keys are reported and ignored.

```yaml
listen: 0.0.0.0:42424
advertise: git.example.org
relays:
  - relay.example.org:42425
lan: true
dht: true
upnp: true
```

The daemon reads settings when it starts. After changing them run
`swarm daemon restart`. A daemon also inherits the environment of the shell
that started it, so prefer the files over environment variables.

| YAML key | Environment variable | Default and meaning |
| --- | --- | --- |
| `home` | `SWARM_HOME` | Data directory. |
| `listen` | `SWARM_LISTEN` | `0.0.0.0:42424`. If busy, another port is chosen. |
| `advertise` | `SWARM_ADVERTISE` | Host name or IP put in `swarm link`. |
| `relays` | `SWARM_RELAYS` | Relays to register with and dial through. |
| `relay_mode` | `SWARM_RELAY_MODE` | `auto`, `always` or `off`. |
| `relay_delay_ms` | `SWARM_RELAY_DELAY_MS` | `1500`. Head start given to a direct address. |
| `relay_discovery` | `SWARM_RELAY_DISCOVERY` | `on`. Find public relays. |
| `relay_password` | `SWARM_RELAY_PASSWORD` | Secret for a private relay. |
| `relay_max_bytes` | `SWARM_RELAY_MAX_BYTES` | 2 GiB per relayed connection. |
| `lan` | `SWARM_NO_LAN=1` disables | `true`. Local network discovery. |
| `lan_port` | `SWARM_LAN_PORT` | UDP `42099`. |
| `dht` | `SWARM_NO_DHT=1` disables | `true`. Mainline DHT discovery. |
| `dht_bootstrap` | `SWARM_DHT_BOOTSTRAP` | Replaces the built-in bootstrap nodes. |
| `dht_bind` | `SWARM_DHT_BIND` | IP address for the DHT socket. |
| `dht_port` | `SWARM_DHT_PORT` | Any free UDP port. |
| `upnp` | `SWARM_NO_UPNP=1` disables | `true`. Ask the router to forward the port. |
| `find_timeout` | `SWARM_FIND_TIMEOUT` | `30` seconds a first clone waits for the local network. |
| `max_pack_bytes` | `SWARM_MAX_PACK_BYTES` | 2 GiB. Largest pack served or accepted. |
| `idle_exit_minutes` | `SWARM_IDLE_EXIT_MINUTES` | `30`, when nothing is seeded. `0` never exits. |
| `log` | `SWARM_LOG` | `info`. Log filter, such as `debug`. |
| `mirrors` | `SWARM_MIRRORS` | Static mirror base URLs. |
| `upload_kbps` | `SWARM_UPLOAD_KBPS` | `0`, unlimited. Upload cap in kilobits per second. |
| `proxy` | `SWARM_PROXY` | SOCKS5 proxy for every outbound connection. |
| `onion_control` | `SWARM_ONION_CONTROL` | Tor control port; hosts an onion service. |
| `onion_password` | `SWARM_ONION_PASSWORD` | Tor control password. |
| `onion_max_connections` | `SWARM_ONION_MAX_CONNECTIONS` | `16` onion visitors at once. |
| `onion_max_per_minute` | `SWARM_ONION_MAX_PER_MINUTE` | `120` new onion connections per minute. |
| `obfs` | `SWARM_OBFS` | `false`. Vary handshake sizes and timing. |
| `udp` | `SWARM_NO_UDP=1` | Enabled by default. Peers try a direct UDP connection after meeting through a relay. Set `udp: false` to disable it. |

## 17. Privacy and hardening

- **Tor.** `proxy` sends every connection through SOCKS5, and `onion_control`
  makes you reachable as an onion service. With a proxy, the local network,
  DHT, UPnP and UDP paths are off. See the [Tor guide](TOR-GUIDE.md).
- **Less exposure.** `dht: false` and `lan: false` stop announcing your
  repositories. Peers then need a hint in the link.
- **Obfuscation.** `obfs: true` pads and delays the handshake and varies frame
  sizes. It does not disguise Swarmgit as another protocol.
- **Inbound limits.** The daemon accepts 64 connections at once, 4 from one
  address and 60 new ones a minute from one address. An address that breaks the
  protocol 3 times is ignored for 10 minutes.
- **UDP hole punching.** By default, peers try a direct UDP connection after
  meeting through a relay. If it fails, or the direct path drops during a
  transfer, they continue over the relay. This reveals each peer's IP address
  to the other; set `udp: false` on either peer to prevent it. UDP works only
  over IPv4 and often fails behind symmetric NAT. Like TCP, it slows down when
  packets are lost. Swarmgit disables it automatically when a proxy is
  configured.

## 18. Command reference

```text
swarm init [--object-format sha1|sha256] [--json]
swarm link [--json] [repo]
swarm about <repo> [--name TEXT] [--description TEXT] [--json]
swarm inspect <link> [--json] [--progress]
swarm fork <link> [--json]
swarm seed <link> [--json]
swarm unseed <link> [--json]
swarm status [--json] [repo]
swarm status repos [repo] | relays | peers [repo]
swarm mr create [branch] [--base B] [--title T] [-m TEXT]
swarm mr list [--all] | show [mr] | diff [mr] | checkout <mr>
swarm mr comment [mr] [-m TEXT] [--file P [--line N]] [--reply-to C]
swarm mr review [mr] --approve | --request-changes [-m TEXT]
swarm mr merge <mr> [--no-push] | close <mr> [-m TEXT] | reopen <mr>
swarm maintainers add <repo> <key-id> [glob...] | remove <repo> <key-id> | list <repo>
swarm accept <repo> <ref> <signer>
swarm key new <repo> | id <repo> | backup <repo> -o FILE
swarm key recovery <repo> -o FILE | rotate <repo> --recovery FILE
swarm export <repo> -o FILE [--json]
swarm import <file> [--json]
swarm mirror export <repo> <dir>
swarm discover-relays [--json]
swarm gc <repo> [--json]
swarm purge <repo> [--yes] [--delete-key] [--json]
swarm daemon start | stop | restart
swarm daemon --foreground
swarm completions bash|zsh|fish|elvish|powershell
swarm relay [options]
```

`swarm mr` commands also take `-R <repo>` and `--offline`. `<repo>` is a repo
ID, in base32 or hex, or a full link. Each command has a manual page, such as
`man swarm-mr-create`, and `--help`.

Exit codes of `swarm`, for scripts:

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | General error. |
| 2 | Wrong usage or a bad link. |
| 3 | No peer could be reached. |
| 4 | No signing key. |
| 5 | The daemon could not be reached. |
| 6 | Authentication or verification failed. |

## 19. Troubleshooting

Start with `swarm status` and the daemon log, `daemon.log` in the data
directory. For more detail set `log: debug`, restart the daemon, and try again.

### Cannot connect

**`no_peers`, or the clone hangs.** Nobody who has the repo could be reached.
Check that the other machine's daemon is running (`swarm status` there), that
the address in the link is right, and that TCP 42424 is open. From mobile data
or two home networks, neither side may be able to accept connections: use a
relay (section 10).

**`relay is full`.** The relay has reached a server-wide limit and cannot take
another registration, session, or waiting request. Try again later or use
another relay. To run your own, use `swarm relay` on a machine that accepts
incoming connections; see the [relay guide](RELAY-GUIDE.md). A machine that
cannot register shows the same message in `swarm status relays`.

**`no_peers` mentions a relay.** The relay was tried, but the seeder is not
registered with it or is offline. Run `swarm status relays` on the seeder and
share a fresh `swarm link`.

**A link without hints finds nothing.** Off the local network this relies on
the DHT, which needs outbound UDP and a seeder that announced in the last 15
minutes. On the local network it needs UDP 42099, and some Wi-Fi networks
block devices from seeing each other. Add a hint.

**`auth_failed`.** The `@<node-id>` in the link does not match the machine that
answered. Check you copied the current link. Remove the `@` part only if you
know why the identity changed.

**`no_shared_repo`.** The machine you reached does not have that repository.

### Publishing and syncing

**`no signing key`.** This machine may not publish. Run `swarm mr create` to
propose the change, `swarm fork` to publish your own version, or ask to be
added as a maintainer.

**`out_of_scope`.** Your maintainer key may not publish that ref. Ask the owner
to add it with a wider glob.

**A push did not reach the other peer.** Check both daemons are running. A
seeder catches up at its next 5-minute sync. To get it immediately, fetch the
pushing machine's link.

**A clone ends on an older commit.** Every peer reached was behind. Add a hint
for an up-to-date one.

**A merge request or comment has not arrived.** The command says how many peers
have it. With 0, it is sent at the next sync with a peer.

**`matches several merge requests`.** Two propose branches of the same name.
Give an ID from the list instead.

### Daemon

**A fix or new setting has no effect.** The old daemon is still running. Run
`swarm daemon restart`, or restart the service.

**The daemon cannot be reached (exit code 5).** Read `daemon.log`. Check that
the data directory is yours and writable, and that another program is not using
it through `SWARM_HOME`.

**Nothing is served after a reboot.** The daemon is not started at boot. See
section 9.

**`busy` from `swarm gc`.** A sync or upload is running. Try again shortly.

**`seeding failed: files missing`.** The daemon's copy was deleted. Run
`swarm seed <link>` to download it again.

## 20. Security notes

- Connections are Noise XX plus ML-KEM-768, then ChaCha20-Poly1305, with no
  classical-only fallback. Both sides prove their node identity with a hybrid
  signature.
- A pinned peer (`@<node-id>`) cannot be impersonated. An unpinned first
  contact could be intercepted by an active attacker, who still cannot forge
  content: every state and object is verified before it is stored.
- Peer exchange, the DHT, relays and mirrors only suggest addresses or move
  bytes. They are not trusted.
- The repo ID never appears in a handshake or in DHT data. Anyone who has it
  can find and clone the repo, so a private link is a secret.
- Content that was published cannot be recalled. Removing a secret from history
  with a force push does not remove copies that peers already have. Revoke the
  secret instead.
- Anyone with the link can add merge requests and comments, which nobody can
  delete (section 7).

## Appendix: Testing and development

```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
SWARM_NO_DHT=1 SWARM_NO_UPNP=1 cargo test --locked --workspace

# End-to-end scripts; build the workspace first.
tests/e2e/m1.sh

# Against the public Mainline DHT (needs internet access).
SWARM_LIVE_DHT=1 cargo test -p swarm-disc live_dht -- --ignored
```

The network tests need Linux and root. `.github/workflows/ci.yml` lists the
tools to install and how the full run, `scripts/ci.sh`, is started.

After changing the command line, run `scripts/gen-completions.sh` to update
completions and manual pages.
