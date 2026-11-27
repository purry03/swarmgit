//! Proof of node ownership (REL-1), and the publish session that shares it
//! with listening.

use std::net::SocketAddr;

use std::sync::atomic::Ordering;
use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::{node_id, random_bytes, relay_auth_input};
use swarm_core::pq;

use crate::proto::Msg;
use crate::secure::Channel;
use crate::server::{send_error, Relay};
use crate::topics::TOPIC_TTL;
use crate::RelayError;

impl Relay {
    /// Challenge the client and return the node ID it proved to hold.
    pub(crate) async fn prove(&self, stream: &mut Channel) -> Result<[u8; 32], RelayError> {
        let limits = &self.state.config.limits;
        let nonce = random_bytes::<32>().map_err(|err| RelayError::Auth(err.to_string()))?;
        stream.send(&Msg::Challenge { nonce }).await?;
        let proof = tokio::time::timeout(limits.hello_timeout, stream.recv())
            .await
            .map_err(|_| RelayError::Timeout("prove"))??;
        let Msg::Prove { bundle, sig, name } = proof else {
            send_error(stream, "malformed", "expected Prove").await;
            return Err(RelayError::Protocol("expected Prove".into()));
        };
        let names = &self.state.config.names;
        // A relay found by discovery is dialed by IP, so the address this
        // connection arrived on is also one of this relay's names.
        let arrived_on = stream
            .local_addr()
            .ok()
            .map(|addr| SocketAddr::new(addr.ip().to_canonical(), addr.port()).to_string());
        let name_ok = names.is_empty() || names.contains(&name) || arrived_on == Some(name.clone());
        let signature_ok = name_ok
            && bundle.len() == swarm_core::consts::SUITE1_BUNDLE_LEN
            && sig.len() == swarm_core::consts::SUITE1_SIG_LEN
            && pq::verify(
                SUITE_HYBRID,
                &bundle,
                &relay_auth_input(&nonce, &name),
                &sig,
            )
            .is_ok();
        if !signature_ok {
            self.state
                .counters
                .auth_failures
                .fetch_add(1, Ordering::SeqCst);
            send_error(stream, "auth_failed", "proof rejected").await;
            return Err(RelayError::Auth("proof rejected".into()));
        }
        Ok(*node_id(&bundle).as_bytes())
    }

    /// Take one list of rendezvous topics from a proven node.
    pub(crate) async fn publish_session(
        &self,
        mut stream: Channel,
        peer: SocketAddr,
    ) -> Result<(), RelayError> {
        let limits = &self.state.config.limits;
        if let Some(ip) = self.counted(peer) {
            if !self.state.quotas.connect(ip, limits.connects_per_minute) {
                send_error(&mut stream, "limit_exceeded", "too many requests").await;
                return Err(RelayError::Protocol("publish rate".into()));
            }
        }
        let node = self.prove(&mut stream).await?;
        let allowed = &self.state.config.allow_nodes;
        if !allowed.is_empty() && !allowed.contains(&node) {
            send_error(&mut stream, "auth_failed", "node is not allowed here").await;
            return Err(RelayError::Auth("node not on the allow list".into()));
        }
        let request = tokio::time::timeout(limits.hello_timeout, stream.recv())
            .await
            .map_err(|_| RelayError::Timeout("publish"))??;
        let Msg::Publish { topics } = request else {
            send_error(&mut stream, "malformed", "expected Publish").await;
            return Err(RelayError::Protocol("expected Publish".into()));
        };
        self.state.topics.publish(node, &topics);
        stream
            .send(&Msg::Published {
                ttl_secs: TOPIC_TTL.as_secs(),
            })
            .await
    }
}
