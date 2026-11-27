# Swarmgit over Tor

Swarmgit can reach peers through Tor, host your daemon as an onion service, or
both. This is optional. Tor hides your IP address from peers; it does not make
Swarmgit anonymous by itself (see "What Tor does not hide").

## 1. Install Tor

```bash
sudo apt install tor        # Debian, Ubuntu
sudo dnf install tor        # Fedora
brew install tor && brew services start tor   # macOS
```

A system Tor listens for SOCKS connections on `127.0.0.1:9050`. Tor Browser
uses port 9150 instead.

## 2. Send all traffic through Tor

In `~/.config/swarmgit/config.yaml`:

```yaml
proxy: socks5://127.0.0.1:9050
```

or `SWARM_PROXY=socks5://127.0.0.1:9050`. Then restart the daemon with
`swarm daemon restart`.

With a proxy set:

- every outbound TCP connection goes through it, including relays and static
  mirrors, and host names are resolved by the proxy, never locally;
- LAN discovery, the Mainline DHT, UPnP and UDP hole punching are off, because
  they cannot go through a SOCKS proxy;
- the daemon listens on `127.0.0.1:42424` unless `listen` says otherwise;
- `swarm link` leaves out your direct address unless `advertise` is set;
- a proxy value that cannot be parsed stops the daemon. It never falls back to
  direct connections.

With the DHT and LAN off, a clone needs a hint in the link: an `onion:`,
`tcp:`, `relay:` or `http(s):` hint.

`swarm status` prints the proxy in use.

## 3. Host an onion service

An onion service lets peers reach you with no port forwarding and without
learning your IP address.

Enable Tor's control port in `/etc/tor/torrc`:

```text
ControlPort 9051
CookieAuthentication 1
CookieAuthFileGroupReadable 1
```

The daemon's user must be able to read the cookie file. On Debian and Ubuntu:

```bash
sudo usermod -a -G debian-tor "$USER"    # then log in again
sudo systemctl restart tor
```

Or use a password instead of the cookie: run `tor --hash-password <secret>`,
put the result in `torrc` as `HashedControlPassword`, and set
`onion_password` (below).

Then configure Swarmgit:

```yaml
proxy: socks5://127.0.0.1:9050     # reach other onion peers
onion_control: 127.0.0.1:9051      # host your own service
# onion_password: <secret>         # only with HashedControlPassword
```

After `swarm daemon restart`, `swarm status` shows the onion address and
`swarm link` includes a hint such as
`h=onion:<56 characters>.onion:42424@<node-id>`.

- The service key is saved in `$SWARM_HOME/keys/onion.key` (mode 0600), so the
  address stays the same across restarts. Delete the file to get a new one.
- Tor must run on the same machine as the daemon: the service forwards to a
  loopback port.
- The service lives as long as the daemon's control connection. If Tor
  restarts, the daemon adds the service again.
- `onion_control` without `proxy` is allowed. Then onion visitors can reach
  you, but your own outbound connections are direct.

## 4. Limits for onion visitors

Tor makes all visitors look alike, so one cannot be banned on its own. They
share their own limits, which leaves the other connection slots for direct
peers:

```yaml
onion_max_connections: 16     # open at once, 1 to 64
onion_max_per_minute: 120     # new connections per minute
```

## 5. Clone through Tor

```bash
export SWARM_PROXY=socks5://127.0.0.1:9050
swarm daemon restart
git clone "swarm://<repo-id>?h=onion:<address>.onion:42424@<node-id>"
```

Keep the `@<node-id>` part: it pins the peer, so nobody else can answer for it.

## What Tor does not hide

- Peers still learn your node ID, which stays the same over time. Use a
  separate `SWARM_HOME` for an identity you want to keep apart.
- Manifests and git history carry whatever names and email addresses you
  committed with.
- A repo you publish is public to anyone who has its link.

## Troubleshooting

| Message | Likely cause |
| --- | --- |
| `cannot host the hidden service ... cannot read the Tor cookie` | The daemon's user is not in Tor's group. |
| `Tor wants a password: set onion_password` | `torrc` uses `HashedControlPassword`. |
| `Tor refused the request: 515 ...` | Wrong password or cookie. |
| `no_peers` for an onion link | `proxy` is not set, or the seeder is offline. |
