# Swarmgit relay: design spec (draft)

Status: draft for review. Nothing here is implemented. It extends sections 6, 7 and 7.5 of the main spec and replaces the "relay-through-peer is out of scope" line. Section 5.4 covers relay-to-relay discovery.

## 1. Problem

Outbound dialing works from behind NAT, but two peers that are both unreachable (mobile data, CGNAT, strict firewalls) cannot connect. Today the clone ends with `no_peers` and advice to use a VPN. A relay lets such peers reach each other with no VPN and no port forwarding.

## 2. Goals and non-goals

Goals
- REL-G1. A peer that cannot accept inbound connections can still be cloned from, and can clone from others.
- REL-G2. The relay never sees repo IDs, repo contents, or plaintext. It cannot forge or alter traffic (INV-1, NET-11 hold unchanged).
- REL-G3. No change to sync, manifests or the message protocol. The relay is a transport.
- REL-G4. Anyone can run a relay. A client uses zero, one or several. Nothing is mandatory (INV: no central dependency).
- REL-G5. Direct connections stay the fast path. The relay is a fallback.
- REL-G6. Relays find each other, and clients find relays, with no configured list and no central directory, the way BitTorrent trackers and the Mainline DHT work. A node must be reachable through relays it never heard of until it needed them.

Non-goals (v1)
- UDP hole punching and direct-connection upgrade (listed in 10).
- Hiding node IDs and IPs from the relay or from a path observer of the relay connection (see 8).
- Paying for, or rate-plan billing of, bandwidth.

## 3. Design in one paragraph

A relay is a small TCP server that splices two byte streams. A trapped peer keeps one outbound control connection to a relay and is reachable at the address `relay + node ID`. A dialer connects to the relay and asks for that node ID. The relay tells the trapped peer over its control connection, the trapped peer opens a second outbound connection to the relay to accept, and the relay copies bytes between the two. The normal swarm handshake (Noise XX, ML-KEM, PqAuth) then runs end to end through the pipe. The dialer pins the target node ID, so a relay that tries to man-in-the-middle fails with `auth_failed`.

```
 trapped B ──control──▶ relay ◀── dial(B) ── A
 trapped B ◀─Incoming(tok)─ relay
 trapped B ──accept(tok)─▶ relay ══ splice ══ A
 A ◀═════ swarm handshake and sync, end to end encrypted ═════▶ B
```

This works because `swarm-net::handshake::{connect, accept}` are already generic over any async stream.

## 4. Relay protocol (new crate `swarm-relay`, wire in `swarm-net`)

Relay framing before the splice: `u32` big-endian length plus one CBOR array, first element the type, 8 KiB maximum. After `Ready`, the connection is raw bytes in both directions with no relay framing.

Default relay port: TCP 42425.

