//! Repo rendezvous topics (relay design spec milestone R6): a node that cannot
//! accept connections tells the relays a topic belongs on that it serves it,
//! and a dialer asks them which nodes do. A relay learns a hash of the repo ID
//! that changes daily, like the DHT topic. It can still see that the same nodes
//! publish yesterday's and today's: rotation stops an old topic from naming
//! holders and moves it between discovered relays, no more than that.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use swarm_core::consts::CTX_TOPIC;

use crate::proto::MAX_HOLDERS;

/// How long a publication lasts.
pub(crate) const TOPIC_TTL: Duration = Duration::from_secs(60 * 60);
/// Topics held at once.
const MAX_TOPICS: usize = 20_000;

/// The topic of a repo on `day`, days since the Unix epoch: its ID and the day
/// hashed under a fixed key.
#[must_use]
pub fn topic(repo: &[u8; 32], day: u64) -> [u8; 32] {
    let mut input = [0; 40];
    input[..32].copy_from_slice(repo);
    input[32..].copy_from_slice(&day.to_be_bytes());
    *blake3::keyed_hash(&blake3::derive_key(CTX_TOPIC, b""), &input).as_bytes()
}

type Holders = HashMap<[u8; 32], Instant>;

/// Who serves which topic. Held in memory only (REL-7).
#[derive(Default)]
pub(crate) struct Topics(Mutex<HashMap<[u8; 32], Holders>>);

impl Topics {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Holders>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record that `node` serves `topics`. A full topic drops its oldest
    /// holder; a full relay takes no new topic.
    pub(crate) fn publish(&self, node: [u8; 32], topics: &[[u8; 32]]) {
        let now = Instant::now();
        let mut held = self.lock();
        if held.len() + topics.len() > MAX_TOPICS {
            held.retain(|_, holders| {
                holders.retain(|_, until| *until > now);
                !holders.is_empty()
            });
        }
        for topic in topics {
            if held.len() >= MAX_TOPICS && !held.contains_key(topic) {
                continue;
            }
            let holders = held.entry(*topic).or_default();
            holders.retain(|_, until| *until > now);
            if holders.len() >= MAX_HOLDERS && !holders.contains_key(&node) {
                let oldest = holders.iter().min_by_key(|(_, until)| **until);
                if let Some(oldest) = oldest.map(|(node, _)| *node) {
                    holders.remove(&oldest);
                }
            }
            holders.insert(node, now + TOPIC_TTL);
        }
    }

    /// The nodes serving `topic`, most recently published first.
    pub(crate) fn holders(&self, topic: &[u8; 32]) -> Vec<[u8; 32]> {
        let now = Instant::now();
        let held = self.lock();
        let mut found: Vec<_> = held
            .get(topic)
            .into_iter()
            .flatten()
            .filter(|(_, until)| **until > now)
            .map(|(node, until)| (*until, *node))
            .collect();
        found.sort_unstable_by(|a, b| b.cmp(a));
        found.into_iter().map(|(_, node)| node).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topic_hides_the_repo_and_keeps_the_newest_holders() {
        let repo = [7; 32];
        assert_ne!(topic(&repo, 9), repo);
        assert_eq!(topic(&repo, 9), topic(&repo, 9));
        assert_ne!(topic(&repo, 9), topic(&repo, 10));
        assert_ne!(topic(&repo, 9), topic(&[8; 32], 9));

        let topics = Topics::default();
        for n in 0..=MAX_HOLDERS as u8 {
            topics.publish([n; 32], &[topic(&repo, 9)]);
        }
        let holders = topics.holders(&topic(&repo, 9));
        assert_eq!(holders.len(), MAX_HOLDERS);
        assert!(holders.contains(&[MAX_HOLDERS as u8; 32]));
        assert!(!holders.contains(&[0; 32]), "the oldest makes room");
        assert!(topics.holders(&[0; 32]).is_empty());
    }
}
