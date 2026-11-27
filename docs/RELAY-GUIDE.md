# Swarmgit relay operator guide

This guide covers installing a relay server, connecting clients to it, and
running it safely. A relay is optional: direct connections, the local network,
the DHT, peer exchange and offline bundles work without one. For the client,
see the [client guide](USER-GUIDE.md).

## 1. What a relay does

A relay makes a peer reachable when NAT, carrier-grade NAT, a mobile network or
a firewall blocks inbound connections. The seeder keeps an outbound connection
to the relay. When someone asks for that seeder's node ID, the relay tells it,
the seeder opens a second outbound connection, and the relay joins the two.

```text
Seeder ── control connection ──▶ Relay ◀── outbound dial ── Cloner
Seeder ◀── accept connection ─── Relay ══ byte stream ══ Cloner
```

The relay is a transport, not a git server. The encrypted Swarmgit session runs
between the two peers through it. The relay stores no repository data.

Swarmgit ships no default relay. Clients use the relays you configure, the
relay hints in links and, unless `relay_discovery` is `off`, public relays they
find through the DHT.

### Which kind to run

| Kind | Who can use it | How |
| --- | --- | --- |
| Unlisted | Anyone you give the address to | `--no-network`. The package default. |
| Private | Only clients with its key and password | `--password-file` and `--key-file` |
| Public | Any Swarmgit user can find it | Remove `--no-network` |

## 2. Requirements

- A Linux server with a stable public IP address. Behind NAT, forward the port.
- Preferably a DNS name pointing at it, such as `relay.example.org`.
- Open inbound **TCP 42425** for relay connections and **UDP 42425** for hole
  punching.
- Little CPU and memory. Bandwidth is the real cost: every relayed byte comes
  in and goes out again.

If the relay host runs a peer too, that peer uses TCP 42424 by default, which
does not clash.

### DNS

Create an `A` record (and `AAAA` for IPv6) for the name, pointing at the
server. It must reach the server directly. An HTTP proxy or CDN in front of the
name, such as a proxied Cloudflare record, does not pass relay connections.

### Firewall

With `ufw`, make sure SSH is allowed **before** turning the firewall on:

```bash
sudo ufw allow OpenSSH
sudo ufw allow 42425/tcp
sudo ufw allow 42425/udp
sudo ufw enable
sudo ufw status
```

With `firewalld`:

```bash
sudo firewall-cmd --permanent --add-port=42425/tcp
sudo firewall-cmd --permanent --add-port=42425/udp
sudo firewall-cmd --reload
```

Open the same ports in your cloud provider's firewall or security group.

## 3. Quick start

On a Debian or Ubuntu server with a DNS name already pointing at it:

```bash
curl -LO https://github.com/purry03/swarmgit/releases/download/v0.2.1/swarm-relay_0.2.1_amd64.deb
sudo apt install ./swarm-relay_0.2.1_amd64.deb
sudoedit /etc/default/swarm-relay
```

Set this line, with your own name:

```bash
SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --name relay.example.org:42425 --no-network --key-file /var/lib/swarm-relay/relay.key"
```

Then restart and check:

```bash
sudo systemctl restart swarm-relay
systemctl is-active swarm-relay
sudo journalctl -u swarm-relay -n 20 --no-pager
```

The log should show `relay listening on 0.0.0.0:42425` and
`relay address: relay.example.org:42425#<key>`.

On the machine that needs to be reachable, add the relay and restart its
daemon:

```yaml
# ~/.config/swarmgit/config.yaml
relays:
  - relay.example.org:42425
relay_mode: always
```

```bash
swarm daemon restart
swarm status relays
swarm link
```

`swarm status relays` should say `registered`, and the link should contain
`h=relay:relay.example.org:42425@<node-id>`. `relay_mode: always` makes the
test work even if the machine can already be reached directly; remove it
afterwards to use the relay only when needed.

Finally, clone that link from a different network.

## 4. Install

### Debian and Ubuntu