| Type | Name | Fields | Sent by | Reply |
| --- | --- | --- | --- | --- |
| 0 | `RHello` | `version` (1), `role` (1 listen, 2 connect, 3 accept, 4 info) | client | none |
| 1 | `RError` | `code` (tstr), `msg` (tstr) | relay | none, then close |
| 2 | `Challenge` | `nonce` (bstr 32) | relay | `Prove` |
| 3 | `Prove` | `bundle` (bstr 1984), `sig` (bstr 3373), `name` (tstr, the relay address the listener dialed) | listener | `Listening` |
| 4 | `Listening` | `ttl_secs`, `observed` (bstr, listener's address as the relay sees it) | relay | none |
| 5 | `Incoming` | `token` (bstr 16) | relay to listener | listener opens an accept connection |
| 6 | `Connect` | `target` (bstr 32, node ID) | dialer | `Ready` or `RError` |
| 7 | `Accept` | `token` (bstr 16) | acceptor | `Ready` or `RError` |
| 8 | `Ready` | none | relay | raw splice begins |
| 9 | `RPing` | `nonce` | either, on control connection | `RPong` |
| 10 | `RPong` | `nonce` | either | none |
| 11 | `RInfo` | none | any client, role 4 (info) | `RInfoReply` |
| 12 | `RInfoReply` | `version`, `caps` (tstr array), `limits` (map), `relay_id` (bstr 32), `observed` | relay | none |
| 13 | `RGetRelays` | `max` (at most 32) | any client, role 4 | `RRelays` |
| 14 | `RRelays` | array of relay addresses (kinds 4 and 6 of 7.3, no node ID), at most 32 | relay | none |
| 15 | `Where` | `target` (bstr 32) | any client, role 4 | `Referral` |
| 16 | `Referral` | `found` (bool: registered here), `closer` (array of relay addresses, at most 8) | relay | none |

Error codes: `no_such_node`, `busy`, `timeout` (the listener did not accept in time), `limit_exceeded`, `bad_token`, `auth_failed`, `malformed`, `unsupported_version`.

Rules
- REL-1. Listen. After `RHello role=1` the relay sends `Challenge`. The listener signs `relay_auth_input(nonce, name)`, where `name` is the `host:port` it dialed, with its node key, using the same hybrid suite 1 signature as `PqAuth`, and sends `Prove`. The relay checks that `name` is one of its configured names (`--name`), then `key_id(bundle)` and both signature halves. A malicious relay that forwards its challenge to another relay gets a signature over its own name, which the other relay rejects. The node ID is `key_id(bundle)`. This stops anyone from claiming another node's mailbox. Domain string `CTX_RELAY = b"swarmgit/v1/relayauth"` joins the table in 4.2 of the main spec.
- REL-2. A node ID has at most one live control connection. A newer one replaces the older one, so reconnects after a network change work at once.
- REL-3. Connect. The relay looks up `target`. If it is not listening, reply `RError no_such_node` and close. Otherwise create a pending pair with a random 16-byte token, send `Incoming(token)` to the listener, and hold the dialer's socket for at most 10 s.
- REL-4. Accept. The listener opens a new TCP connection, sends `RHello role=3` and `Accept(token)`. The relay checks the token exists, was issued to this node, and has not expired, then sends `Ready` to both sockets and starts copying. A token is single use.
- REL-5. Splice. Copy bytes both ways with no inspection. Close both sides when either closes. Idle close after 120 s with no bytes in either direction (sessions ping every 30 s, so a live session keeps it open).
- REL-6. Keepalive. The listener sends `RPing` every 25 s. The relay closes a control connection that is silent for 75 s. The listener reconnects with exponential backoff from 1 s to 60 s.
- REL-7. The relay stores nothing on disk and keeps no logs of node IDs by default. Counters only.

## 5. Addresses and discovery

5.1 New address kinds (extend 7.3 `Peers` encoding)

| Kind | Meaning |
| --- | --- |
| 8 | Relay over IPv4: 4 bytes relay IP, `u16` relay port, 32-byte target node ID |
| 9 | Relay over IPv6: 16 bytes relay IP, `u16` relay port, 32-byte target node ID |

`PeerAddr` gains `Relay { relay: SocketAddr, node: NodeId }`. A relay address always carries a node ID, so it is always pinned. A node that does not know the kinds ignores them, as it ignores any unknown address.

5.2 How a dialer learns a relay address

- REL-8. Link hints. `swarm link` appends relay hints, `h=relay:relay.example:42425@<node-id>` in the link's hint list (hint scheme `relay`, R2), one per relay this node is registered with (at most 2). Hostnames are resolved on the dialer. This is the main path and needs no DHT. It reuses the `@<node-id>` pin from NET-11.
- REL-9. Peer exchange. A node registered with a relay includes `Relay { relay, self }` for itself in the `Peers` replies it sends and in `Hello.observed`-adjacent data (the `relay` cap, see 5.3). Other peers pass these along like any address.
- REL-10. Remembered. The `peers` table stores relay addresses like any other, with a `kind` column.
- REL-11. The DHT cannot carry a trapped node's relay address (Mainline stores only the announcer's IP and port). A trapped node is found by link hint, peer exchange, or, with only its node ID, by the home relay computation in REL-29. Finding which node ID holds a repo still needs peer exchange or topics (R6).

5.3 Capability. Nodes registered with a relay add `relay` to `Hello.caps`. Informational only.

5.4 Relay network: how relays and clients find each other

Relays are independent servers. There is no coordinator. Like BitTorrent trackers, any public relay can be found through the Mainline DHT, and relays and clients pass relay addresses around by gossip.

