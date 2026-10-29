# Improvements

Deferred work and UX gaps found while building and demonstrating M2. None of
these block the M2 acceptance set; they are the next things that make a
multi-peer workflow feel normal.

---

## 1. Maintainers: write access without copying the root key

**Priority:** high  
**Spec:** 5.1 field 8, 5.4 (authorization and scope), 8.4, 11.8 (M7)

### Current behaviour

A Swarmgit repo is identified by its root key. `publish` refuses to sign
without `$SWARM_HOME/keys/<repo-id>.key`, so a fresh clone is read-only. To
push from a second machine today the user copies the root key file. That works,
but it hands the second machine full root authority: it can publish any ref,
change maintainers, and rotate ownership.

`swarm fork` is the only other option, and it creates a new repo with a new ID.

### Proposed change

Use the maintainer mechanism that the manifest already defines.

- A writer generates its **own** suite-1 key for the repo. This should be a
  local operation that writes `$SWARM_HOME/keys/<repo-id>.key` (the existing
  key-file format already allows a suite-1 key that is not the repo ID).
- The writer sends its public bundle (or base32 key ID) to the root owner.
- The root owner runs:

  ```text
  swarm maintainers add <repo> <pubkey-base32> [globs...]
  swarm maintainers remove <repo> <pubkey-base32>
  ```

  `add` edits the root manifest's `maintainers` array and publishes a new root
  manifest.
- `publish` is relaxed to allow a non-root key that is listed in the current
  root manifest. It builds a non-root manifest: no `head`, `maintainers`,
  `object_format`, `tombstones`, `forked_from`, `recovery` or `new_root`, and
  refs restricted to the granted globs.
- `swarm accept <repo> <ref> <signer-hex>` resolves conflicts between signers
  (also M7).
- Sync already collects `Heads` from every signer and `resolve` already merges
  non-root refs, so the storage and resolution layers need little work.

### Acceptance

- A maintainer whose key is listed and whose refs match its globs can publish;
  the root can remove it and its refs disappear unless another signer lists
  them.
- A maintainer pushing outside its globs gets `out_of_scope`.
- A machine with only the maintainer key cannot add or remove maintainers,
  change `object_format`, or publish `head`.
- The two-peer demo can drop the key-copy step entirely; B keeps its own key.

### Affected code

- `crates/swarm-daemon/src/repo.rs` — root-only check in `publish`; non-root
  field and scope rules.
- `crates/swarm-cli/src/cli.rs` — `maintainers add|remove`, `accept`, and the
  maintainer-key generation command.
- `crates/swarm-core/src/accept.rs` — already has authorization, scope and
  forbidden-field checks; extend tests rather than logic where possible.
- `crates/swarm-core/src/resolve.rs` — already merges signers; add conflict
  coverage for the CLI path.
- `tests/e2e/m2.sh` / `scripts/peers-demo.sh` — replace the key copy with the
  maintainer flow.

### Open questions

- Name and shape of the maintainer-key command. The spec's CLI table only
  fixes `swarm maintainers add|remove`; generating the key can be a separate
  command, e.g. `swarm key new <repo> --maintainer`.
- How the public bundle is exchanged. A single base32 key ID is not enough to
  verify signatures; `add` likely needs the 1984-byte bundle, or the bundle can
  be carried in a small file whose key ID matches. The spec says
  `pubkey-base32`, so define a canonical base32 form for the full bundle.

---

## 2. Automatic push propagation

**Priority:** high  
**Spec:** 7.4 (`Announce`), 8.2 (sync loop), 8.3 (helper push), 6.4 (peer cache)

### Current behaviour

`git push` does not upload to the remote. The helper syncs from the remote,
moves refs in the **local** daemon store, and calls `publish`, which signs a new
manifest in that store. `publish` calls `NetState::announce`, but that only
reaches sessions currently registered on the publishing daemon. Sync sessions
are short-lived and outbound sessions are not registered, so a push from B
usually announces to nobody.

A's remote points at A itself, so a plain `git pull swarm main` cannot find B.
The guide has to make A fetch B's link explicitly.

### Proposed change

Make a successful publish actively notify the peers in the repo's cache and let
the receiver pull the new objects over the same session.

1. After `publish` writes a **new** manifest (not a no-op), spawn a best-effort
   background task that dials up to N cached peers for the repo from the
   `peers` table, using the existing handshake, pins and timeouts.
2. Send `Announce` with the new envelope on each session.
3. On the receiving side, when `Announce` is accepted, fetch the manifest's
   missing tips from the **announcing session** with `Want` (the session is
   bidirectional and the announcer already serves `Want`). Index the pack and
   apply canonical refs before the session closes.
4. Keep the existing rule that a duplicate announce is not forwarded. Do not
   loop: only forward to other healthy sessions that share the repo, and only
   when the announce was accepted as new.

Then A's normal `git pull swarm main` reads the objects and manifest that
already arrived in A's store; no B link and no manual fetch are needed.

### Acceptance

- With A and B running, B pushes and A's store `seq` advances without A running
  a command.
- `git pull swarm main` on A updates the worktree normally.
- If A is offline, the publish succeeds on B, the dial is best-effort, and A
  can still pull later from B's link or discovery.
- A bad announce is rejected exactly as before; a duplicate is not forwarded.
- Existing M2 e2e cases (wrong repo, kill-mid-pack, wrong pin, pcap) still pass.

### Affected code

- `crates/swarm-daemon/src/repo.rs` — after `publish`, trigger the background
  announce task.
- `crates/swarm-daemon/src/net/mod.rs` — a helper to dial cached peers and
  send `Announce`; reuse the candidate/dedupe logic from sync.
- `crates/swarm-daemon/src/net/serve.rs` — on `Announce` acceptance, fetch
  missing objects over the same session before returning.
- `crates/swarm-daemon/src/sync.rs` — factor the pack-fetch and index steps so
  the announce path can reuse them.
- `docs/USER-GUIDE.md` — remove the “pull from B's link” workaround once the
  propagation path is reliable.

### Open questions

- How many peers to notify per publish, and whether to wait briefly for one
  successful delivery before returning from `publish`.
- Whether the receiver should fetch eagerly on `Announce` or enqueue a normal
  `sync_repo` with the announcer as a candidate. Eager fetch keeps the session
  alive and avoids a second handshake; enqueueing reuses more code but needs a
  short-lived session registry.
- Interaction with the five-minute seeded sync loop and with M3/M4 discovery:
  the announce is still useful as a fast path, but discovery remains the source
  of truth for new peers.

---

## 3. Documentation and demo polish (low priority)

- Add a single `swarm peers <repo>` view that prints cached peer addresses with
  their node pins, so a user can see why a peer is or is not reachable.
- Have `git push` print the publisher's link and a one-line hint that other
  peers can pull from it, until automatic propagation lands.
- Add a `scripts/peers-reset.sh` helper as an alias for
  `docker compose -f compose.peers.yaml down -v`.
