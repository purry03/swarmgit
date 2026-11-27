//! Carrying merge requests between peers. Each side lists its `refs/mr` tips;
//! whoever is behind asks for the objects with `Want`, checks every new event
//! and only then moves its ref. Anyone with the link can write events, so
//! everything here is bounded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::consts::{
    MAX_ABOUT_SKEW_SECS, MAX_MR_CHAIN, MAX_MR_PACK_BYTES, MAX_MR_REFS, MAX_MR_REFS_PER_AUTHOR,
};
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_core::mr::Kind;
use swarm_net::message::{MrTip, MR_CAP};
use swarm_net::{Message, NetError, SessionHandle};
use swarm_store::mr::{MrTips, Objects};
use swarm_store::RepoStore;

use super::dial::Connected;
use super::fetch::fetch_pack;
use super::progress::{Progress, Transfer};
use crate::net::NetState;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BUSY_RETRIES: u32 = 10;
const BUSY_WAIT: Duration = Duration::from_millis(300);
const MAX_HAVES: usize = 1024;

/// The refs held, in wire form.
pub(crate) fn wire_tips(tips: &MrTips) -> Vec<MrTip> {
    tips.iter()
        .filter_map(|((mr, author), tip)| {
            Some(MrTip {
                mr: Oid::new(hex::decode(mr).ok()?),
                author: *author.as_bytes(),
                tip: tip.clone(),
            })
        })
        .take(MAX_MR_REFS)
        .collect()
}

fn key_of(tip: &MrTip) -> (String, KeyId) {
    (tip.mr.to_hex(), KeyId::from_bytes(tip.author))
}

async fn is_ancestor(store: &RepoStore, older: &str, newer: &str) -> bool {
    store
        .git()
        .output(&["merge-base", "--is-ancestor", older, newer])
        .await
        .is_ok()
}

/// True when everything `oid` needs beyond what our refs already reach is here.
async fn is_complete(store: &RepoStore, oid: &str) -> bool {
    store
        .git()
        .output(&["rev-list", "--objects", "--quiet", oid, "--not", "--all"])
        .await
        .is_ok()
}

