//! Relay acceptance tests (design spec milestone R1).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::crypto::relay_auth_input;
use swarm_core::keyfile::NodeKey;
use swarm_core::pq::Signer;
use swarm_net::{handshake, NetError, PeerAddr};
use swarm_relay::proto::{read_msg, write_msg, Msg, Role, MAX_FRAME, VERSION};
use swarm_relay::{Limits, ListenState, Relay, RelayConfig, RelayError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const T: Duration = Duration::from_secs(20);

fn shared() -> [swarm_core::ids::RepoId; 1] {
    [swarm_core::ids::RepoId::from_bytes([1; 32])]
}

fn placeholder() -> PeerAddr {
    PeerAddr::Tcp("0.0.0.0:0".parse::<SocketAddr>().unwrap())
}

struct Harness {
    addr: String,
    relay: Relay,
    task: JoinHandle<()>,
}

async fn start(limits: Limits, bind_names: bool) -> Harness {
    start_with(
        RelayConfig {
            limits,
            ..RelayConfig::default()
        },
        bind_names,
    )
    .await
}

async fn start_with(mut config: RelayConfig, bind_names: bool) -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    if bind_names {
        config.names = vec![addr.clone()];
    }
    let relay = Relay::new(config);
    let task = {
        let relay = relay.clone();
        tokio::spawn(async move { relay.serve(listener).await })
    };
    Harness { addr, relay, task }
}

fn key() -> Arc<NodeKey> {
    Arc::new(NodeKey::generate().unwrap())
}

/// Register by hand and return the control connection.
async fn raw_listen(addr: &str, key: &NodeKey, name: &str) -> Result<TcpStream, RelayError> {
    let mut stream = TcpStream::connect(addr).await?;
    write_msg(
        &mut stream,
        &Msg::Hello {
            version: VERSION,
            role: Role::Listen,
        },
    )
    .await?;
    let Msg::Challenge { nonce } = read_msg(&mut stream).await? else {
        panic!("expected challenge");
    };
    let sig = key
        .signer()
        .unwrap()
        .try_sign(&relay_auth_input(&nonce, name))
        .unwrap();
    write_msg(
        &mut stream,
        &Msg::Prove {
            bundle: key.bundle().unwrap(),
            sig,
            name: name.to_string(),
        },
    )
    .await?;
    match read_msg(&mut stream).await? {
        Msg::Listening { .. } => Ok(stream),
        Msg::Error { code, msg } => Err(RelayError::Remote { code, msg }),
        other => panic!("unexpected {other:?}"),
    }
}

async fn raw_hello(addr: &str, role: Role) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    write_msg(
        &mut stream,
        &Msg::Hello {
            version: VERSION,
            role,
        },
    )
    .await
    .unwrap();
    stream
}

