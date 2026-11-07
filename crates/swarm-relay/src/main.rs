//! `swarm-relay`: run a relay server.

use std::time::Duration;

use clap::Parser;
use swarm_relay::{Limits, Relay, RelayConfig, DEFAULT_PORT};
use tokio::net::TcpListener;

/// Run a Swarmgit relay.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value_t = format!("0.0.0.0:{DEFAULT_PORT}"))]
    listen: String,
    /// Name clients dial this relay by, as host:port. Repeatable. A listener's
    /// proof is bound to it. Without any, proofs are not bound to a name.
    #[arg(long = "name")]
    names: Vec<String>,
    /// Most registered listeners.
    #[arg(long, default_value_t = 10_000)]
    max_listeners: usize,
    /// Most simultaneous splices.
    #[arg(long, default_value_t = 500)]
    max_splices: usize,
    /// Close a splice after this many idle seconds.
    #[arg(long, default_value_t = 120)]
    idle_secs: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let listener = TcpListener::bind(&args.listen).await?;
    if args.names.is_empty() {
        tracing::warn!("no --name given: listener proofs are not bound to this relay");
    }
    tracing::info!("relay listening on {}", listener.local_addr()?);
    let relay = Relay::new(RelayConfig {
        names: args.names,
        limits: Limits {
            max_listeners: args.max_listeners,
            max_splices: args.max_splices,
            idle: Duration::from_secs(args.idle_secs),
            ..Limits::default()
        },
    });
    tokio::select! {
        () = relay.serve(listener) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}