/// Take the events in `theirs` that we do not have from `peer`. Returns the
/// refs that moved. The caller holds the repo's sync lock.
pub(crate) async fn ingest(
    net: &NetState,
    repo: RepoId,
    peer: &SessionHandle,
    theirs: &[MrTip],
) -> Result<Vec<MrTip>, NetError> {
    let store = net.store(repo);
    let Some(repo_ix) = peer.info().repo_ix(repo) else {
        return Ok(Vec::new());
    };
    if !store.exists() {
        return Ok(Vec::new());
    }
    let failed = |err: swarm_store::StoreError| NetError::Malformed(err.to_string());
    let mine = store.mr_tips().await.map_err(failed)?;

    // Quotas count refs already held and new ones together.
    let mut total = mine.len();
    let mut per_author: BTreeMap<KeyId, usize> = BTreeMap::new();
    for (_, author) in mine.keys() {
        *per_author.entry(*author).or_default() += 1;
    }
    let mut wanted: Vec<&MrTip> = Vec::new();
    for tip in theirs.iter().take(MAX_MR_REFS) {
        let key = key_of(tip);
        let new_oid = tip.tip.to_hex();
        match mine.get(&key) {
            Some(held) if *held == tip.tip => continue,
            // They are behind us.
            Some(held) if is_ancestor(&store, &new_oid, &held.to_hex()).await => continue,
            Some(_) => {}
            None => {
                let count = per_author.entry(key.1).or_default();
                if total >= MAX_MR_REFS || *count >= MAX_MR_REFS_PER_AUTHOR {
                    continue;
                }
                *count += 1;
                total += 1;
            }
        }
        wanted.push(tip);
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    store.ensure_empty_tree().await.map_err(failed)?;
    let mut missing = Vec::new();
    for tip in &wanted {
        if !is_complete(&store, &tip.tip.to_hex()).await {
            missing.push(tip.tip.clone());
        }
    }
    if !missing.is_empty() {
        let mut haves: Vec<Oid> = mine.values().cloned().collect();
        let branches = store.read_refs().await.map_err(failed)?;
        haves.splice(0..0, branches.into_values());
        haves.truncate(MAX_HAVES);
        let max = net.max_pack_bytes.min(MAX_MR_PACK_BYTES);
        let mut tries = 0;
        let pack = loop {
            let result = fetch_pack(
                peer.clone(),
                repo_ix,
                missing.clone(),
                haves.clone(),
                max,
                Progress::none(),
                Transfer::new(),
            )
            .await;
            match result {
                // One `Want` per session at a time: wait for the other one.
                Err(NetError::Remote { ref code, .. })
                    if code == "busy" && tries < BUSY_RETRIES =>
                {
                    tries += 1;
                    tokio::time::sleep(BUSY_WAIT).await;
                }
                other => break other?,
            }
        };
        store
            .git()
            .index_pack_limited(&pack, max)
            .await
            .map_err(|err| NetError::PackInvalid(err.to_string()))?;
    }

    let mut accepted = Vec::new();
    // A comment can only be kept once the merge request it is on is, so go
    // round again for those listed before its opening.
    for _ in 0..2 {
        let mut objects = store.objects().map_err(failed)?;
        for tip in &wanted {
            let held = store.mr_tips().await.map_err(failed)?;
            let key = key_of(tip);
            let old = held.get(&key).map(Oid::to_hex);
            let new = tip.tip.to_hex();
            if old.as_deref() == Some(new.as_str()) {
                continue;
            }
            let known = held.keys().any(|(mr, _)| *mr == key.0);
            let checked = check_chain(&mut objects, repo, &key, &new, old.as_deref(), known)
                .await
                .map_err(failed)?;
            if !checked || !is_complete(&store, &new).await {
                continue;
            }
            match store.set_mr_ref(&key.0, key.1, &new, old.as_deref()).await {
                Ok(()) => accepted.push((*tip).clone()),
                Err(err) => tracing::debug!(error = %err, "merge request ref not moved"),
            }
        }
    }
    Ok(accepted)
}

/// Whether the chain ending at `tip` may replace `old` under `key`: every new
/// event is signed by the author for this merge request of this repo, and the
/// chain only ever grows. `known` says the merge request is already held.
async fn check_chain(
    objects: &mut Objects,
    repo: RepoId,
    key: &(String, KeyId),
    tip: &str,
    old: Option<&str>,
    known: bool,
) -> Result<bool, swarm_store::StoreError> {
    let (mr, author) = key;
    let limit = crate::net::now().max(0) as u64 + MAX_ABOUT_SKEW_SECS;
    let mut next = Some(tip.to_string());
    let mut opens = false;
    for _ in 0..=MAX_MR_CHAIN {
        if next.as_deref() == old && old.is_some() {
            return Ok(true);
        }
        let Some(oid) = next else {
            // Reached the start without meeting `old`: a rewritten chain.
            return Ok(old.is_none() && (opens || known));
        };
        let Some(event) = objects.event(&oid).await? else {
            return Ok(false);
        };
        let belongs = event.event.mr.as_deref().unwrap_or(&event.id) == mr;
        if event.signer != *author || event.event.repo != repo || !belongs || event.event.ts > limit
        {
            return Ok(false);
        }
        opens |= event.event.kind == Kind::Open;
        next = event.event.prev;
    }
    Ok(false)
}

/// Tell the sessions that share `repo` about refs they may not have.
pub(crate) async fn announce(net: &NetState, repo: RepoId, tips: &[MrTip]) {
    if tips.is_empty() {
        return;
    }
    for handle in net.sessions_for(repo) {
        send_announce(&handle, repo, tips.to_vec()).await;
    }
}

async fn send_announce(handle: &SessionHandle, repo: RepoId, tips: Vec<MrTip>) {
    let info = handle.info();
    let capable = info.caps.iter().any(|cap| cap == MR_CAP);
    let Some(repo_ix) = info.repo_ix(repo).filter(|_| capable) else {
        return;
    };
    let _ = handle.send(&Message::AnnounceMrs { repo_ix, tips }).await;
}

/// The refs a peer holds, or none when it cannot say.
pub(crate) async fn ask(handle: &SessionHandle, repo: RepoId) -> Option<Vec<MrTip>> {
    let info = handle.info();
    let capable = info.caps.iter().any(|cap| cap == MR_CAP);
    let repo_ix = info.repo_ix(repo).filter(|_| capable)?;
    match handle
        .request(Message::GetMrs { rid: 0, repo_ix }, REQUEST_TIMEOUT)
        .await
    {
        Ok(Message::Mrs { tips, .. }) => Some(tips),
        _ => None,
    }
}

/// Part of a sync: take what each peer has and tell it what it lacks. A peer
/// that fails here is not marked failed; the sync does not depend on it.
pub(crate) async fn exchange(net: &Arc<NetState>, repo: RepoId, peers: &[Connected]) {
    let store = net.store(repo);
    for peer in peers.iter().filter(|peer| !peer.failed) {
        let Some(theirs) = ask(&peer.handle, repo).await else {
            continue;
        };
        let accepted = match ingest(net, repo, &peer.handle, &theirs).await {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::debug!(error = %err, "merge requests from peer not taken");
                Vec::new()
            }
        };
        let Ok(mine) = store.mr_tips().await else {
            return;
        };
        let lacking: Vec<MrTip> = wire_tips(&mine)
            .into_iter()
            .filter(|tip| !theirs.contains(tip))
            .collect();
        if !lacking.is_empty() {
            send_announce(&peer.handle, repo, lacking).await;
        }
        announce(net, repo, &accepted).await;
    }
}
