//! `swarm-relay-peer`: a tiny peer for the relay acceptance test.
//!
//! `listen` registers with a relay and answers every dialer after a full swarm
//! handshake. `dial` reaches a node through a relay, pins its node ID, runs the
//! handshake, and exchanges one message. This is test tooling, not shipped.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use swarm_core::ids::NodeId;
use swarm_core::keyfile::NodeKey;
use swarm_net::handshake;
use swarm_net::PeerAddr;

#[derive(Parser)]
struct Args {
    /// Node key file; created when missing.
    #[arg(long)]
    key: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the node ID and exit.
    Id,
    /// Register with a relay and serve dialers.
    Listen {
        /// Relay as host:port.
        #[arg(long)]
        relay: String,
    },
    /// Reach a node through a relay.
    Dial {
        /// Relay as host:port.
        #[arg(long)]
        relay: String,
        /// Target node ID (base32).
        #[arg(long)]
        target: String,
    },
}

fn load_key(path: &PathBuf) -> anyhow::Result<NodeKey> {
    if let Ok(bytes) = std::fs::read(path) {
        return Ok(NodeKey::from_bytes(&bytes)?);
    }
    let key = NodeKey::generate()?;
    std::fs::write(path, key.to_bytes())?;
    Ok(key)
}

const PLACEHOLDER: &str = "0.0.0.0:0";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let key = Arc::new(load_key(&args.key)?);
    let node = key.node_id()?;
    let timeout = Duration::from_secs(20);
    // The handshake needs one shared repo; both ends use this fixed one.
    let repos = [swarm_core::ids::RepoId::from_bytes([1; 32])];
    let addr = PeerAddr::Tcp(PLACEHOLDER.parse::<SocketAddr>()?);
    match args.cmd {
        Cmd::Id => println!("{}", node.to_base32()),
        Cmd::Listen { relay } => {
            let mut listener = swarm_relay::listen(&relay, key.clone()).await?;
            println!("listening node={}", node.to_base32());
            while let Some(stream) = listener.accept().await {
                let key = key.clone();
                let addr = addr.clone();
                tokio::spawn(async move {
                    match handshake::accept(stream, &key, &repos, addr, timeout).await {
                        Ok(mut transport) => {
                            let peer = transport.info.node_id.to_base32();
                            match transport.reader.read_message(1 << 20).await {
                                Ok(message) => {
                                    println!(
                                        "served peer={peer} got={}",
                                        String::from_utf8_lossy(&message)
                                    );
                                    let _ = transport.writer.write_message(b"pong via relay").await;
                                }
                                Err(err) => println!("read failed: {err}"),
                            }
                        }
                        Err(err) => println!("handshake failed: {err}"),
                    }
                });
            }
        }
        Cmd::Dial { relay, target } => {
            let target = NodeId::from_base32(&target)?;
            let stream = swarm_relay::connect(&relay, target).await?;
            let mut transport =
                handshake::connect(stream, &key, &repos, Some(target), addr, timeout).await?;
            transport.writer.write_message(b"ping via relay").await?;
            let reply = transport.reader.read_message(1 << 20).await?;
            println!(
                "dialed peer={} reply={}",
                transport.info.node_id.to_base32(),
                String::from_utf8_lossy(&reply)
            );
        }
    }
    Ok(())
}