async fn expect_error(stream: &mut TcpStream) -> String {
    match read_msg(stream).await {
        Ok(Msg::Error { code, .. }) => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

async fn wait_for(mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn handshake_and_data_flow_end_to_end_through_the_relay() {
    let h = start(Limits::default(), true).await;
    let (a, b) = (key(), key());
    let b_id = b.node_id().unwrap();
    let mut listener = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    assert!(matches!(listener.state(), ListenState::Registered { .. }));
    assert_eq!(h.relay.stats().listeners, 1);

    let served = tokio::spawn({
        let b = b.clone();
        async move {
            let stream = listener.accept().await.unwrap();
            let mut t = handshake::accept(stream, &b, &shared(), placeholder(), T)
                .await
                .unwrap();
            let got = t.reader.read_message(1 << 20).await.unwrap();
            t.writer.write_message(b"pong").await.unwrap();
            (t.info.node_id, got, listener)
        }
    });

    let stream = swarm_relay::connect(&h.addr, b_id).await.unwrap();
    let mut t = handshake::connect(stream, &a, &shared(), Some(b_id), placeholder(), T)
        .await
        .unwrap();
    assert_eq!(t.info.node_id, b_id);
    t.writer.write_message(b"ping").await.unwrap();
    assert_eq!(t.reader.read_message(1 << 20).await.unwrap(), b"pong");
    let (seen, got, _listener) = served.await.unwrap();
    assert_eq!(seen, a.node_id().unwrap());
    assert_eq!(got, b"ping");
    let stats = h.relay.stats();
    assert_eq!(stats.splices_total, 1);
    assert!(stats.bytes > 0);
}

#[tokio::test]
async fn relay_carries_only_ciphertext() {
    // Sniff the bytes the relay copies by splicing through a tap.
    let h = start(Limits::default(), false).await;
    let (a, b) = (key(), key());
    let mut listener = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    let b_id = b.node_id().unwrap();
    let secret = b"top-secret-plaintext-marker";
    let server = tokio::spawn({
        let b = b.clone();
        async move {
            let stream = listener.accept().await.unwrap();
            let mut t = handshake::accept(stream, &b, &shared(), placeholder(), T)
                .await
                .unwrap();
            let got = t.reader.read_message(1 << 20).await.unwrap();
            (got, listener)
        }
    });
    // A tap between the dialer and the relay records every byte A sends.
    let tap = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tap_addr = tap.local_addr().unwrap();
    let relay_addr = h.addr.clone();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    // The dialer asks for the relay's key first, then connects.
    let tap_task = {
        let seen = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut inbound, _) = tap.accept().await.unwrap();
                let mut outbound = TcpStream::connect(relay_addr.clone()).await.unwrap();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let (mut ir, mut iw) = inbound.split();
                    let (mut or, mut ow) = outbound.split();
                    let up = async {
                        let mut buf = [0u8; 4096];
                        loop {
                            let n = ir.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            seen.lock().unwrap().extend_from_slice(&buf[..n]);
                            ow.write_all(&buf[..n]).await.unwrap();
                        }
                    };
                    let down = async {
                        let mut buf = [0u8; 4096];
                        loop {
                            let n = or.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            iw.write_all(&buf[..n]).await.unwrap();
                        }
                    };
                    tokio::join!(up, down);
                });
            }
        })
    };
    let stream = swarm_relay::connect(&tap_addr.to_string(), b_id)
        .await
        .unwrap();
    let mut t = handshake::connect(stream, &a, &shared(), Some(b_id), placeholder(), T)
        .await
        .unwrap();
    t.writer.write_message(secret).await.unwrap();
    let (got, _l) = server.await.unwrap();
    assert_eq!(got, secret);
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.len() > 1000,
        "handshake bytes should have passed the tap"
    );
    assert!(
        !seen.windows(secret.len()).any(|w| w == secret),
        "plaintext leaked through the relay path"
    );
    // R5: the control connection is encrypted, so the target stays hidden.
    assert!(
        !seen.windows(32).any(|w| w == b_id.as_bytes()),
        "the target node ID was visible on the way to the relay"
    );
    tap_task.abort();
}