- REL-23. Directory infohash. `H_dir = blake3::derive_key(CTX_RELAY_DIR, "v1")[..20]`, where `CTX_RELAY_DIR = "swarmgit/v1 relay directory"` joins 4.2 of the main spec. It is a fixed, public value. A relay that believes it is publicly reachable runs `announce_peer(H_dir, port)` on the Mainline DHT every 15 minutes, using the same `swarm-disc` DHT code as repo discovery (6.4). Anyone can run `get_peers(H_dir)` to list relay candidates.
- REL-24. Probe before trust. A candidate address, from the DHT or from gossip, is added to the relay set only after a client connects with `role=4`, gets a valid `RInfoReply` with a supported `version`, and the reply's `observed` is sane. Candidates that fail are dropped and not retried for 10 minutes. This keeps junk DHT entries out.
- REL-25. Reachability self-check. A relay announces to the DHT only after another relay or a bootstrap probe has dialed it back successfully, so a relay behind NAT does not poison the directory. A relay started with `--public` skips the check.
- REL-26. Relay gossip. Every relay asks the relays it knows for `RGetRelays` every 30 minutes and merges verified results. Clients do the same with the relays they use. Each side keeps at most 256 verified relays, ranked by recent success and round-trip time, and answers `RGetRelays` with a random sample of the healthy ones. This is peer exchange (6.5) applied to relays.
- REL-27. Bootstrap, in order: the `relays` config list and `relay:` link hints, relays saved in `$SWARM_HOME` from last run, the DHT directory (REL-23), then gossip. With a populated saved list the DHT is not needed. With nothing configured the DHT alone suffices.
- REL-28. Placement by rendezvous hashing. For a node ID `n` and a relay `r`, `weight(n, r) = blake3(CTX_RELAY_PLACE || n || r.addr)`. The relays with the three highest weights among a node's known relay set are its home relays. A trapped node registers with its top 2 home relays that are reachable, plus any relays it was configured with, at most 3 in all.
- REL-29. Finding a node with only its node ID. A dialer computes the node's top 3 home relays from its own relay set and sends `Connect` to each in turn, or in parallel with a 300 ms stagger. A hit needs no relay hint. When the dialer's relay set differs from the target's, a relay that does not have the node replies to `Where` with a `Referral` listing the relays it considers closer. The dialer follows referrals for at most 2 hops. Relays answer `Where` from local state and from their own gossip. They never forward `Connect` traffic to each other (no multi-hop splice), so every byte path is exactly one relay.
- REL-30. Advertising. A registered node publishes its relay addresses as kind 8 and 9 entries (5.1) in `Peers`, as in REL-9. The home relays (REL-28) are preferred, and the link carries the same ones (REL-8).
- REL-31. Spam and sybils. An attacker can fill the DHT directory with fake relays. REL-24 removes the fakes. The attacker can also run many real relays that drop traffic. Defenses: clients prefer relays with a track record (a relay is ranked by successful splices, kept in `$SWARM_HOME/relays.db`), a node registers with up to 3 relays so one bad relay is not decisive, and the relay set is capped at 256 so flooding costs the attacker real servers. Relays cannot read or alter content (section 8), so the worst case is denial of service on one path.
- REL-32. Privacy of discovery. A client doing `get_peers(H_dir)` reveals to the DHT only that it wants relays, the same exposure as any DHT lookup. Relay lookups happen only when direct dials fail and `relay_mode` is not `off`. A new config key `relay_discovery` (`on` | `off`, default `on`) turns the DHT and gossip off for people who want only their own relays.

```
 relay set sources:   config ─┐
                      saved ──┤
                      DHT ────┼─▶ probe (REL-24) ─▶ relay set ─▶ gossip (REL-26)
                      gossip ─┘                         │
                                      home relays = top 3 HRW(node, relay)  (REL-28)
```

## 6. Client behavior (swarm-daemon)

- REL-12. Config keys: `relays` (list of `host:port`, default empty: extra pinned relays, discovery supplies the rest, see 5.4), `relay_discovery` (REL-32), `relay_mode` (`auto` | `always` | `off`, default `auto`), `relay_max_bytes` (per-session cap through a relay, default 2 GiB, matches `max_pack_bytes`).
- REL-13. Registration. With `auto`, register with each configured relay (up to 2) when direct inbound reachability is not confirmed: UPnP mapping failed and no peer has reported our `observed` address as reachable. `always` registers regardless. `off` never uses relays, including as a dialer.
- REL-14. Dial order (happy eyeballs). Dial direct addresses first. Start relay addresses for the same node 1.5 s later, or at once when the direct dial fails. The first session to finish the handshake wins and the others are closed. Already-open sessions are not re-dialed.
- REL-15. Serving. A connection accepted through the relay is handed to the existing `serve` path as an ordinary stream, with the responder role, so interest matching, bans and quotas apply. Per-IP limits use the node ID instead of the IP for relayed streams.
- REL-16. `sync` progress shows relayed sessions: `connected via relay`. The `no_peers` message (REACH-1) adds the count of relay addresses tried and, when none are configured, says to set `relays`.
- REL-17. Status. `swarm status` shows relay registration state, relay address and last error.
- REL-18. If a relayed session is the only healthy session and a direct address for the same node later works, prefer it for the next fetch (no live migration in v1).