Packages: `swarm-relay_0.2.1_amd64.deb` and `swarm-relay_0.2.1_arm64.deb` from
the [releases page](https://github.com/purry03/swarmgit/releases). Check them against `SHA256SUMS` from the same release:

```bash
sha256sum -c SHA256SUMS --ignore-missing
sudo apt install ./swarm-relay_0.2.1_amd64.deb
```

The package installs:

| Path | Purpose |
| --- | --- |
| `/usr/bin/swarm-relay` | The server. |
| `/lib/systemd/system/swarm-relay.service` | The service unit. |
| `/etc/default/swarm-relay` | Its arguments. |
| `/var/lib/swarm-relay/` | Its key, created on first start. |

It creates the unprivileged `swarm-relay` account, then enables and starts the
service. The default arguments listen on `0.0.0.0:42425`, stay out of the
public network and set no `--name`. Set your name before relying on it.

### Fedora and RHEL

The RPM is built with `scripts/package.sh --rpm` as
`swarm-relay-0.2.1-1.x86_64.rpm` (or `aarch64`):

```bash
sudo dnf install ./swarm-relay-0.2.1-1.x86_64.rpm
```

It uses the same unit and settings file.

### Archive

The client archives also contain `bin/swarm-relay`. Copy it to
`/usr/local/bin/`, create the account, and save this as
`/etc/systemd/system/swarm-relay.service`:

```ini
[Unit]
Description=Swarmgit relay
After=network-online.target
Wants=network-online.target

[Service]
User=swarm-relay
Group=swarm-relay
ExecStart=/usr/local/bin/swarm-relay --listen 0.0.0.0:42425 --name relay.example.org:42425 --no-network --key-file /var/lib/swarm-relay/relay.key
StateDirectory=swarm-relay
StateDirectoryMode=0700
Restart=always
RestartSec=2
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -M -d /nonexistent -s /usr/sbin/nologin swarm-relay
sudo systemctl daemon-reload
sudo systemctl enable --now swarm-relay
```

### Run by hand

Useful for a test. `swarm relay` in the client takes the same options.

```bash
swarm-relay --listen 0.0.0.0:42425 --name relay.example.org:42425 \
  --key-file relay.key --no-network
```

Stop it with Ctrl-C. Without `--no-network` it joins the public network.

### Build from source

```bash
cargo build --release --locked -p swarm-relay
```

The binary is `target/release/swarm-relay`.

## 5. Configure the server

The package reads its arguments from `/etc/default/swarm-relay`. After editing:

```bash
sudo systemctl restart swarm-relay
sudo systemctl status swarm-relay
sudo ss -ltnp | grep 42425
```

A restart drops the sessions in progress. Registered clients reconnect by
themselves.

### The name

`--listen` is the local address to bind. `--name` is the public `host:port`
that clients dial, and it must match what they configure, character for
character in host and port. A client's registration proof is bound to it: if
they differ, the client reports `auth_failed: proof rejected`. Repeat `--name`
to accept more than one name.

Without `--name`, any name is accepted and proofs are not bound to your relay.
Do not run a public relay that way.

If you change the host or port, update the server and clients together, and
have seeders share a new `swarm link`.

### The key

`--key-file` keeps the relay's key across restarts, so that addresses of the
form `host:port#key` keep working. The log prints that address at each start:

```bash
sudo journalctl -u swarm-relay | grep 'relay address' | tail -1
```

Clients that use `#<key>` refuse any server that does not hold this key. Back
the file up:

```bash
sudo cp /var/lib/swarm-relay/relay.key /root/relay.key.backup
```

### Options

| Option | Purpose | Default |
| --- | --- | --- |
| `--listen ADDRESS` | Address and port to listen on. | `0.0.0.0:42425` |
| `--name HOST:PORT` | Public address clients should use; can be repeated. | none |
| `--key-file FILE` | File for the relay key, kept across restarts. | new key each start |
| `--password-file FILE` | Require a shared password; use with `--key-file`. | off |
| `--allow-node NODE_ID` | Allow only this node to register; can be repeated. | all nodes |
| `--metrics ADDR` | Serve counters over HTTP at this address. | off |
| `--max-listeners N` | Maximum number of registered peers. | 10000 |
| `--listeners-per-ip N` | Maximum registered peers from one address. | 2 |
| `--max-splices N` | Maximum number of relayed sessions at once. | 500 |
| `--splices-per-ip N` | Maximum sessions from one dialing address. | 8 |
| `--connects-per-minute N` | Maximum connection attempts per address per minute. | 20 |
| `--requests-per-minute N` | Maximum info and lookup requests per address per minute. | 120 |
| `--splice-kib-per-sec N` | Rate limit for each session; `0` means unlimited. | 8192 |
| `--node-daily-mib N` | Maximum data relayed for one target node each day. | 10240 |
| `--idle-secs N` | Close a session after this many idle seconds. | 120 |
| `--no-network` | Disable DHT announcements and relay gossip. | off |
| `--public` | Announce without waiting for another relay to dial back. | off |
| `--allow-private` | Accept private relay addresses on closed networks. | off |
| `--round-secs N` | Time between relay gossip rounds, in seconds. | 1800 |

The relay queues up to 1,000 requests. Each waits at most 10 seconds. It closes
a control connection after 75 seconds of silence. Per-address limits do not
apply to loopback; IPv6 addresses are counted by /64.

If the relay reaches `--max-listeners`, `--max-splices`, or the 1,000-request
queue limit, it returns `relay is full`. It logs this warning at most once a
minute. Clients display the message and suggest trying later or using another
relay.

### A private relay

Create the password file so only the service can read it:

```bash
openssl rand -base64 32 | sudo tee /etc/swarm-relay.password >/dev/null
sudo chown root:swarm-relay /etc/swarm-relay.password
sudo chmod 640 /etc/swarm-relay.password
```

Add `--password-file /etc/swarm-relay.password` to the arguments and restart.
Give users the full address from the log and the password:

```yaml
relays:
  - relay.example.org:42425#<key>
relay_password: <the password>
```

A client sends the password only to an address that includes the key, so a
machine that merely answers on your name never receives it. Links never carry
the key or password: every machine that dials the relay, including one that
only clones through it, needs both in its settings. A private relay never joins
the public network.

To restrict who may register instead, use `--allow-node <node-id>` once per
machine. `swarm status` on a client shows its node ID. This limits who can be
reached through the relay, not who can dial them.

### A public relay

Remove `--no-network` and restart. The relay announces itself on the DHT once
another relay has dialed it back, and exchanges relay addresses with others. If
no other relay can dial it back, such as the first one of an empty network, add
`--public`.

```bash
SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --name relay.example.org:42425 --public --key-file /var/lib/swarm-relay/relay.key"
```

If the name is a domain that resolves to the address clients dialed, they show
it and put it into share links instead of the IP address, so links survive an
address change.

With debug logging on, `relay directory announce done=true` means the
announcement was stored. From another machine, `swarm discover-relays` should
then list the relay.

Before you do this:

- Anyone can use the relay, within the limits above. You pay for the traffic.
- Unrelated hosts will connect to the port. Debug lines such as
  `relay protocol: bad frame length` are most likely BitTorrent clients and
  crawlers that saw the DHT announcement. They are dropped.
- A client with several relays may pick another one. Users who want yours
  should list it in `relays`.

### A relay as an onion service

Tor can publish a relay without changes to it. In `torrc`:

```text
HiddenServiceDir /var/lib/tor/swarm-relay/
HiddenServicePort 42425 127.0.0.1:42425
```

Run with `--listen 127.0.0.1:42425 --name <name>.onion:42425 --no-network`.
Clients need `proxy` set; an onion relay is never dialed without one. See the
[Tor guide](TOR-GUIDE.md).

## 6. Connect clients

### The seeder

Configure the machine that must be reachable, in
`~/.config/swarmgit/config.yaml` for one user or `/etc/swarmgit/config.yaml`
for the whole machine:

```yaml
relays:
  - relay.example.org:42425
```

`SWARM_RELAYS` is the environment variable form and takes priority. At most two
configured relays are used.

```bash
swarm daemon restart
swarm seed 'swarm://<repo-id>'
swarm status relays
swarm link
```

If the daemon runs as a service, restart the service instead.

| `swarm status relays` shows | Meaning |
| --- | --- |
| `registered` | Working. `DETAIL` shows the address the relay sees. |
| `not registered` | `DETAIL` has the reason. |
| `none in use` | No relay is configured or discovered, or `relay_mode` does not call for one. |

Share the link printed by `swarm link`. An old link does not gain new relay
hints by itself.

`relay_mode` defaults to `auto`: register unless peers have recently reached
this machine directly. `always` registers regardless. `off` never uses a relay,
as seeder or cloner. With discovery on, a node also registers with public
relays it found (at most 2 found, 3 in all), and only while it serves at least
one repository. `relay_discovery: off` limits it to configured relays.
Discovery is off behind a proxy.

### The cloner

Someone cloning needs only outbound access to the relay named in the link. No
configuration is needed:

```bash
git clone 'swarm://<repo-id>?h=relay:relay.example.org:42425@<node-id>' project
```

Direct addresses are tried first, with about 1.5 seconds' head start
(`relay_delay_ms`), and the relay immediately if they fail. A cloner with only
a node ID or repo ID can ask relays which of them holds it; they see a hash of
the repo ID, never the ID. Relays do not forward to each other, so a transfer
crosses at most one.

`relay_max_bytes` defaults to 2 GiB per connection. After peers meet through
the relay, they also try a direct UDP connection. The relay stays open as a
fallback for the sync. This can save relay bandwidth, but each peer can see the
other's IP address. Set `udp: false` on either peer to keep the transfer on the
relay.

## 7. Operate the relay

### Logs

```bash
sudo journalctl -u swarm-relay -f
sudo journalctl -u swarm-relay | grep -E 'WARN|ERROR'
```

The default level is `info`, which logs no line per registration or session.
For connection errors and network rounds, turn on debug logging:

```bash
sudo systemctl edit swarm-relay
# add these two lines, then save:
#   [Service]
#   Environment=RUST_LOG=info,swarm_relay=debug
sudo systemctl restart swarm-relay
```

Undo it with `sudo systemctl revert swarm-relay` and a restart. `revert` also
removes any other overrides of the unit.

| Log line | Meaning |
| --- | --- |
| `Our current id ... is not valid for address ... Using new id ...` | Normal, once per start. The DHT node picks an ID that matches the public IP. |
| `mainline::rpc: Could not bootstrap the routing table` | Usually follows the line above, while the table is being rebuilt. Check for `announce done=true` before treating it as a fault. |
| `relay network round relays=0 reachable=true` | Announcing, and no other relay known yet. |
| `relay io: early eof` | The other end closed while the relay was reading. Expected when a client stops. |
| `relay protocol: bad frame length` | The other end is not speaking the relay protocol. |
| `relay auth: noise: decrypt error` | A client's encrypted handshake failed. If it repeats, check the `#<key>` in client addresses. |

### Metrics

Add `--metrics 127.0.0.1:9100`, restart, then:

```bash
curl http://127.0.0.1:9100/metrics
```

| Metric | Type | Meaning |
| --- | --- | --- |
| `swarm_relay_listeners` | gauge | Registered peers now. |
| `swarm_relay_splices` | gauge | Relayed sessions now. |
| `swarm_relay_splices_total` | counter | Sessions since start. |
| `swarm_relay_bytes_total` | counter | Bytes carried since start. |
| `swarm_relay_auth_failures_total` | counter | Failed authentications. |
| `swarm_relay_errors_total` | counter | Connection errors. |

The endpoint has no authentication. Keep it on loopback.

### Upgrade

Install the newer package; the service restarts. When asked about
`/etc/default/swarm-relay`, keep your version, or run:

```bash
sudo dpkg --force-confold -i swarm-relay_*.deb
```

A kept file keeps its old arguments, so it does not pick up new defaults.
Afterwards check `systemctl is-active swarm-relay` and the log.

### Move to another server

Install the package there, copy `/var/lib/swarm-relay/relay.key` and your
arguments, give the key file to the `swarm-relay` user with mode 600, restart,
and point the DNS name at the new server. Clients that use the name and `#<key>`
need no change.

### Stop or remove

```bash
sudo systemctl stop swarm-relay          # stop until the next start or boot
sudo systemctl disable --now swarm-relay # keep it off
sudo apt remove swarm-relay              # remove, keeping the settings
sudo apt purge swarm-relay               # also remove settings and the account
sudo rm -rf /var/lib/swarm-relay         # delete the key as well
```

## 8. Capacity

Each registered peer holds one long-lived TCP connection: a file descriptor and
a little memory. Ten thousand registered peers do not mean ten thousand
transfers. Each relayed session adds two data connections and copies bytes both
ways, so large clones cost mostly bandwidth.

| Resource | Default limit |
| --- | --- |
| Registered peers | 10,000 |
| Relayed sessions at once | 500 |
| Rate of one session | 8 MiB/s |
| Bytes per target node per day | 10 GiB |
| Idle session timeout | 120 seconds |
| Connect attempts per address | 20 a minute |
| Info and lookup requests per address | 120 a minute |

On a small server, lower `--max-splices`, `--splice-kib-per-sec`, and
`--node-daily-mib` to limit concurrent sessions, each session's transfer rate,
and daily traffic to a target node. Higher limits do not add bandwidth or file
descriptors, so check CPU, memory, open files, and traffic before raising them.
For a server with one CPU and 512 MB of memory, start with `--max-splices 25`.
Daily quotas are kept in memory and reset when the relay restarts.

## 9. Privacy and security

- The relay sees both peers' IP addresses and node IDs, connection times and
  byte counts.
- It cannot read repository IDs, file contents or signed states: the peer
  session is encrypted end to end.
- The node ID in a relay hint pins the expected peer. A hostile relay can block
  or drop a connection, but cannot impersonate the peer.
- The control connection is encrypted to the relay's key when the client knows
  it. A client that learned the key from the relay itself is protected only
  against passive observers; use `#<key>` to rule out an impostor.
- The relay writes only its key file. Logs can contain IP addresses; handle and
  expire them accordingly.
- It is not a general proxy. It only pairs a request for a registered node with
  that node's one-time answer.
- Keep the server updated, and expose no ports beyond SSH and the relay port.

## 10. Troubleshooting

Check these in order: the service is active, the port is listening, the port is
reachable from outside, the name matches, then the client.

```bash
systemctl is-active swarm-relay            # on the server
sudo ss -ltnp | grep 42425                 # on the server
nc -vz relay.example.org 42425             # from another network
swarm status relays                        # on the seeder
```

| Symptom | Cause and fix |
| --- | --- |
| Service does not start | Read `journalctl -u swarm-relay`. A bad option, a port in use, or a key or password file the `swarm-relay` user cannot read. |
| `Connection refused` | Nothing is listening at that address. Check the service, the port and the DNS record. |
| Timeout connecting | A firewall or security group blocks TCP 42425, or the DNS name is behind an HTTP proxy. |
| `auth_failed: proof rejected` | The client's address differs from `--name`. Make them match. |
| `auth_failed: this relay needs its password` | Set `relay_password`, and write the address with `#<key>`. |
| `limit_exceeded: too many listeners` | More than 2 daemons behind one public address are registering. Raise `--listeners-per-ip`. |
| `limit_exceeded: relay is full` | The relay log shows which server-wide limit was reached. Raise it only if the machine has room; otherwise, use another relay. |
| `limit_exceeded: too many requests` | This address exceeded `--requests-per-minute` for info and lookup requests. |
| `busy` or `limit_exceeded` on clone | A session or rate limit was reached. Check load before raising it. |
| `none in use` on the seeder | No relay configured or found, or the machine is reachable directly. Set `relay_mode: always` to test. |
| Link has no `h=relay:` hint | The seeder is not registered yet. Wait for `registered`, then run `swarm link` again. |
| `no_such_node` when cloning | The seeder is offline or no longer registered there. Restart its daemon and share a new link. |
| Relay hint ignored | The cloner has `relay_mode: off`, or the hint lacks `@<node-id>`. |
| A public relay is never found | It must run without `--no-network`, a password or `--allow-node`, and needs `--public` if alone. |

### Testing discovery

The daemon remembers relays it has seen. To test discovery from nothing, use a
fresh `SWARM_HOME`, remove `relays` from every settings file (both
`/etc/swarmgit/config.yaml` and `~/.config/swarmgit/config.yaml` are read),
make sure `relay_discovery` is on, set `relay_mode: always`, and seed a
repository there. Settings from files are passed to the daemon as environment
variables, so seeing `SWARM_RELAYS` in its environment does not mean it came
from your shell.