#[tokio::test]
async fn proof_must_be_signed_by_the_node_key() {
    let h = start(Limits::default(), false).await;
    let (real, other) = (key(), key());
    // Sign with one key, present the other's bundle.
    let mut stream = raw_hello(&h.addr, Role::Listen).await;
    let Msg::Challenge { nonce } = read_msg(&mut stream).await.unwrap() else {
        panic!()
    };
    let sig = other
        .signer()
        .unwrap()
        .try_sign(&relay_auth_input(&nonce, "x"))
        .unwrap();
    write_msg(
        &mut stream,
        &Msg::Prove {
            bundle: real.bundle().unwrap(),
            sig,
            name: "x".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(expect_error(&mut stream).await, "auth_failed");
    assert_eq!(h.relay.stats().auth_failures, 1);
    assert_eq!(h.relay.stats().listeners, 0);
}

#[tokio::test]
async fn proof_is_bound_to_the_relay_name() {
    let h = start(Limits::default(), true).await;
    let k = key();
    // A signature made for another relay's name is rejected here (REL-1).
    let err = raw_listen(&h.addr, &k, "evil-relay.example:42425")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("auth_failed"));
    // The right name works.
    raw_listen(&h.addr, &k, &h.addr).await.unwrap();
    // A replayed signature (same bytes, new challenge) fails.
    let mut stream = raw_hello(&h.addr, Role::Listen).await;
    let Msg::Challenge { .. } = read_msg(&mut stream).await.unwrap() else {
        panic!()
    };
    write_msg(
        &mut stream,
        &Msg::Prove {
            bundle: k.bundle().unwrap(),
            sig: vec![0; swarm_core::consts::SUITE1_SIG_LEN],
            name: h.addr.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(expect_error(&mut stream).await, "auth_failed");
}

#[tokio::test]
async fn tokens_are_single_use_and_unknown_tokens_are_rejected() {
    let h = start(Limits::default(), false).await;
    let b = key();
    let mut control = raw_listen(&h.addr, &b, "n").await.unwrap();

    let dial = tokio::spawn({
        let addr = h.addr.clone();
        let id = b.node_id().unwrap();
        async move { swarm_relay::connect(&addr, id).await }
    });
    let Msg::Incoming { token } = read_msg(&mut control).await.unwrap() else {
        panic!("expected Incoming");
    };

    let mut first = raw_hello(&h.addr, Role::Accept).await;
    write_msg(&mut first, &Msg::Accept { token }).await.unwrap();
    assert_eq!(read_msg(&mut first).await.unwrap(), Msg::Ready);
    let mut dialed = dial.await.unwrap().unwrap();
    // The pipe works both ways.
    first.write_all(b"hi").await.unwrap();
    let mut buf = [0u8; 2];
    dialed.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hi");

    let mut again = raw_hello(&h.addr, Role::Accept).await;
    write_msg(&mut again, &Msg::Accept { token }).await.unwrap();
    assert_eq!(expect_error(&mut again).await, "bad_token");

    let mut bogus = raw_hello(&h.addr, Role::Accept).await;
    write_msg(&mut bogus, &Msg::Accept { token: [7; 16] })
        .await
        .unwrap();
    assert_eq!(expect_error(&mut bogus).await, "bad_token");
}

#[tokio::test]
async fn an_unanswered_connect_times_out() {
    let limits = Limits {
        pending_ttl: Duration::from_millis(300),
        ..Limits::default()
    };
    let h = start(limits, false).await;
    let b = key();
    let _control = raw_listen(&h.addr, &b, "n").await.unwrap();
    let err = swarm_relay::connect(&h.addr, b.node_id().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("timeout"));
}

#[tokio::test]
async fn a_newer_registration_replaces_the_older_one() {
    let h = start(Limits::default(), false).await;
    let b = key();
    let mut first = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    let mut second = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    // The dialer reaches the second registration.
    let id = b.node_id().unwrap();
    let dial = tokio::spawn({
        let addr = h.addr.clone();
        async move { swarm_relay::connect(&addr, id).await }
    });
    let stream = tokio::time::timeout(T, second.accept()).await.unwrap();
    assert!(stream.is_some());
    dial.await.unwrap().unwrap();
    // The first one is told its control connection ended.
    let mut watch = first.watch();
    tokio::time::timeout(T, async {
        while !matches!(*watch.borrow(), ListenState::Reconnecting { .. }) {
            watch.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn closing_one_end_closes_the_other() {
    let h = start(Limits::default(), false).await;
    let b = key();
    let mut listener = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    let id = b.node_id().unwrap();
    let dial = tokio::spawn({
        let addr = h.addr.clone();
        async move { swarm_relay::connect(&addr, id).await.unwrap() }
    });
    let mut accepted = listener.accept().await.unwrap();
    let dialed = dial.await.unwrap();
    drop(dialed);
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(T, accepted.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    drop(accepted);
    wait_for(|| h.relay.stats().splices == 0).await;
}

#[tokio::test]
async fn idle_splices_are_closed() {
    let limits = Limits {
        idle: Duration::from_millis(400),
        ..Limits::default()
    };
    let h = start(limits, false).await;
    let b = key();
    let mut listener = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    let id = b.node_id().unwrap();
    let dial = tokio::spawn({
        let addr = h.addr.clone();
        async move { swarm_relay::connect(&addr, id).await.unwrap() }
    });
    let mut accepted = listener.accept().await.unwrap();
    let _dialed = dial.await.unwrap();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), accepted.read(&mut buf))
        .await
        .expect("idle splice should close")
        .unwrap_or(0);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn limits_are_enforced() {
    let limits = Limits {
        max_listeners: 1,
        ..Limits::default()
    };
    let h = start(limits, false).await;
    let _first = raw_listen(&h.addr, &key(), "n").await.unwrap();
    let err = raw_listen(&h.addr, &key(), "n").await.unwrap_err();
    assert_eq!(err.code(), Some("limit_exceeded"));

    let limits = Limits {
        max_splices: 0,
        ..Limits::default()
    };
    let h = start(limits, false).await;
    let b = key();
    let _control = raw_listen(&h.addr, &b, "n").await.unwrap();
    let err = swarm_relay::connect(&h.addr, b.node_id().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("limit_exceeded"));
}

#[tokio::test]
async fn malformed_and_oversized_input_is_refused() {
    let h = start(Limits::default(), false).await;
    // Not an RHello first.
    let mut stream = TcpStream::connect(&h.addr).await.unwrap();
    write_msg(&mut stream, &Msg::Ready).await.unwrap();
    assert_eq!(expect_error(&mut stream).await, "malformed");
    // Wrong version.
    let mut stream = TcpStream::connect(&h.addr).await.unwrap();
    write_msg(
        &mut stream,
        &Msg::Hello {
            version: 9,
            role: Role::Connect,
        },
    )
    .await
    .unwrap();
    assert_eq!(expect_error(&mut stream).await, "unsupported_version");
    // A frame over the limit closes the connection with no reply.
    let mut stream = TcpStream::connect(&h.addr).await.unwrap();
    stream
        .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(T, stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    // A silent connection is dropped after the hello timeout.
    let limits = Limits {
        hello_timeout: Duration::from_millis(200),
        ..Limits::default()
    };
    let h = start(limits, false).await;
    let mut stream = TcpStream::connect(&h.addr).await.unwrap();
    let n = tokio::time::timeout(T, stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_relay_that_bridges_to_the_wrong_node_fails_the_pin() {
    // A malicious relay answers Connect(X) but terminates the pipe with node Y.
    let evil = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let evil_addr = evil.local_addr().unwrap().to_string();
    let (victim, impostor) = (key(), key());
    let impostor_task = {
        let impostor = impostor.clone();
        tokio::spawn(async move {
            // It offers no key, so the control connection stays in clear.
            let (mut stream, _) = evil.accept().await.unwrap();
            let Msg::Hello { .. } = read_msg(&mut stream).await.unwrap() else {
                panic!()
            };
            assert_eq!(read_msg(&mut stream).await.unwrap(), Msg::Info);
            let reply = Msg::InfoReply {
                version: 1,
                caps: Vec::new(),
                limits: Vec::new(),
                relay_id: [0; 32],
                observed: "127.0.0.1:1".into(),
            };
            write_msg(&mut stream, &reply).await.unwrap();
            let (mut stream, _) = evil.accept().await.unwrap();
            let Msg::Hello { .. } = read_msg(&mut stream).await.unwrap() else {
                panic!()
            };
            let Msg::Connect { .. } = read_msg(&mut stream).await.unwrap() else {
                panic!()
            };
            write_msg(&mut stream, &Msg::Ready).await.unwrap();
            let _ = handshake::accept(stream, &impostor, &[], placeholder(), T).await;
        })
    };
    let dialer = key();
    let victim_id = victim.node_id().unwrap();
    let stream = swarm_relay::connect(&evil_addr, victim_id).await.unwrap();
    let outcome = handshake::connect(stream, &dialer, &[], Some(victim_id), placeholder(), T).await;
    assert!(
        matches!(outcome, Err(NetError::AuthFailed(_))),
        "pin must reject the impostor"
    );
    impostor_task.abort();
}

#[tokio::test]
async fn listener_reconnects_after_the_relay_restarts() {
    let h = start(Limits::default(), false).await;
    let b = key();
    let mut listener = swarm_relay::listen(&h.addr, b.clone()).await.unwrap();
    let mut watch = listener.watch();

    h.relay.shutdown();
    h.task.await.unwrap();
    tokio::time::timeout(T, async {
        while !matches!(*watch.borrow(), ListenState::Reconnecting { .. }) {
            watch.changed().await.unwrap();
        }
    })
    .await
    .expect("listener should notice the relay is gone");

    // A new relay comes up on the same address.
    let socket = TcpListener::bind(&h.addr).await.unwrap();
    let relay = Relay::new(RelayConfig::default());
    let _server = {
        let relay = relay.clone();
        tokio::spawn(async move { relay.serve(socket).await })
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while !matches!(*watch.borrow(), ListenState::Registered { .. }) {
            watch.changed().await.unwrap();
        }
    })
    .await
    .expect("listener should register again");
    assert_eq!(relay.stats().listeners, 1);

    let id = b.node_id().unwrap();
    let dial = tokio::spawn({
        let addr = h.addr.clone();
        async move { swarm_relay::connect(&addr, id).await }
    });
    assert!(tokio::time::timeout(T, listener.accept())
        .await
        .unwrap()
        .is_some());
    dial.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_relay_offers_its_domain_name_but_never_an_ip() {
    for (names, expected) in [
        (
            vec!["127.0.0.1:1", "relay.example:1"],
            Some("name:relay.example:1"),
        ),
        (vec!["127.0.0.1:1"], None),
    ] {
        let config = RelayConfig {
            names: names.into_iter().map(str::to_string).collect(),
            ..RelayConfig::default()
        };
        let harness = start_with(config, false).await;
        let info = swarm_relay::client::info(&harness.addr).await.unwrap();
        let offered = info.caps.iter().find(|cap| cap.starts_with("name:"));
        assert_eq!(offered.map(String::as_str), expected);
    }
}
