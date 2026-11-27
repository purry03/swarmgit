//! `swarm-relay`: run a relay server.

use clap::Parser;
use swarm_relay::cli::{run, RelayArgs};

/// Run a Swarmgit relay.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[command(flatten)]
    relay: RelayArgs,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    run(Args::parse().relay).await
}
