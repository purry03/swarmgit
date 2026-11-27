# Swarmgit relay operator guide

This guide covers running a relay server, configuring Swarmgit clients to use
it, and operating it safely. A relay is optional: direct peer connections,
LAN, global lookup, peer exchange, and offline bundles continue to work
without one.

## 1. What a relay does

A relay makes a peer reachable when its NAT, carrier-grade NAT, mobile network,
or firewall prevents inbound connections. The seeder keeps an outbound
control connection to the relay. When a cloner asks for that seeder's node ID,
the relay notifies it; the seeder opens a second outbound connection, and the
relay joins that connection to the cloner's outbound connection.

```text
Seeder ── control connection ──▶ Relay ◀── outbound dial ── Cloner
Seeder ◀── accept connection ─── Relay ══ byte stream ══ Cloner
```

The relay is a transport, not a Git server. The regular authenticated,
encrypted Swarmgit session runs between the peers through the byte stream. The
relay does not store repository data.

Swarmgit ships no default relay. Clients use the relays you configure, the
relay hints in links, and (unless `relay_discovery` is `off`) public relays
they find through the DHT. Run your own if you do not want to depend on
strangers.

## 2. Requirements and network setup

- Run the relay on a host with a stable, publicly reachable IP address. If the
  host itself is behind NAT, forward its relay port.
- Point a stable DNS name (or public IP) at the host.
- Allow inbound **TCP** on port `42425` in the host firewall and any cloud
  firewall/security group. Allow **UDP** on the same port too if peers will
  try experimental hole punching: the relay answers their address probes
  there. Without it relayed connections still work.
- Use the same public `host:port` in the server's `--name`, the seeder's
  `relays` setting, and the relay hints clients receive.

The relay name matters: clients dial it, and the seeder's registration proof
is bound to the exact name it used. If you change the public host or port,
update the server settings and client configuration, then restart the
affected services and share a fresh link.

## 3. Install the relay

### Debian or Ubuntu, x86-64

Download `swarm-relay_0.1.0_amd64.deb` from the Relay section of the Swarmgit
home page, then install it:

```bash
sudo apt install ./swarm-relay_0.1.0_amd64.deb
sudoedit /etc/default/swarm-relay
```

The package installs `/usr/bin/swarm-relay`, the systemd unit, and
`/etc/default/swarm-relay`. It creates the unprivileged `swarm-relay` service
account and enables and starts the service. Set the public `--name` before
relying on the relay. The package's default arguments listen on
`0.0.0.0:42425` but leave the name unset.

To upgrade, install the newer package. To keep your edited
`/etc/default/swarm-relay`, answer "keep" if asked about it, or install with
`sudo dpkg --force-confold -i swarm-relay_*.deb`. An existing file keeps its
old arguments, so it does not pick up new package defaults such as
`--no-network`.

### Build from source

The workspace pins a Rust toolchain. Build in the development container if
your host toolchain cannot read the project toolchain:

```bash
docker compose run --rm dev cargo build --release -p swarm-relay
```

The binary is `target/release/swarm-relay`. For a manual test, run:

```bash
./target/release/swarm-relay \
  --listen 0.0.0.0:42425 \
  --name relay.example.org:42425
```

Stop a manually run relay with Ctrl-C. For a persistent deployment, use the
packaged systemd service or create an equivalent service for your platform.

## 4. Configure and operate the server

For the Debian/Ubuntu package, edit `/etc/default/swarm-relay`:

```bash
SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --name relay.example.org:42425 --key-file /var/lib/swarm-relay/relay.key"
```

`--listen` is the local bind address. `--name` is the public `host:port`
clients will dial. Repeat `--name` to support multiple public names. Avoid
leaving `--name` unset on a public relay: without it, registration proofs are
not bound to a configured public name.

Restart and inspect the service:

```bash
sudo systemctl restart swarm-relay
sudo systemctl status swarm-relay
sudo journalctl -u swarm-relay -f
sudo ss -ltnp | grep 42425
```

