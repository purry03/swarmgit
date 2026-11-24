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

Swarmgit does not ship or automatically discover a default public relay. Run
your own or select a relay whose operator you trust.

## 2. Requirements and network setup

- Run the relay on a host with a stable, publicly reachable IP address. If the
  host itself is behind NAT, forward its relay port.
- Point a stable DNS name (or public IP) at the host.
- Allow inbound **TCP** on port `42425` in the host firewall and any cloud
  firewall/security group. The relay server does not use UDP.
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
SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --name relay.example.org:42425"
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

```text
--listen ADDRESS       Bind address (default 0.0.0.0:42425)
--name HOST:PORT       Public client-facing name; repeatable
--max-listeners N      Maximum registered peers (default 10000)
--max-splices N        Maximum simultaneous relayed sessions (default 500)
--idle-secs N          Close an idle splice after N seconds (default 120)
```

The pending-request limit is 1,000, and a pending request expires after
10 seconds. A silent listener control connection is closed after 75 seconds;
clients normally keep it alive. These are server defaults, not throughput
guarantees.

## 5. Register a seeder

Configure the peer that must be reachable—usually the seeder—with the relay
address. Use `/etc/swarmgit/config.yaml` for a system-wide setting, or
`~/.config/swarmgit/config.yaml` for one user:

```yaml
relays:
  - relay.example.org:42425
```

`SWARM_RELAYS=relay.example.org:42425` is the environment-variable equivalent.
Environment variables override YAML settings. A node can use at most two
configured relays.

The daemon reads settings when it starts. If it is already running, stop it
before starting the next command:

```bash
swarm daemon stop
swarm seed 'swarm://<repo-id>'
swarm status
swarm link
```

Wait for `swarm status` to report `relay <name> registered`. Share the updated
link from `swarm link`; it should include a pinned hint like:

```text
h=relay:relay.example.org:42425@<node-id>
```

Use the full link printed by Swarmgit, which includes the repository ID and
any direct or additional relay hints. An old link will not gain new relay
hints by itself.

`relay_mode` defaults to `auto`. In the current implementation, `auto` and
`always` both register with configured relays; `off` disables relay
registration and ignores relay hints. `SWARM_RELAY_MODE` overrides the YAML
`relay_mode` value.

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

Relays are independent; they do not forward connections to one another. If a
seeder registers with two relays, the generated link can advertise both. The
cloner must be able to reach at least one relay named in that link. Relay
addresses can also be passed between connected peers through peer exchange;
the DHT does not itself carry relay traffic or automatically discover a relay
for a peer.

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
| Idle splice timeout | 120 seconds |
| Listener control silence timeout | 75 seconds |

`--max-listeners`, `--max-splices`, and `--idle-secs` adjust the corresponding
limits. Raising limits does not add bandwidth, memory, or file descriptors.
Monitor CPU, memory, file descriptors, network throughput, and egress under
realistic load before increasing them. The relay currently has no per-session
bandwidth quota or built-in metrics endpoint.

## 8. Privacy and security

- The relay sees the connecting peers' IP addresses and node IDs, plus
  connection timing and byte volume.
- The normal peer handshake and repository traffic are end-to-end encrypted.
  The relay cannot read repository IDs, file contents, or signed manifests.
- The node ID in the relay hint pins the expected peer. A malicious relay can
  block or drop a connection, but cannot impersonate the pinned peer without
  failing authentication.
- The relay keeps live registrations and pending tokens in memory. It does
  not store repository data. Diagnostic logs can include remote IP addresses,
  so treat and retain logs accordingly.
- A relay is not a general-purpose proxy: it only pairs a dial request for a
  registered node with the target's one-time accept connection. A public relay
  can still deny service to its users.

## 9. Troubleshooting

| Symptom | Checks |
| --- | --- |
| Seeder never reports `registered` | Check outbound TCP from the seeder, DNS resolution, exact `--name` match, and `journalctl -u swarm-relay`. |
| Clone reports `no_such_node` | The seeder is offline, not registered at the relay in the link, or the link is stale. Restart the seeder daemon and share a fresh `swarm link`. |
| Clone times out | Check inbound TCP 42425 in host/cloud firewalls and confirm the cloner can resolve and reach the relay name in the hint. |
| Relay reports `busy` or `limit_exceeded` | Check listener/splice ceilings, file descriptors, memory, CPU, and bandwidth before raising limits. |
| Relay hint is ignored | Check that the cloner is not using `relay_mode: off` and that the hint includes `@<node-id>`. |

To stop the packaged relay:

```bash
sudo systemctl stop swarm-relay
```

After changing a relay address, restart the relay and seeder daemon, confirm
registration with `swarm status`, and share a newly generated link.
