//! M9 acceptance (spec 11.10): over 1,000 runs, the sizes of the first three
//! handshake messages are not constant.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::RepoId;
use swarm_core::keyfile::NodeKey;
use swarm_net::handshake;
use swarm_net::{ObfsTransport, Options, PeerAddr, Tcp, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const RUNS: usize = 1000;
const AT_ONCE: usize = 100;
/// Critical value of chi-square with one degree of freedom at p = 0.001.
const CRITICAL: f64 = 10.83;

fn options() -> Options {
    Options {
        shaper: ObfsTransport(Tcp).shaper(),
        ..Options::from(Duration::from_secs(60))
    }
}

fn peer() -> PeerAddr {
    PeerAddr::Tcp("127.0.0.1:1".parse().unwrap())
}

/// Pass one length-prefixed message along and report its size.
async fn forward(from: &mut DuplexStream, to: &mut DuplexStream) -> usize {
    let length = from.read_u16().await.unwrap();
    let mut body = vec![0u8; usize::from(length)];
    from.read_exact(&mut body).await.unwrap();
    to.write_u16(length).await.unwrap();
    to.write_all(&body).await.unwrap();
    usize::from(length)
}

/// Sit between the two ends of a real handshake for its first three messages,
/// then hang up.
async fn observe(client_key: Arc<NodeKey>, server_key: Arc<NodeKey>) -> [usize; 3] {
    let repo = RepoId::from_bytes([7u8; 32]);
    let (client_io, mut near_client) = tokio::io::duplex(64 * 1024);
    let (server_io, mut near_server) = tokio::io::duplex(64 * 1024);
    let client = tokio::spawn(async move {
        handshake::connect(client_io, &client_key, &[repo], None, peer(), options())
            .await
            .map(drop)
    });
    let server = tokio::spawn(async move {
        handshake::accept(server_io, &server_key, &[repo], peer(), options())
            .await
            .map(drop)
    });
    let sizes = [
        forward(&mut near_client, &mut near_server).await,
        forward(&mut near_server, &mut near_client).await,
        forward(&mut near_client, &mut near_server).await,
    ];
    drop((near_client, near_server));
    // Both fail now; only their end matters.
    let _ = client.await.unwrap();
    let _ = server.await.unwrap();
    sizes
}

/// Pearson's statistic against "one size, 999 times in 1,000".
fn chi_square_against_constant(sizes: &[usize]) -> f64 {
    let mut counts = HashMap::new();
    for size in sizes {
        *counts.entry(size).or_insert(0usize) += 1;
    }
    let total = sizes.len() as f64;
    let common = counts.values().copied().max().unwrap_or(0) as f64;
    let expected = [total * 0.999, total * 0.001];
    [common, total - common]
        .iter()
        .zip(expected)
        .map(|(seen, wanted)| (seen - wanted).powi(2) / wanted)
        .sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_message_sizes_are_not_constant() {
    let client_key = Arc::new(NodeKey::generate().unwrap());
    let server_key = Arc::new(NodeKey::generate().unwrap());
    let mut seen: [Vec<usize>; 3] = Default::default();
    for _ in 0..RUNS / AT_ONCE {
        let batch: Vec<_> = (0..AT_ONCE)
            .map(|_| tokio::spawn(observe(Arc::clone(&client_key), Arc::clone(&server_key))))
            .collect();
        for run in batch {
            for (list, size) in seen.iter_mut().zip(run.await.unwrap()) {
                list.push(size);
            }
        }
    }
    for (index, sizes) in seen.iter().enumerate() {
        assert_eq!(sizes.len(), RUNS);
        let statistic = chi_square_against_constant(sizes);
        assert!(
            statistic > CRITICAL,
            "message {} looks constant: chi-square {statistic}",
            index + 1
        );
    }
}

#[test]
fn the_statistic_does_not_reject_a_constant() {
    assert!(chi_square_against_constant(&[96; RUNS]) < CRITICAL);
}