The packaged service runs as the `swarm-relay` user, restarts after failure,
and sets `LimitNOFILE=65536`.

### Server options

`swarm relay` takes the same options as `swarm-relay`.

```text
--listen ADDRESS          Bind address (default 0.0.0.0:42425)
--name HOST:PORT          Public client-facing name; repeatable
--key-file FILE           Keep the relay key, so host:port#key addresses last
--password-file FILE      Require a shared secret (needs --key-file)
--allow-node NODE_ID      Let only this node register; repeatable
--metrics ADDR            Counters over HTTP (off by default)
--max-listeners N         Registered peers (default 10000)
--listeners-per-ip N      Registered peers per address (default 2)
--max-splices N           Simultaneous relayed sessions (default 500)
--splices-per-ip N        Open sessions per dialing address (default 8)
--connects-per-minute N   Connect attempts per address (default 20)
--splice-kib-per-sec N    Rate of one session (default 8192, 0 for none)
--node-daily-mib N        Bytes carried per target node per day (default 10240)
--idle-secs N             Close an idle session (default 120)
--no-network              No DHT announce and no gossip
--public                  Announce without first being dialed back
--allow-private           Accept private relay addresses (closed networks)
--round-secs N            Seconds between gossip rounds (default 1800)
```

The pending-request limit is 1,000, and a pending request expires after
10 seconds. A silent listener control connection is closed after 75 seconds.
Per-address limits do not apply to loopback; IPv6 addresses are counted by
/64.

### The relay network

By default a relay joins the public relay network: it announces itself on the
Mainline DHT once another relay has dialed it back, and trades relay addresses
with the others. The first relay of an empty network needs `--public`. Use
`--no-network` to stay out. A relay with a password or `--allow-node` never
joins. The Debian and RPM packages default to `--no-network`.

If a `--name` is a domain, clients that find the relay check that it resolves to
the address they dialed, then show it and put it into share links instead of the
IP. Links then survive a change of address.

To make your relay one that clients find without listing it, remove
`--no-network`, add `--public` if no other relay can dial it back, and
restart. With debug logging on (see Logs below), `relay directory announce
done=true` means the announcement was stored. From another machine,
`swarm discover-relays` should then list it. Before you do this:

- Anyone can then use the relay, within the limits in section 7.
- Unrelated hosts will connect to the port. Expect debug lines such as
  `relay protocol: bad frame length`, most likely BitTorrent clients and
  crawlers that saw the DHT announcement. The relay drops them.
- A client with several relays to choose from may pick another one. List
  yours in `relays` where it matters.

### Logs

```bash
sudo journalctl -u swarm-relay -f
sudo journalctl -u swarm-relay | grep -E 'WARN|ERROR'
```

The default level is `info`, which logs no line per registration or session.
Check registration from the client with `swarm status relays`. For connection errors
and relay network rounds, turn on debug logging:

```bash
sudo systemctl edit swarm-relay
# add these two lines, then save:
#   [Service]
#   Environment=RUST_LOG=info,swarm_relay=debug
sudo systemctl restart swarm-relay
```

Remove it afterwards with `sudo systemctl revert swarm-relay` and another
restart. `revert` also removes any other overrides you made to the unit.

| Log line | Meaning |
| --- | --- |
| `Our current id ... is not valid for address ... Using new id ...` | Normal, once per start. The DHT node picks an ID that matches the public IP (BEP 42). |
| `mainline::rpc: Could not bootstrap the routing table` | Seen at start on relays and clients where announcing and discovery still worked. Check for `announce done=true` before treating it as a fault. |
| `relay network round relays=0 reachable=true` | The relay is announcing itself and knows no other relay. |
| `relay io: early eof` | The other end closed the connection while the relay was reading. Expected when a client daemon stops. |
| `relay protocol: bad frame length` | The other end is not speaking the relay protocol. |
| `relay auth: noise: decrypt error` | A client's encrypted handshake failed. If it keeps happening, check any `#<key>` in client addresses. Without `--key-file` the key changes at every restart. |

### A private relay

