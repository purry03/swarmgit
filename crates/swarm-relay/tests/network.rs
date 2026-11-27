//! The relay network (design spec milestone R3), on a local DHT testnet.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use swarm_core::keyfile::NodeKey;
use swarm_disc::dht::{Dht, MainlineDht};
use swarm_relay::{
    client, directory_topic, Directory, DirectoryConfig, NetworkConfig, Relay, RelayConfig,
};
use tokio::net::TcpListener;

const T: Duration = Duration::from_secs(60);

fn config() -> DirectoryConfig {
    DirectoryConfig {
        allow_private: true,
        probe_timeout: Duration::from_secs(2),
        dial_back_spacing: Duration::ZERO,
    }
}

fn net_config(port: u16, public: bool) -> NetworkConfig {
    NetworkConfig {
        public,
        round: Duration::from_millis(500),
        announce_every: Duration::from_secs(2),
        retry: Duration::from_millis(200),
        directory: config(),
        port,
    }
}

fn testnet() -> mainline::Testnet {
    mainline::Testnet::builder(8)
        .bind_address(Ipv4Addr::LOCALHOST)
        .build()
        .unwrap()
}

fn dht(testnet: &mainline::Testnet) -> MainlineDht {
    let node = mainline::Dht::builder()
        .bootstrap(&testnet.bootstrap)
        .bind_address(Ipv4Addr::LOCALHOST)
        .build()
        .unwrap();
    MainlineDht::from_node(node)
}

async fn start() -> (Relay, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let relay = Relay::new(RelayConfig::default());
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (relay, addr)
}

/// A relay in the network, using the DHT.
async fn member(testnet: &mainline::Testnet, public: bool) -> (Relay, SocketAddr, Arc<Directory>) {
    let (relay, addr) = start().await;
    let directory = relay.join_network(Some(dht(testnet)), net_config(addr.port(), public));
    (relay, addr, directory)
}

async fn until<F: FnMut() -> bool>(what: &str, mut done: F) {
    tokio::time::timeout(T, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn knows(directory: &Directory, addrs: &[SocketAddr]) -> bool {
    let known: Vec<SocketAddr> = directory.ranked().iter().map(|r| r.addr).collect();
    addrs.iter().all(|addr| known.contains(addr))
}

#[tokio::test(flavor = "multi_thread")]
async fn three_relays_find_each_other_through_the_dht() {
    let testnet = testnet();
    let (a, a_addr, a_dir) = member(&testnet, true).await;
    let (b, b_addr, b_dir) = member(&testnet, false).await;
    let (c, c_addr, c_dir) = member(&testnet, false).await;
    until("every relay to know the other two", || {
        knows(&a_dir, &[b_addr, c_addr])
            && knows(&b_dir, &[a_addr, c_addr])
            && knows(&c_dir, &[a_addr, b_addr])
    })
    .await;
    // No relay lists itself.
    assert_eq!(a_dir.len(), 2);

    // A fresh client with only the DHT lists all three, and a junk entry in
    // the directory is dropped (REL-24).
    let junk = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let junk_addr = junk.local_addr().unwrap();
    tokio::spawn(async move { while junk.accept().await.is_ok() {} });
    assert!(
        dht(&testnet)
            .announce(directory_topic(), junk_addr.port())
            .await
    );
    let client = Directory::new(Some(dht(&testnet)), None, config());
    tokio::time::timeout(T, async {
        while !knows(&client, &[a_addr, b_addr, c_addr]) {
            client.refresh(&[]).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(client.len(), 3);
    assert!(!knows(&client, &[junk_addr]));
    for relay in [a, b, c] {
        relay.shutdown();
    }
}

#[tokio::test]
async fn a_relay_nobody_can_dial_is_not_verified() {
    let (a, a_addr) = start().await;
    let a_dir = a.join_network(None, net_config(a_addr.port(), true));
    let closed = {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        socket.local_addr().unwrap().port()
    };
    let (reachable, other) = (start().await, start().await);
    // Nothing listens on the port asked about.
    assert!(!client::dial_back(&a_addr.to_string(), closed, [1; 32])
        .await
        .unwrap());
    // Something listens, but it is not the relay that asked.
    assert!(
        !client::dial_back(&a_addr.to_string(), other.1.port(), [1; 32])
            .await
            .unwrap()
    );
    assert!(a_dir.is_empty());
    let id = reachable.0.relay_id();
    assert!(
        client::dial_back(&a_addr.to_string(), reachable.1.port(), id)
            .await
            .unwrap()
    );
    assert!(knows(&a_dir, &[reachable.1]));
    assert_eq!(
        client::get_relays(&a_addr.to_string(), 32).await.unwrap(),
        vec![reachable.1]
    );
}

#[tokio::test]
async fn a_relay_outside_the_network_shares_nothing() {
    let (_relay, addr) = start().await;
    let name = addr.to_string();
    let info = client::info(&name).await.unwrap();
    assert!(!info.caps.iter().any(|cap| cap == "network"));
    assert!(client::get_relays(&name, 32).await.unwrap().is_empty());
    assert!(!client::dial_back(&name, addr.port(), info.relay_id)
        .await
        .unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_is_found_by_id_through_referrals_and_survives_a_dead_relay() {
    let key = Arc::new(NodeKey::generate().unwrap());
    let node = key
        .bundle()
        .map(|b| swarm_core::crypto::node_id(&b))
        .unwrap();
    let (a, a_addr) = start().await;
    let (b, b_addr) = start().await;
    let (c, c_addr) = start().await;
    // `a` knows the other two relays; the node is on them and not on `a`.
    let a_dir = a.join_network(None, net_config(a_addr.port(), true));
    for (relay, addr) in [(&b, b_addr), (&c, c_addr)] {
        a_dir.verified(addr, relay.relay_id(), Duration::ZERO);
    }
    let on_b = swarm_relay::listen(&b_addr.to_string(), Arc::clone(&key))
        .await
        .unwrap();
    let _on_c = swarm_relay::listen(&c_addr.to_string(), Arc::clone(&key))
        .await
        .unwrap();

    // The client's set holds only `a`, so it needs the referral.
    let client = Directory::new(None, None, config());
    assert!(client.probe(a_addr).await);
    let mut found = client.locate(node, &[]).await;
    found.sort();
    let mut expected = vec![b_addr, c_addr];
    expected.sort();
    assert_eq!(found, expected);

    // One home relay dies: the node is still found through the other.
    drop(on_b);
    b.shutdown();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(client.locate(node, &[]).await, vec![c_addr]);
    client::connect(&c_addr.to_string(), node).await.unwrap();
    c.shutdown();
}
