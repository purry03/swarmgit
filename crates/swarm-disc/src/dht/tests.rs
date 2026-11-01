use std::net::Ipv4Addr;

use super::*;

fn repo(byte: u8) -> RepoId {
    RepoId::from_bytes([byte; 32])
}

#[test]
fn day_and_topics() {
    assert_eq!(day_number(0), 0);
    assert_eq!(day_number(86_399), 0);
    assert_eq!(day_number(86_400), 1);
    let r = repo(1);
    let lookup = lookup_topics(&r, 10);
    assert_eq!(lookup, vec![topic(&r, 10), topic(&r, 9)]);
    let announce = announce_topics(&r, 10);
    assert_eq!(announce, vec![topic(&r, 9), topic(&r, 10), topic(&r, 11)]);
    assert_ne!(topic(&r, 10), topic(&repo(2), 10));
}

#[test]
fn salt_is_48_bytes() {
    let salt = head_salt(&repo(7));
    assert_eq!(salt.len(), 48);
    assert!(salt.starts_with(SALT_HEAD));
    // The repo ID must never appear in the clear (it is secret).
    assert_ne!(&salt[16..], &[7u8; 32]);
    assert!(!salt.windows(32).any(|window| window == [7u8; 32]));
    assert_ne!(head_salt(&repo(7)), head_salt(&repo(8)));
}

#[test]
fn head_round_trip_and_strictness() {
    let head = Head {
        manifest_id: [9; 32],
        seq: 42,
    };
    let bytes = encode_head(&head);
    assert!(bytes.starts_with(b"d1:m32:"));
    assert!(bytes.ends_with(b"1:si42ee"));
    assert_eq!(decode_head(&bytes), Some(head));
    let zero = Head {
        manifest_id: [0; 32],
        seq: 0,
    };
    assert_eq!(decode_head(&encode_head(&zero)), Some(zero));

    let mut leading_zero = b"d1:m32:".to_vec();
    leading_zero.extend_from_slice(&[1; 32]);
    leading_zero.extend_from_slice(b"1:si042ee");
    assert_eq!(decode_head(&leading_zero), None);
    let mut negative = b"d1:m32:".to_vec();
    negative.extend_from_slice(&[1; 32]);
    negative.extend_from_slice(b"1:si-1ee");
    assert_eq!(decode_head(&negative), None);
    assert_eq!(decode_head(b"d1:m3:abc1:si1ee"), None);
    let mut trailing = bytes.clone();
    trailing.push(b'x');
    assert_eq!(decode_head(&trailing), None);
}

fn testnet() -> mainline::Testnet {
    mainline::Testnet::builder(10)
        .bind_address(Ipv4Addr::LOCALHOST)
        .build()
        .expect("testnet")
}

fn client(bootstrap: &[String]) -> MainlineDht {
    MainlineDht::start(&DhtConfig {
        bootstrap: bootstrap.to_vec(),
        replace_default: true,
        bind: Some(Ipv4Addr::LOCALHOST),
        port: None,
    })
    .expect("client node")
}

#[tokio::test(flavor = "multi_thread")]
async fn announce_then_find_on_testnet() {
    let net = tokio::task::spawn_blocking(testnet).await.expect("join");
    let a = client(&net.bootstrap);
    let b = client(&net.bootstrap);
    let r = repo(3);
    assert_eq!(a.announce_repo(r, 4242).await, 3);
    let found = b.find(r).await;
    assert!(
        found.iter().any(|addr| addr.port() == 4242),
        "B did not find A: {found:?}"
    );
    assert!(b.find(repo(4)).await.is_empty());
    assert!(!a.known_nodes().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn head_put_get_keeps_newest() {
    let net = tokio::task::spawn_blocking(testnet).await.expect("join");
    let a = client(&net.bootstrap);
    let b = client(&net.bootstrap);
    let seed = [5u8; 32];
    let key = mainline::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    let r = repo(6);
    assert_eq!(b.get_head(key, r).await, None);
    for seq in [2, 3] {
        let head = Head {
            manifest_id: [seq as u8; 32],
            seq,
        };
        assert!(a.put_head(seed, r, head).await, "put seq {seq}");
    }
    let got = b.get_head(key, r).await.expect("head");
    assert_eq!(got.seq, 3);
    assert_eq!(got.manifest_id, [3; 32]);
    // A different repo has a different salt.
    assert_eq!(b.get_head(key, repo(7)).await, None);
}

/// Against the public Mainline network. Run by hand:
/// `SWARM_LIVE_DHT=1 cargo test live_dht -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the public internet; set SWARM_LIVE_DHT=1"]
async fn live_dht() {
    if std::env::var("SWARM_LIVE_DHT").map_or(true, |value| value != "1") {
        eprintln!("live_dht: SWARM_LIVE_DHT=1 is not set, skipping");
        return;
    }
    let config = DhtConfig {
        bootstrap: Vec::new(),
        replace_default: false,
        bind: None,
        port: None,
    };
    let a = MainlineDht::start(&config).expect("node a");
    let b = MainlineDht::start(&config).expect("node b");
    let mut id = [0u8; 32];
    for (ix, byte) in id.iter_mut().enumerate() {
        *byte = (std::process::id() as u8).wrapping_add(ix as u8);
    }
    let r = RepoId::from_bytes(id);
    let port = 40_000 + (std::process::id() % 20_000) as u16;
    assert!(a.announce_repo(r, port).await > 0, "no announce stored");
    let found = b.find(r).await;
    assert!(
        found.iter().any(|addr| addr.port() == port),
        "B did not find A on the live DHT: {found:?}"
    );
    let seed = id;
    let key = mainline::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    let head = Head {
        manifest_id: [9; 32],
        seq: 1,
    };
    assert!(a.put_head(seed, r, head).await, "put head");
    let got = b.get_head(key, r).await.expect("live head");
    assert_eq!(got.seq, 1);
}