```bash
printf '%s\n' 'a long secret' > relay.password
swarm-relay --listen 0.0.0.0:42425 --name relay.example.org:42425 \
  --key-file relay.key --password-file relay.password
```

The log prints `relay address: relay.example.org:42425#<key>`. Give that whole
address and the password to your clients:

```yaml
relays:
  - relay.example.org:42425#<key>
relay_password: a long secret
```

A client sends the password only to an address that includes the key, so a
machine that merely answers on your name never receives it. Without `#<key>`
the client gets `auth_failed` and is told to add it. Links never carry the
key: each machine that dials a private relay needs it in its own settings.
Keep `relay.key`; a new key changes every client's address.

### Metrics

`--metrics 127.0.0.1:9100` serves `swarm_relay_listeners`,
`swarm_relay_splices`, `swarm_relay_splices_total`,
`swarm_relay_bytes_total`, `swarm_relay_auth_failures_total` and
`swarm_relay_errors_total`. It has no authentication, so keep it local.

### A relay as an onion service

Tor can publish a relay; the relay itself needs no change. In `torrc`:

```text
HiddenServiceDir /var/lib/tor/swarm-relay/
HiddenServicePort 42425 127.0.0.1:42425
```

Run it with `--listen 127.0.0.1:42425 --name <name>.onion:42425
--no-network`. Clients need `proxy` set; an onion relay is never dialed
without one. See [the Tor guide](TOR-GUIDE.md).

## 5. Register a seeder

Configure the peer that must be reachable—usually the seeder—with the relay
address. Use `/etc/swarmgit/config.yaml` for a system-wide setting, or
`~/.config/swarmgit/config.yaml` for one user. Both are read, setting by
setting, and the user file wins:

```yaml
relays:
  - relay.example.org:42425
```

`SWARM_RELAYS=relay.example.org:42425` is the environment-variable equivalent.
Environment variables override YAML settings. A node can use at most two
configured relays.

The daemon reads settings when it starts, so restart it:

```bash
swarm daemon restart
swarm seed 'swarm://<repo-id>'
swarm status relays
swarm link
```

Wait for `swarm status relays` to show the relay as `registered`.
`not registered` has the reason under `DETAIL`. `none in use` means the daemon is using no relay: none is
configured or discovered, or `relay_mode` does not call for one. Share the updated
link from `swarm link`; it should include a pinned hint like:

```text
h=relay:relay.example.org:42425@<node-id>
```

Use the full link printed by Swarmgit, which includes the repository ID and
any direct or additional relay hints. An old link will not gain new relay
hints by itself.

`relay_mode` defaults to `auto`: register unless peers have recently reached
this node directly. `always` registers regardless. `off` never uses a relay,
as seeder or cloner. With discovery on, a node also registers with public
relays it found (at most 2 found, 3 in all); `relay_discovery: off` limits it
to configured ones. Discovery is off behind a proxy.

A node registers with a discovered relay only while it serves at least one
repository. To test discovery, use a fresh `SWARM_HOME` (the daemon remembers
relays it has seen), remove the `relays` setting from every config file that
defines it, make sure `relay_discovery` is on, and seed a repository there.
Both `/etc/swarmgit/config.yaml` and `~/.config/swarmgit/config.yaml` are read;
the user file wins for settings present in both. Swarmgit copies file settings
into the daemon's environment, so seeing `SWARM_RELAYS` there does not mean it
came from your shell. If it came from YAML, there is no shell variable to
unset.

## 6. How cloners use the relay

A cloner only needs outbound TCP access to a relay in the seeder's link. It
does **not** need to register with that relay or configure its own relay just
to clone. Configure a relay on the cloner only if that machine also needs to
be reachable by other peers.

For example:

```bash
git clone \
  'swarm://<repo-id>?h=relay:relay.example.org:42425@<node-id>' \
  project
```

When direct and relay addresses are both available, Swarmgit tries the direct
path first and gives it about 1.5 seconds to succeed. It tries the relay
immediately if direct dialing fails. `SWARM_RELAY_DELAY_MS` changes that
delay.