## 7. Relay server

- Binary: R1 ships `swarm-relay --listen 0.0.0.0:42425 --name relay.example:42425`, from the new crate `swarm-relay` (library plus binary, testable without the CLI). Folding it into the `swarm` binary as `swarm relay` happens in R4. A systemd unit `swarmgit-relay.service` ships in the deb and rpm, disabled by default.
- REL-33. A relay also acts as a relay-network member: it runs the directory announce (REL-23), the self-check (REL-25), gossip (REL-26) and answers `Where` (REL-29). A relay with `--no-network` opts out and serves only direct listeners and dialers, which suits a private relay.
- REL-19. Limits, all configurable with defaults: 10,000 listeners, 2 listeners per IP, 500 concurrent splices, 8 per IP, 20 `Connect` attempts per minute per IP, per-splice bandwidth 8 MiB/s, per-node 10 GiB per day. Exceeding any gives `RError limit_exceeded`.
- REL-20. Pending state (tokens) expires in 10 s and is capped at 1,000 per relay.
- REL-21. A relay needs no keys of its own in v1. It does not authenticate clients except listeners (REL-1). Operators who want a private relay set `--allow-node <id>` (repeatable), which limits listeners to those nodes, or `--password-file`, a shared secret sent in `RHello`.
- REL-22. Metrics endpoint is off by default. When on, it exposes counters only: listeners, splices, bytes, errors.

## 8. Security

| Threat | Response |
| --- | --- |
| Relay reads traffic | Not possible. The Noise and ML-KEM handshake runs end to end. |
| Relay tampers or impersonates the target | Dialer pins the node ID (NET-11). Mismatch gives `auth_failed`. Content is also checked by hashes and signatures (INV-1). |
| Hijack another node's mailbox | REL-1 requires a signature by the node key. |
| Relay learns who talks to whom | Yes: it sees the target node ID, both IPs, timing and volume. Repo IDs and contents stay hidden. Stated as a known limit. |
| Path observer sees node IDs in `Connect` and `Challenge` | Yes in v1 (control framing is plaintext). R5 wraps the control channel in Noise NK to the relay key to fix this. |
| Relay used as an open proxy for abuse | It only splices to registered nodes that accepted a token issued by the relay, never to arbitrary hosts. Limits in REL-19 bound bandwidth abuse. |
| Resource exhaustion | REL-19, REL-20. |
| Listener enumeration | `Connect` to an unknown node gives `no_such_node`, which reveals whether a node ID is online. Node IDs are 32-byte hashes, not guessable. Acceptable. |
| Malicious relay selectively drops | A node registers with up to 3 relays and dialers try several. A relay can deny service but not corrupt data. |
| Fake relays flooding the directory or gossip | Probe before trust (REL-24), cap of 256, ranking by track record (REL-31). |
| Eclipse of a node's home relays | HRW placement is public, so an attacker could run relays that win the top 3 for a node ID. The node also registers with configured and saved relays and link hints carry the working ones, so an eclipse needs to beat all of them. Accepted risk, stated. |

## 9. Milestones

