//! Route C in the daemon (6.4): start the DHT, announce served repos, keep
//! the head pointer fresh and persist the nodes that answered.
//!
//! Nothing here reveals a repo ID: topics and the head salt are keyed
//! derivations of it (DHT-5), and only the node's own port is announced.

use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::{KeyId, RepoId};
use swarm_disc::dht::{self, DhtConfig, Head, MainlineDht};

use super::{now, NetState};

/// DHT-2: re-announce served repos this often.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(15 * 60);
/// DHT-5: refresh head pointers this often (items expire after ~2 h).
const HEAD_EVERY: Duration = Duration::from_secs(60 * 60);
/// DHT-3: save responding nodes this often.
const PERSIST_EVERY: Duration = Duration::from_secs(10 * 60);
/// Nodes kept in `dht_nodes`.
const KEEP_NODES: usize = 64;

/// Start route C unless `SWARM_NO_DHT=1`. A failure leaves the other routes
/// working (DHT-4).
pub async fn start(net: &Arc<NetState>) {
    if dht::disabled() {
        tracing::info!("dht disabled by SWARM_NO_DHT");
        return;
    }
    let persisted = net
        .db
        .lock()
        .ok()
        .and_then(|db| db.dht_nodes(KEEP_NODES).ok())
        .unwrap_or_default();
    let config = DhtConfig::from_env(persisted);
    let started = tokio::task::spawn_blocking(move || MainlineDht::start(&config)).await;
    let dht = match started {
        Ok(Ok(dht)) => dht,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "dht unavailable");
            return;
        }
        Err(err) => {
            tracing::warn!(error = %err, "dht start panicked");
            return;
        }
    };
    let local = dht.local_addr().await;
    tracing::info!(local = ?local, "dht started");
    net.set_dht(dht);
    spawn_loops(net);
}

fn spawn_loops(net: &Arc<NetState>) {
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(ANNOUNCE_EVERY);
        loop {
            tick.tick().await;
            let Some(net) = weak.upgrade() else { break };
            for repo in net.serving_repos() {
                net.announce_dht(repo);
            }
        }
    });
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(HEAD_EVERY);
        loop {
            tick.tick().await;
            let Some(net) = weak.upgrade() else { break };
            for repo in net.serving_repos() {
                net.refresh_head(repo);
            }
        }
    });
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PERSIST_EVERY);
        // The first tick fires at once, before any node answered.
        tick.tick().await;
        loop {
            tick.tick().await;
            let Some(net) = weak.upgrade() else { break };
            net.persist_dht_nodes().await;
        }
    });
}

impl NetState {
    /// Announce `repo` on the DHT now (DHT-2), if it is served.
    pub fn announce_dht(&self, repo: RepoId) {
        let Some(dht) = self.dht() else { return };
        if !self.serving_repos().contains(&repo) {
            return;
        }
        let port = self.listen.port();
        tokio::spawn(async move {
            let done = dht.announce_repo(repo, port).await;
            tracing::debug!(announced = done, "dht announce");
        });
    }

    /// Store the head pointer for a manifest we signed (DHT-5).
    pub fn put_head(&self, repo: RepoId, ed_seed: [u8; 32], manifest_id: [u8; 32], seq: u64) {
        let Some(dht) = self.dht() else { return };
        tokio::spawn(async move {
            let ok = dht::Dht::put_head(&dht, ed_seed, repo, Head { manifest_id, seq }).await;
            tracing::debug!(ok, seq, "dht head put");
        });
    }

    /// Re-put the head pointer for a repo whose root key we hold.
    fn refresh_head(&self, repo: RepoId) {
        let store = self.store(repo);
        let Ok(Some(key)) = store.load_key() else {
            return;
        };
        let Some(seed) = key.ed_seed() else { return };
        let Ok(state) = store.load_state() else {
            return;
        };
        // The pointer belongs to the original root key; after a rotation it
        // is no longer published (DHT-5).
        if state.root != KeyId::from_bytes(*repo.as_bytes()) {
            return;
        }
        let Some(root) = state.root_manifest() else {
            return;
        };
        self.put_head(repo, seed, root.manifest_id, root.manifest.seq);
    }

    /// The newest head pointer for a repo whose root manifest we have.
    pub async fn dht_head(&self, repo: RepoId, signer_keys: &[u8]) -> Option<Head> {
        let dht = self.dht()?;
        let key: [u8; 32] = signer_keys.get(..32)?.try_into().ok()?;
        dht::Dht::get_head(&dht, key, repo).await
    }

    /// DHT-3: remember the nodes that answered for the next start.
    pub async fn persist_dht_nodes(&self) {
        let Some(dht) = self.dht() else { return };
        let nodes = dht.known_nodes().await;
        if nodes.is_empty() {
            return;
        }
        if let Ok(db) = self.db.lock() {
            if let Err(err) = db.save_dht_nodes(&nodes, now(), KEEP_NODES) {
                tracing::debug!(error = %err, "saving dht nodes failed");
            }
        }
    }
}

/// Look up peers for `repo` (DHT-1); empty when the route is off.
pub async fn find(dht: Option<MainlineDht>, repo: RepoId) -> Vec<std::net::SocketAddr> {
    match dht {
        Some(dht) => dht.find(repo).await,
        None => Vec::new(),
    }
}
