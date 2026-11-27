//! A relay as a member of the relay network (REL-23, REL-25, REL-26, REL-33):
//! find other relays, gossip with them, and announce this one in the DHT once
//! another relay has shown it can be reached.

use std::sync::Arc;
use std::time::Duration;

use swarm_disc::dht::{Dht, MainlineDht};
use tokio::time::Instant;

use crate::client;
use crate::directory::{directory_topic, Directory, DirectoryConfig};
use crate::server::{stopped, Relay};

/// Relays asked to dial back in one attempt.
const CHECKERS: usize = 3;

/// How a relay takes part in the relay network.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// The port others reach this relay on.
    pub port: u16,
    /// Skip the reachability check and announce at once (`--public`).
    pub public: bool,
    /// Gossip and re-check interval (REL-26).
    pub round: Duration,
    /// DHT announce interval (REL-23).
    pub announce_every: Duration,
    /// Interval while no relay is known or reachability is unproven.
    pub retry: Duration,
    /// Directory settings.
    pub directory: DirectoryConfig,
}

impl NetworkConfig {
    /// The defaults for a relay listening on `port`.
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self {
            port,
            public: false,
            round: Duration::from_secs(30 * 60),
            announce_every: Duration::from_secs(15 * 60),
            retry: Duration::from_secs(60),
            directory: DirectoryConfig::default(),
        }
    }
}

impl Relay {
    /// Join the relay network. Runs until [`Relay::shutdown`]. Joining a
    /// second time returns the first directory.
    pub fn join_network(&self, dht: Option<MainlineDht>, config: NetworkConfig) -> Arc<Directory> {
        let directory = Directory::new(dht, Some(self.relay_id()), config.directory.clone());
        if self.state.network.set(Arc::clone(&directory)).is_err() {
            return self.state.network.get().cloned().unwrap_or(directory);
        }
        let relay = self.clone();
        let member = Arc::clone(&directory);
        let mut stop = self.stop_signal();
        tokio::spawn(async move {
            tokio::select! {
                () = stopped(&mut stop) => {}
                () = relay.member_loop(member, config) => {}
            }
        });
        directory
    }

    async fn member_loop(&self, directory: Arc<Directory>, config: NetworkConfig) {
        let mut reachable = config.public;
        let mut next_round = Instant::now();
        let mut next_announce = Instant::now();
        loop {
            if Instant::now() >= next_round {
                directory.refresh(&[]).await;
                if !reachable {
                    reachable = self.check_reachability(&directory, config.port).await;
                    if reachable {
                        tracing::info!("reachable from outside: announcing this relay");
                        next_announce = Instant::now();
                    }
                }
                let settled = reachable && !directory.is_empty();
                let wait = if settled { config.round } else { config.retry };
                next_round = Instant::now() + wait;
                tracing::debug!(relays = directory.len(), reachable, "relay network round");
            }
            if reachable && Instant::now() >= next_announce {
                if let Some(dht) = directory.dht() {
                    let done = dht.announce(directory_topic(), config.port).await;
                    tracing::debug!(done, "relay directory announce");
                    let wait = if done {
                        config.announce_every
                    } else {
                        config.retry
                    };
                    next_announce = Instant::now() + wait;
                } else {
                    next_announce = Instant::now() + config.announce_every;
                }
            }
            let wake = if reachable {
                next_round.min(next_announce)
            } else {
                next_round
            };
            tokio::time::sleep_until(wake).await;
        }
    }

    /// REL-25: ask other relays to dial us back.
    async fn check_reachability(&self, directory: &Directory, port: u16) -> bool {
        for checker in directory.sample(CHECKERS) {
            let name = checker.to_string();
            let ask = client::dial_back(&name, port, self.relay_id());
            let wait = directory.config().probe_timeout * 2;
            if matches!(tokio::time::timeout(wait, ask).await, Ok(Ok(true))) {
                return true;
            }
        }
        false
    }
}
