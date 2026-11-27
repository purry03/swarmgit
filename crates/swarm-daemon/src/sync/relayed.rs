//! The relay route (relay design spec REL-29, R6): when no direct dial worked,
//! find the relays that hold a node known only by ID and dial through them.

use std::sync::Arc;

use swarm_core::ids::{NodeId, RepoId};
use swarm_net::addr::PeerAddr;
use swarm_net::proxy::Endpoint;

use super::dial::{Candidate, Dialer, MAX_SESSIONS, SURVEY_SESSIONS};
use super::progress::Progress;
use crate::net::relay::{address_of, configured, mode, Mode};
use crate::net::{topics, NetState};

/// Nodes looked up in one sync.
const MAX_NODES: usize = 4;

/// Find ways through relays to `nodes`: where discovery says each is
/// registered, and the configured relays.
async fn routes(net: &NetState, nodes: &[NodeId]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for &node in nodes {
        let mut relays = match net.discovery() {
            Some(discovery) => discovery.locate(node).await,
            None => Vec::new(),
        };
        for name in configured() {
            if let Some(relay) = Endpoint::parse(address_of(&name)) {
                if !relays.contains(&relay) {
                    relays.push(relay);
                }
            }
        }
        found.extend(relays.into_iter().map(|relay| Candidate {
            addr: PeerAddr::Relay { relay, node },
            node: Some(node),
        }));
    }
    found
}

/// Dial the pinned nodes among `candidates` through relays, then whoever
/// published the repo's topic (R6). The topic is only looked up when that
/// reached nobody, unless `every` asks for all the peers there are.
pub async fn dial(
    net: &Arc<NetState>,
    repo: RepoId,
    dialer: &mut Dialer,
    candidates: &[Candidate],
    progress: &Progress,
    every: bool,
) {
    if mode() == Mode::Off || (net.discovery().is_none() && configured().is_empty()) {
        return;
    }
    let limit = if every { SURVEY_SESSIONS } else { MAX_SESSIONS };
    let mut nodes: Vec<NodeId> = Vec::new();
    for node in candidates.iter().filter_map(|candidate| candidate.node) {
        if node != net.node_id && !nodes.contains(&node) && nodes.len() < MAX_NODES {
            nodes.push(node);
        }
    }
    if dialer.connected.is_empty() {
        progress.say("relay", "no direct connection; looking for relays");
    } else {
        progress.say("relay", "asking relays who else holds the repo");
    }
    if !nodes.is_empty() {
        let found = routes(net, &nodes).await;
        progress.say("relay", format!("{} way(s) to known peers", found.len()));
        dialer.dial_all(net, repo, found, limit).await;
    }
    if every || dialer.connected.is_empty() {
        let mut holders = topics::holders(net, repo, MAX_NODES).await;
        holders.retain(|node| !nodes.contains(node));
        if !holders.is_empty() {
            progress.say(
                "relay",
                format!("relays name {} holder(s) of the repo", holders.len()),
            );
            let found = routes(net, &holders).await;
            dialer.dial_all(net, repo, found, limit).await;
        }
    }
}