- R1. `swarm-relay` crate: protocol types, server (listen, connect, accept, splice), unit tests for each rule, and a client library `RelayClient::{listen, connect}` returning a stream. Status: done. Delivered with `swarm-relay` (library, `swarm-relay` server binary, `swarm-relay-peer` test tool), 14 integration tests in `crates/swarm-relay/tests/relay.rs`, and `tests/e2e/relay.sh`. Acceptance: two test peers on isolated Docker networks (`compose.relay.yaml`) cannot reach each other directly, then complete the full swarm handshake with a pinned node ID and exchange a message through a relay container. A git clone through the relay needs the daemon integration in R2.
- R2. Daemon integration with a configured relay: `relays` config, registration loop with backoff, `PeerAddr::Relay`, address kinds 8 and 9, dial fallback (REL-14), serve path, link hints (REL-8), `swarm status`, `no_peers` text. Acceptance: the `doc` scenario (both machines on separate networks) succeeds with a relay in the link. Status: done. Configure `relays: [host:port]` in YAML or use `SWARM_RELAYS=host:port[,host:port]` (at most 2 used); `relay_mode` and `SWARM_RELAY_MODE` accept `auto|always|off`. The settings can be read from `/etc/swarmgit/config.yaml` or `~/.config/swarmgit/config.yaml`, with environment variables taking precedence. `swarm link` and `swarm init` carry `h=relay:` hints, `swarm status` prints `relay <name> registered`, relay dials wait 1.5 s (`SWARM_RELAY_DELAY_MS`) when a direct address for the same node is also tried. `tests/e2e/relay.sh` now also clones a real repo between two isolated containers through the relay and checks the no-relay and mode-off cases. Reachability checks that let `auto` skip registration come with R3.
- R3. Relay network: `role=4` messages (`RInfo`, `RGetRelays`, `Where`), DHT directory announce and lookup (REL-23), probing and self-check (REL-24, REL-25), gossip (REL-26), saved relay set with ranking, HRW placement and node-ID-only dialing with referrals (REL-28, REL-29). Acceptance: three relays that know only the DHT (the Docker DHT test net) find each other; a client with no `relays` config and no link hint reaches a trapped node.
- R4. Server hardening: limits, quotas, allowlist and password, `--no-network`, metrics, systemd unit, deb and rpm packaging, README and user guide section on running a relay.
- R5. Control channel privacy: Noise NK to the relay, with the relay key in the address (`relay.example:42425#<key-id>`) and in `RInfoReply.relay_id`.
- R6. Repo rendezvous topics: listeners also register `topic = blake3(derive_key(CTX_TOPIC), repo_id)` per seeded repo on the home relays of the topic (HRW applied to the topic), and a dialer can `Lookup(topic)` to get node IDs. The relay learns a hashed topic, no more than the DHT does. This lets a trapped seeder be found from a repo ID alone.
- R7. Later: hole punching (UDP or simultaneous TCP open) coordinated through the relay, and upgrade from a relayed session to a direct one.

## 10. Test plan

- Unit: framing limits, challenge and prove (valid, wrong key, replayed nonce, wrong relay address), token expiry and single use, replace on reconnect, limit enforcement, splice closes both sides, idle timeout.
- Integration (Docker compose, three networks `neta`, `netb`, `netr`; relay on `netr` and both nets' gateways, peers A and B unable to reach each other):
  1. B seeds a repo and registers with the relay. A clones with a link carrying a `relay:` hint. Verify the clone matches and the log says `connected via relay`.
  2. Kill the relay mid-clone: the clone fails cleanly with a message naming the relay.
  3. Relay MITM test peer that bridges to a different node: dial fails with `auth_failed`.
  4. Direct reachable peer plus relay address: the direct path wins and no relay bytes flow.
  5. Link without relay hint and no DHT result: `no_peers` message mentions relays.
- Relay network (Docker, with the existing DHT test net): relays R1 to R3 announce to `H_dir`, a fresh client lists them; a fake DHT entry that does not speak the protocol is dropped; a relay behind NAT does not announce (REL-25); gossip converges to the same set within two intervals; `Where` referral reaches the right relay when the dialer's set differs; killing one home relay still lets a dial succeed through another.
- Hostile: oversized relay frames, `Accept` with a stolen or expired token, 1,000 `Connect` calls to unknown nodes, slow-loris on `RHello`.

## 11. Open questions

1. With discovery in place there is no need to ship a default relay. Is it acceptable that the first public relays are run by the community or by you, found only through the DHT directory?
2. `relay_discovery` defaults to `on` so that clones work with zero setup. This exposes a DHT lookup and metadata to unknown relay operators when a direct dial fails. Keep the default on, or require an explicit opt-in?
3. Should a relay require a shared password by default for self-hosted use, or be open with rate limits? (A relay with `--no-network` and a password is the private option.)
4. Per-session byte cap through a relay: keep 2 GiB, or lower (for example 512 MiB) so public relays are not used for huge clones?
5. R6 (topics) before or after R5 (NK encryption)? Topics make trapped seeders findable from a repo ID. NK hides node IDs from path observers.