Relays do not forward connections to one another, so every transfer crosses
at most one. A cloner that has only the node ID can find the relay that node
chose, and a cloner that has only the repo ID can ask relays which nodes serve
it (they see a hash of the repo ID, never the ID).

A relayed connection can be replaced by a direct UDP path when both peers set
`experimental_udp`. The relayed session is kept until the sync ends: if the
UDP path fails, the download carries on over the relay. It does not work
through every NAT, is IPv4 only and is never used behind a proxy.

`relay_max_bytes` (default 2 GiB) limits what one client session moves through
a relay.

## 7. Capacity and resource use

Each registered peer maintains one long-lived TCP control connection per
relay. That consumes a file descriptor, kernel socket state, and some process
memory even when no clone is running. Ten thousand registered seeders do not
mean ten thousand active transfers.

Each active splice adds two TCP data connections and copies bytes in both
directions. Network throughput and egress costs are usually the main cost
during large clones; CPU and memory use increase with concurrent splices. The
relay does not cache or retain the Git objects.

Default limits in the current server:

| Resource | Default |
| --- | ---: |
| Registered listeners | 10,000 |
| Simultaneous relayed sessions | 500 |
| Pending connection requests | 1,000 |
| Pending request lifetime | 10 seconds |
| Rate of one session | 8 MiB/s |
| Bytes per target node per day | 10 GiB |
| Idle splice timeout | 120 seconds |
| Listener control silence timeout | 75 seconds |

The options above adjust these limits. Raising limits does not add bandwidth, memory, or file descriptors.
Monitor CPU, memory, file descriptors, network throughput, and egress under
realistic load before increasing them. Daily quotas are kept in memory and
start again when the relay restarts.

## 8. Privacy and security

- The relay sees the connecting peers' IP addresses and node IDs, plus
  connection timing and byte volume.
- The normal peer handshake and repository traffic are end-to-end encrypted.
  The relay cannot read repository IDs, file contents, or signed manifests.
- The node ID in the relay hint pins the expected peer. A malicious relay can
  block or drop a connection, but cannot impersonate the pinned peer without
  failing authentication.
- The control channel is encrypted to the relay's key (Noise NK) when the
  client knows it. A client that learned the key from the relay itself is
  protected only against passive observers; put `#<key>` in the address to
  rule out an impostor.
- The relay keeps live registrations and pending tokens in memory. It does
  not store repository data. The only thing it writes is its key file. Diagnostic logs can include remote IP addresses,
  so treat and retain logs accordingly.
- A relay is not a general-purpose proxy: it only pairs a dial request for a
  registered node with the target's one-time accept connection. A public relay
  can still deny service to its users.

## 9. Troubleshooting

| Symptom | Checks |
| --- | --- |
| Seeder never reports `registered` | Check outbound TCP from the seeder, DNS resolution, exact `--name` match, and `journalctl -u swarm-relay`. |
| A relay you did not list is never found | The relay must be announcing (no `--no-network`, password or `--allow-node`; `--public` if it is alone). The client must serve a repository, have `relay_discovery` on and use no proxy. In `auto` mode a directly reachable node registers nowhere. |
| Clone reports `no_such_node` | The seeder is offline, not registered at the relay in the link, or the link is stale. Restart the seeder daemon and share a fresh `swarm link`. |
| Clone times out | Check inbound TCP 42425 in host/cloud firewalls and confirm the cloner can resolve and reach the relay name in the hint. |
| Relay reports `busy` or `limit_exceeded` | Check listener/splice ceilings, file descriptors, memory, CPU, and bandwidth before raising limits. |
| Seeder reports `limit_exceeded: too many listeners` | More than 2 machines or daemons behind one public address are registering. Raise `--listeners-per-ip`. |
| Relay hint is ignored | Check that the cloner is not using `relay_mode: off` and that the hint includes `@<node-id>`. |

To stop the packaged relay:

```bash
sudo systemctl stop swarm-relay
```

After changing a relay address, restart the relay and seeder daemon, confirm
registration with `swarm status`, and share a newly generated link.
