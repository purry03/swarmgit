//! Publishing and finding repo topics on relays (relay design spec R6), so a
//! seeder that cannot accept connections is found from the repo ID alone.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use swarm_core::ids::{NodeId, RepoId};

use super::relay::{address_of, configured, mode, Mode};
use super::NetState;

/// How often the daemon looks for something new to publish.
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// A publication is renewed this often, well inside its lifetime.
const RENEW_EVERY: Duration = Duration::from_secs(20 * 60);
/// A publication some relay refused or missed is tried again this soon.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The relays to use for `topic`: its home relays and the configured ones.
/// Each comes with what to dial and the password for it.
async fn relays_for(net: &NetState, topic: [u8; 32]) -> Vec<(String, Option<String>)> {
    let mut relays: Vec<(String, Option<String>)> = configured()
        .into_iter()
        .map(|name| {
            let password = net.relay_password_for(&name);
            (name, password)
        })
        .collect();
    if let Some(discovery) = net.discovery() {
        for addr in discovery.topic_homes(topic).await {
            let name = addr.to_string();
            if !relays.iter().any(|(known, _)| address_of(known) == name) {
                relays.push((name, None));
            }
        }
    }
    relays
}

/// Publish a topic for every repo served, on the relays it belongs on. False
/// when a relay did not take them.
async fn publish_all(net: &NetState, repos: &[RepoId]) -> bool {
    let mut all = true;
    let mut batches: BTreeMap<(String, Option<String>), Vec<[u8; 32]>> = BTreeMap::new();
    for repo in repos {
        let topic = swarm_relay::topic(repo.as_bytes());
        for relay in relays_for(net, topic).await {
            batches.entry(relay).or_default().push(topic);
        }
    }
    for ((relay, password), topics) in batches {
        for chunk in topics.chunks(swarm_relay::proto::MAX_TOPICS_FRAME) {
            if let Err(err) =
                swarm_relay::publish(&relay, &net.node, chunk, password.as_deref()).await
            {
                tracing::debug!(relay = %relay, error = %err, "cannot publish topics");
                all = false;
                break;
            }
        }
    }
    all
}

/// Keep the topics of served repos published for as long as this node relies
/// on relays to be reached.
pub fn start(net: &Arc<NetState>) {
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        // What was published, after how many registrations, and until when
        // that holds.
        let mut published: Option<(Vec<RepoId>, u64, Instant)> = None;
        loop {
            tokio::time::sleep(LOOK_EVERY).await;
            let Some(net) = weak.upgrade() else { break };
            if mode() == Mode::Auto && net.directly_reachable() {
                continue;
            }
            // A holder nobody can reach through a relay is no use.
            let registered = net.relay_statuses().iter().any(|status| status.registered);
            let mut repos = net.serving_repos();
            repos.sort_unstable_by_key(|repo| *repo.as_bytes());
            if !registered || repos.is_empty() {
                continue;
            }
            // A relay holds topics in memory only: after any new registration,
            // such as with a restarted relay, they are published again.
            let registrations = net.registrations();
            let current = published.as_ref().is_some_and(|(done, at, until)| {
                *done == repos && *at == registrations && Instant::now() < *until
            });
            if !current {
                let renew = RENEW_EVERY.min(super::discovery::round());
                let holds = if publish_all(&net, &repos).await {
                    renew
                } else {
                    renew.min(RETRY_AFTER)
                };
                published = Some((repos, registrations, Instant::now() + holds));
            }
        }
    });
}

/// The nodes that published `repo` on the relays its topic belongs on.
pub async fn holders(net: &NetState, repo: RepoId, max: usize) -> Vec<NodeId> {
    let topic = swarm_relay::topic(repo.as_bytes());
    let mut nodes = Vec::new();
    for (relay, _) in relays_for(net, topic).await {
        match swarm_relay::lookup(&relay, topic).await {
            Ok(found) => {
                for node in found {
                    if node != net.node_id && !nodes.contains(&node) && nodes.len() < max {
                        nodes.push(node);
                    }
                }
            }
            Err(err) => tracing::debug!(relay = %relay, error = %err, "topic lookup failed"),
        }
    }
    nodes
}
