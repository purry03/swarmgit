//! End-to-end tests for the three-phase handshake and session actor.

use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::RepoId;
use swarm_core::keyfile::NodeKey;
use swarm_net::handshake;
use swarm_net::{Message, NetError, PeerAddr, Session};

fn peer() -> PeerAddr {
    PeerAddr::Tcp("127.0.0.1:1".parse().unwrap())
}

#[tokio::test]
async fn both_sides_authenticate_and_share_a_repo() {
    let client_key = Arc::new(NodeKey::generate().unwrap());
    let server_key = Arc::new(NodeKey::generate().unwrap());
    let client_id = client_key.node_id().unwrap();
    let server_id = server_key.node_id().unwrap();
    let repo = RepoId::from_bytes([1u8; 32]);
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);

    let server = {
        let key = Arc::clone(&server_key);
        tokio::spawn(async move {
            handshake::accept(server_io, &key, &[repo], peer(), Duration::from_secs(10)).await
        })
    };
    let client = handshake::connect(
        client_io,
        &client_key,
        &[repo],
        None,
        peer(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let server = server.await.unwrap().unwrap();
    assert_eq!(client.info.node_id, server_id);
    assert_eq!(server.info.node_id, client_id);
    assert_eq!(client.info.shared, vec![(repo, 0)]);
    assert_eq!(server.info.shared, vec![(repo, 0)]);
    assert_ne!(client.info.handshake_hash, [0u8; 32]);

    let mut server_session = Session::start(server);
    let client_session = Session::start(client);
    let responder = tokio::spawn(async move {
        let request = server_session.recv_incoming().await.unwrap();
        let Message::GetHeads { rid, repo_ix: _ } = request else {
            panic!("unexpected request");
        };
        server_session
            .handle()
            .send(&Message::Heads {
                rid,
                envelopes: vec![vec![1, 2, 3]],
            })
            .await
            .unwrap();
    });
    let reply = client_session
        .handle()
        .request(
            Message::GetHeads { rid: 0, repo_ix: 0 },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(
        reply,
        Message::Heads {
            rid: 1,
            envelopes: vec![vec![1, 2, 3]]
        }
    );
    responder.await.unwrap();
}

#[tokio::test]
async fn a_wrong_pin_is_rejected() {
    let client_key = NodeKey::generate().unwrap();
    let server_key = Arc::new(NodeKey::generate().unwrap());
    let repo = RepoId::from_bytes([2u8; 32]);
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let server = {
        let key = Arc::clone(&server_key);
        tokio::spawn(async move {
            handshake::accept(server_io, &key, &[repo], peer(), Duration::from_secs(10)).await
        })
    };
    let wrong = swarm_core::ids::NodeId::from_bytes([9u8; 32]);
    let result = handshake::connect(
        client_io,
        &client_key,
        &[repo],
        Some(wrong),
        peer(),
        Duration::from_secs(10),
    )
    .await;
    assert!(matches!(result, Err(NetError::AuthFailed(_))));
    let _ = server.await;
}

#[tokio::test]
async fn a_repo_the_server_does_not_hold_ends_with_no_shared_repo() {
    let client_key = NodeKey::generate().unwrap();
    let server_key = Arc::new(NodeKey::generate().unwrap());
    let server_repo = RepoId::from_bytes([3u8; 32]);
    let wanted = RepoId::from_bytes([4u8; 32]);
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let server = {
        let key = Arc::clone(&server_key);
        tokio::spawn(async move {
            handshake::accept(
                server_io,
                &key,
                &[server_repo],
                peer(),
                Duration::from_secs(10),
            )
            .await
        })
    };
    let result = handshake::connect(
        client_io,
        &client_key,
        &[wanted],
        None,
        peer(),
        Duration::from_secs(10),
    )
    .await;
    assert!(matches!(result, Err(NetError::NoSharedRepo)));
    let _ = server.await;
}

/// SEC-18: a relay that terminates the connection cannot impersonate the
/// target. Pinning the real target's node id must reject the relay, while an
/// unpinned client can still talk to it.
#[tokio::test]
async fn a_relay_that_terminates_the_connection_fails_a_target_pin() {
    let client_key = NodeKey::generate().unwrap();
    let relay_key = Arc::new(NodeKey::generate().unwrap());
    let target_id = swarm_core::ids::NodeId::from_bytes([7u8; 32]);
    let repo = RepoId::from_bytes([5u8; 32]);
    let (client_io, relay_io) = tokio::io::duplex(256 * 1024);
    let relay = {
        let key = Arc::clone(&relay_key);
        tokio::spawn(async move {
            handshake::accept(relay_io, &key, &[repo], peer(), Duration::from_secs(10)).await
        })
    };
    let result = handshake::connect(
        client_io,
        &client_key,
        &[repo],
        Some(target_id),
        peer(),
        Duration::from_secs(10),
    )
    .await;
    assert!(matches!(result, Err(NetError::AuthFailed(_))));
    let _ = relay.await;

    let (client_io, relay_io) = tokio::io::duplex(256 * 1024);
    let relay = {
        let key = Arc::clone(&relay_key);
        tokio::spawn(async move {
            handshake::accept(relay_io, &key, &[repo], peer(), Duration::from_secs(10)).await
        })
    };
    let client = handshake::connect(
        client_io,
        &client_key,
        &[repo],
        None,
        peer(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let relay = relay.await.unwrap().unwrap();
    assert_eq!(client.info.node_id, relay_key.node_id().unwrap());
    assert_eq!(relay.info.node_id, client_key.node_id().unwrap());
}
