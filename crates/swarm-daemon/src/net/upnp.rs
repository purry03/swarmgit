//! Best-effort UPnP IGD port mapping for the TCP listener (M5).
//!
//! When a gateway answers, the listen port is mapped with a one-hour lease
//! that is renewed every 30 minutes. Any failure is logged at debug and
//! ignored: peers behind a NAT still reach out through their own dials.
//! `SWARM_NO_UPNP=1` turns this off (the tests set it).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use igd_next::aio::tokio::search_gateway;
use igd_next::{PortMappingProtocol, SearchOptions};

use super::NetState;

/// Lease asked of the gateway, in seconds.
const LEASE_SECS: u32 = 3600;
/// Renew the mapping well before the lease ends.
const RENEW_EVERY: Duration = Duration::from_secs(30 * 60);
/// Gateway search timeout.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

/// True when `SWARM_NO_UPNP=1`.
#[must_use]
pub fn disabled() -> bool {
    std::env::var("SWARM_NO_UPNP").is_ok_and(|value| value == "1")
}

/// Spawn the mapping loop unless disabled.
pub fn start(net: &Arc<NetState>) {
    if disabled() {
        return;
    }
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(RENEW_EVERY);
        loop {
            tick.tick().await;
            let Some(net) = weak.upgrade() else { break };
            let listen = net.listen;
            drop(net);
            if !map_once(listen).await {
                // No gateway: do not keep searching every 30 minutes.
                break;
            }
        }
    });
}

/// Map the listen port once. Returns false when no gateway was found.
async fn map_once(listen: SocketAddr) -> bool {
    let mut options = SearchOptions::default();
    options.timeout = Some(SEARCH_TIMEOUT);
    let gateway = match search_gateway(options).await {
        Ok(gateway) => gateway,
        Err(err) => {
            tracing::debug!(error = %err, "no upnp gateway");
            return false;
        }
    };
    let local_ip = match listen.ip() {
        ip if ip.is_unspecified() => match super::primary_ipv4() {
            Some(ip) => IpAddr::V4(ip),
            None => return false,
        },
        ip => ip,
    };
    let local = SocketAddr::new(local_ip, listen.port());
    match gateway
        .add_port(
            PortMappingProtocol::TCP,
            listen.port(),
            local,
            LEASE_SECS,
            "swarmgit",
        )
        .await
    {
        Ok(()) => tracing::debug!(port = listen.port(), "upnp mapping added"),
        Err(err) => tracing::debug!(error = %err, "upnp mapping failed"),
    }
    true
}
