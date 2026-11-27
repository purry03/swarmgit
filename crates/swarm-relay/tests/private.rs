//! Pinned relay keys (R5) and rendezvous topics (R6).

use std::sync::Arc;

use swarm_core::keyfile::NodeKey;
use swarm_relay::{Relay, RelayConfig};
use tokio::net::TcpListener;

fn key() -> Arc<NodeKey> {
    Arc::new(NodeKey::generate().unwrap())
}

async fn start() -> (Relay, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let relay = Relay::new(RelayConfig::default());
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (relay, addr)
}

#[tokio::test]
async fn a_pinned_relay_key_must_match() {
    let h = start().await;
    let id = h.0.relay_id();
    let b = key();
    // The proof is bound to the address without the pin (REL-1).
    let pinned = format!("{}#{}", h.1, swarm_relay::key_text(id));
    let _listener = swarm_relay::listen(&pinned, b.clone()).await.unwrap();
    assert_eq!(h.0.stats().listeners, 1);

    let wrong = swarm_relay::RelayKey::generate().unwrap().public();
    let wrong = format!("{}#{}", h.1, swarm_relay::key_text(wrong));
    let err = swarm_relay::connect(&wrong, b.node_id().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("auth_failed"), "{err}");
}

#[tokio::test]
async fn published_topics_name_the_proven_node() {
    let h = start().await;
    let b = key();
    let topic = swarm_relay::topic(&[3; 32]);
    assert!(swarm_relay::lookup(&h.1, topic).await.unwrap().is_empty());
    swarm_relay::publish(&h.1, &b, &[topic], None)
        .await
        .unwrap();
    assert_eq!(
        swarm_relay::lookup(&h.1, topic).await.unwrap(),
        vec![b.node_id().unwrap()]
    );
    // Publishing is not registering.
    assert_eq!(h.0.stats().listeners, 0);
}

#[tokio::test]
async fn connecting_to_an_unknown_node_is_no_such_node() {
    let h = start().await;
    let ghost = key().node_id().unwrap();
    let err = swarm_relay::connect(&h.1, ghost).await.unwrap_err();
    assert_eq!(err.code(), Some("no_such_node"));
}
