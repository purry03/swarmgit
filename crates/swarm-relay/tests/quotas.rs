//! Relay limits and access control (REL-19, REL-21, REL-22) over loopback.

use std::sync::Arc;
use std::time::{Duration, Instant};

use swarm_core::keyfile::NodeKey;
use swarm_relay::{Limits, Relay, RelayConfig, RelayError, RelayListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn start(config: RelayConfig) -> (String, Relay) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let relay = Relay::new(config);
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (addr, relay)
}

fn limited(limits: Limits) -> RelayConfig {
    RelayConfig {
        limits: Limits {
            limit_loopback: true,
            ..limits
        },
        ..RelayConfig::default()
    }
}

fn key() -> Arc<NodeKey> {
    Arc::new(NodeKey::generate().unwrap())
}

fn code<T>(result: Result<T, RelayError>) -> String {
    match result {
        Ok(_) => "ok".into(),
        Err(err) => err.code().unwrap_or("other").to_string(),
    }
}

/// Dial the registered node and return both ends of the pipe.
async fn pipe(addr: &str, key: &NodeKey, listener: &mut RelayListener) -> (TcpStream, TcpStream) {
    let id = key.node_id().unwrap();
    let (dialed, accepted) = tokio::join!(swarm_relay::connect(addr, id), listener.accept());
    (dialed.unwrap(), accepted.unwrap())
}

#[tokio::test]
async fn one_address_holds_two_registrations() {
    let (addr, _relay) = start(limited(Limits::default())).await;
    let first = key();
    let _a = swarm_relay::listen(&addr, first.clone()).await.unwrap();
    let _b = swarm_relay::listen(&addr, key()).await.unwrap();
    assert_eq!(
        code(swarm_relay::listen(&addr, key()).await),
        "limit_exceeded"
    );
    // The same node coming back is not a new registration.
    assert_eq!(code(swarm_relay::listen(&addr, first).await), "ok");
}

#[tokio::test]
async fn connects_are_rate_limited_even_for_unknown_nodes() {
    let (addr, relay) = start(limited(Limits::default())).await;
    let ghost = key().node_id().unwrap();
    let mut codes = Vec::new();
    for _ in 0..1000 {
        codes.push(code(swarm_relay::connect(&addr, ghost).await));
    }
    assert!(codes[..20].iter().all(|code| code == "no_such_node"));
    assert!(codes[20..].iter().all(|code| code == "limit_exceeded"));
    assert_eq!(relay.stats().splices_total, 0);
}

#[tokio::test]
async fn one_address_has_a_cap_on_open_pipes() {
    let (addr, _relay) = start(limited(Limits {
        splices_per_ip: 1,
        ..Limits::default()
    }))
    .await;
    let node = key();
    let mut listener = swarm_relay::listen(&addr, node.clone()).await.unwrap();
    let open = pipe(&addr, &node, &mut listener).await;
    let id = node.node_id().unwrap();
    assert_eq!(
        code(swarm_relay::connect(&addr, id).await),
        "limit_exceeded"
    );
    drop(open);
}

#[tokio::test]
async fn a_node_over_its_daily_quota_is_cut_off() {
    let (addr, _relay) = start(limited(Limits {
        node_daily_bytes: 32 * 1024,
        ..Limits::default()
    }))
    .await;
    let node = key();
    let mut listener = swarm_relay::listen(&addr, node.clone()).await.unwrap();
    let (mut near, mut far) = pipe(&addr, &node, &mut listener).await;
    let sender = tokio::spawn(async move {
        let _ = near.write_all(&vec![7u8; 256 * 1024]).await;
        near
    });
    let mut got = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), far.read_to_end(&mut got))
        .await
        .expect("the pipe closes");
    assert!(got.len() < 128 * 1024, "{} bytes got through", got.len());
    drop(sender.await);
    let id = node.node_id().unwrap();
    assert_eq!(
        code(swarm_relay::connect(&addr, id).await),
        "limit_exceeded"
    );
}

#[tokio::test]
async fn a_splice_is_paced() {
    let (addr, _relay) = start(limited(Limits {
        splice_bytes_per_second: 256 * 1024,
        ..Limits::default()
    }))
    .await;
    let node = key();
    let mut listener = swarm_relay::listen(&addr, node.clone()).await.unwrap();
    let (mut near, mut far) = pipe(&addr, &node, &mut listener).await;
    let started = Instant::now();
    let sender = tokio::spawn(async move {
        near.write_all(&vec![7u8; 192 * 1024]).await.unwrap();
        near
    });
    let mut got = vec![0u8; 192 * 1024];
    far.read_exact(&mut got).await.unwrap();
    // 192 KiB at 256 KiB/s takes most of a second.
    assert!(started.elapsed() > Duration::from_millis(500));
    drop(sender.await);
}

#[tokio::test]
async fn a_password_guards_listening_and_dialing() {
    let relay_key = swarm_relay::RelayKey::generate().unwrap();
    let public = relay_key.public();
    let (plain, relay) = start(RelayConfig {
        password: Some("correct horse".into()),
        key: Some(relay_key),
        ..RelayConfig::default()
    })
    .await;
    let addr = format!("{plain}#{}", swarm_relay::key_text(public));
    let node = key();
    assert_eq!(
        code(swarm_relay::listen(&addr, node.clone()).await),
        "auth_failed"
    );
    let wrong = swarm_relay::listen_with(&addr, node.clone(), Some("battery".into())).await;
    assert_eq!(code(wrong), "auth_failed");
    let mut listener = swarm_relay::listen_with(&addr, node.clone(), Some("correct horse".into()))
        .await
        .unwrap();
    let id = node.node_id().unwrap();
    assert_eq!(code(swarm_relay::connect(&addr, id).await), "auth_failed");
    let (dialed, accepted) = tokio::join!(
        swarm_relay::connect_with(&addr, id, Some("correct horse")),
        listener.accept()
    );
    assert!(dialed.is_ok() && accepted.is_some());
    assert_eq!(relay.stats().auth_failures, 3);
    // Without the relay's key the password stays here, and the error says why.
    let err = swarm_relay::connect_with(&plain, id, Some("correct horse"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("host:port#key"), "{err}");
}

#[tokio::test]
async fn the_allow_list_limits_who_registers() {
    let friend = key();
    let (addr, _relay) = start(RelayConfig {
        allow_nodes: vec![*friend.node_id().unwrap().as_bytes()],
        ..RelayConfig::default()
    })
    .await;
    assert_eq!(code(swarm_relay::listen(&addr, key()).await), "auth_failed");
    assert_eq!(code(swarm_relay::listen(&addr, friend).await), "ok");
}

#[tokio::test]
async fn metrics_are_counters_only() {
    let (_addr, relay) = start(RelayConfig::default()).await;
    let metrics = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = metrics.local_addr().unwrap();
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve_metrics(metrics).await });
    let mut stream = TcpStream::connect(at).await.unwrap();
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.contains("swarm_relay_listeners 0"));
    for line in reply.split("\r\n\r\n").nth(1).unwrap().lines() {
        let words: Vec<&str> = line.split(' ').collect();
        assert!(line.starts_with('#') || (words.len() == 2 && words[1].parse::<u64>().is_ok()));
    }
}
