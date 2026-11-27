//! Command line of a relay server, shared by `swarm-relay` and `swarm relay`.

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use swarm_core::ids::NodeId;
use swarm_disc::dht::{self, DhtConfig, MainlineDht};
use tokio::net::TcpListener;

use crate::proto::MAX_PASSWORD;
use crate::{
    key_text, DirectoryConfig, Limits, NetworkConfig, Relay, RelayConfig, RelayKey, DEFAULT_PORT,
};

/// Options of a relay server.
#[derive(Clone, Debug, clap::Args)]
pub struct RelayArgs {
    /// Address to listen on.
    #[arg(long, default_value_t = format!("0.0.0.0:{DEFAULT_PORT}"))]
    listen: String,
    /// Name clients dial this relay by, as host:port. Repeatable. A listener's
    /// proof is bound to it. Without any, proofs are not bound to a name.
    #[arg(long = "name")]
    names: Vec<String>,
    /// Let only this node register. Repeatable. Keeps the relay out of the
    /// relay network.
    #[arg(long = "allow-node", value_name = "NODE_ID", value_parser = node_id)]
    allow_nodes: Vec<NodeId>,
    /// Require the secret in this file to register or dial. Clients send it
    /// only to an address that includes the relay's key, so this needs
    /// --key-file. Keeps the relay out of the relay network.
    #[arg(long, value_name = "FILE", requires = "key_file")]
    password_file: Option<PathBuf>,
    /// Keep the relay's key in this file, created on first use, so that
    /// `host:port#key` addresses keep working across restarts.
    #[arg(long, value_name = "FILE")]
    key_file: Option<PathBuf>,
    /// Serve counters over HTTP on this address (off by default).
    #[arg(long, value_name = "ADDR")]
    metrics: Option<SocketAddr>,
    /// Most registered listeners.
    #[arg(long, default_value_t = 10_000)]
    max_listeners: usize,
    /// Most simultaneous splices.
    #[arg(long, default_value_t = 500)]
    max_splices: usize,
    /// Most nodes registered from one address.
    #[arg(long, default_value_t = 2)]
    listeners_per_ip: usize,
    /// Most splices one dialing address may have open.
    #[arg(long, default_value_t = 8)]
    splices_per_ip: usize,
    /// Most connect attempts from one address in a minute.
    #[arg(long, default_value_t = 20)]
    connects_per_minute: usize,
    /// Rate of one splice in KiB per second, 0 for no limit.
    #[arg(long, default_value_t = 8 * 1024)]
    splice_kib_per_sec: u64,
    /// Most bytes carried for one node in a day, in MiB.
    #[arg(long, default_value_t = 10 * 1024)]
    node_daily_mib: u64,
    /// Close a splice after this many idle seconds.
    #[arg(long, default_value_t = 120)]
    idle_secs: u64,
    /// Stay out of the relay network: no DHT announce and no gossip.
    #[arg(long, conflicts_with = "public")]
    no_network: bool,
    /// This relay is reachable from the internet: announce it without first
    /// asking another relay to dial back.
    #[arg(long)]
    public: bool,
    /// Accept relays on loopback and private addresses, for a closed network.
    #[arg(long)]
    allow_private: bool,
    /// Seconds between relay network rounds (gossip).
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..))]
    round_secs: u64,
}

fn node_id(text: &str) -> Result<NodeId, String> {
    NodeId::from_base32(text).map_err(|err| err.to_string())
}

fn password(args: &RelayArgs) -> anyhow::Result<Option<String>> {
    let Some(path) = &args.password_file else {
        return Ok(None);
    };
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let secret = text.trim_end_matches(['\r', '\n']);
    if secret.is_empty() || secret.len() > MAX_PASSWORD {
        bail!("the password must be 1 to {MAX_PASSWORD} bytes");
    }
    Ok(Some(secret.to_string()))
}

/// The key in `path`, created with owner-only access when missing.
fn load_key(path: &Path) -> anyhow::Result<RelayKey> {
    let named = |what: &str| format!("cannot {what} {}", path.display());
    match std::fs::read(path) {
        Ok(bytes) => return RelayKey::from_bytes(&bytes).with_context(|| named("use")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| named("read")),
    }
    let key = RelayKey::generate()?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(&key.to_bytes()))
        .with_context(|| named("write"))?;
    Ok(key)
}

/// Join the relay network (REL-33), through the DHT unless `SWARM_NO_DHT=1`.
fn join(relay: &Relay, args: &RelayArgs, port: u16) {
    let dht = if dht::disabled() {
        None
    } else {
        match MainlineDht::start(&DhtConfig::from_env(Vec::new())) {
            Ok(dht) => Some(dht),
            Err(err) => {
                tracing::warn!("relay network runs without the DHT: {err}");
                None
            }
        }
    };
    let round = Duration::from_secs(args.round_secs);
    let defaults = NetworkConfig::new(port);
    relay.join_network(
        dht,
        NetworkConfig {
            public: args.public,
            round,
            announce_every: defaults.announce_every.min(round),
            retry: defaults.retry.min(round),
            directory: DirectoryConfig {
                allow_private: args.allow_private,
                // A closed test network checks again quickly.
                dial_back_spacing: Duration::from_secs(10).min(round / 2),
                ..DirectoryConfig::default()
            },
            ..defaults
        },
    );
}

/// Run a relay until interrupted. The caller sets up logging.
pub async fn run(args: RelayArgs) -> anyhow::Result<()> {
    let password = password(&args)?;
    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("cannot listen on {}", args.listen))?;
    if args.names.is_empty() {
        tracing::warn!("no --name given: listener proofs are not bound to this relay");
    }
    let local = listener.local_addr()?;
    let key = args.key_file.as_deref().map(load_key).transpose()?;
    // A relay strangers cannot use would only waste their time in the
    // directory.
    let private = password.is_some() || !args.allow_nodes.is_empty();
    let relay = Relay::new(RelayConfig {
        names: args.names.clone(),
        allow_nodes: args.allow_nodes.iter().map(|id| *id.as_bytes()).collect(),
        password,
        key: key.clone(),
        limits: Limits {
            max_listeners: args.max_listeners,
            max_splices: args.max_splices,
            listeners_per_ip: args.listeners_per_ip,
            splices_per_ip: args.splices_per_ip,
            connects_per_minute: args.connects_per_minute,
            splice_bytes_per_second: args.splice_kib_per_sec.saturating_mul(1024),
            node_daily_bytes: args.node_daily_mib.saturating_mul(1024 * 1024),
            idle: Duration::from_secs(args.idle_secs),
            ..Limits::default()
        },
    });
    if let Some(addr) = args.metrics {
        let metrics = TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot serve metrics on {addr}"))?;
        tracing::info!("metrics on {}", metrics.local_addr()?);
        let relay = relay.clone();
        tokio::spawn(async move { relay.serve_metrics(metrics).await });
    }
    tracing::info!("relay listening on {local}");
    if let Some(key) = &key {
        for name in &args.names {
            tracing::info!("relay address: {name}#{}", key_text(key.public()));
        }
    }
    if private && !args.no_network {
        tracing::info!("private relay: staying out of the relay network");
    } else if !args.no_network {
        join(&relay, &args, local.port());
    }
    tokio::select! {
        () = relay.serve(listener) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}
