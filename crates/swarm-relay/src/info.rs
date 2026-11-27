//! The `role=4` requests: what the relay is, which other relays it knows,
//! whether it holds a node, and dial-back checks (relay design spec 5.4).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use crate::proto::{Msg, MAX_REFERRALS, MAX_RELAYS_REPLY, VERSION};
use crate::secure::{Channel, CAP_NK};
use crate::server::{send_error, Relay};
use crate::{client, RelayError};
use crate::{is_domain_name, name};

/// One dial-back per caller address in this long.
const DIAL_BACK_EVERY: Duration = Duration::from_secs(10);
/// Callers remembered for that limit.
const DIAL_BACK_CALLERS: usize = 1024;
/// Time for the dial back to answer.
const DIAL_BACK_TIMEOUT: Duration = Duration::from_secs(5);

impl Relay {
    /// Answer one request. The connection closes after the reply.
    pub(crate) async fn info_session(
        &self,
        mut stream: Channel,
        peer: SocketAddr,
    ) -> Result<(), RelayError> {
        let wait = self.state.config.limits.hello_timeout;
        let request = tokio::time::timeout(wait, stream.recv())
            .await
            .map_err(|_| RelayError::Timeout("info"))??;
        // Refused after reading the request, so the caller sees the reason.
        let max = self.state.config.limits.requests_per_minute;
        if let Some(ip) = self.counted(peer) {
            if !self.state.quotas.request(ip, max) {
                send_error(&mut stream, "limit_exceeded", "too many requests").await;
                return Err(RelayError::Protocol("request rate".into()));
            }
        }
        let reply = match request {
            Msg::Info => self.describe(peer),
            Msg::GetRelays { max } => {
                let max = usize::try_from(max)
                    .unwrap_or(MAX_RELAYS_REPLY)
                    .min(MAX_RELAYS_REPLY);
                Msg::Relays {
                    addrs: self
                        .state
                        .network
                        .get()
                        .map(|directory| directory.sample(max))
                        .unwrap_or_default(),
                }
            }
            Msg::Where { target } => Msg::Referral {
                found: self.has_listener(&target),
                closer: self
                    .state
                    .network
                    .get()
                    .map(|directory| directory.closest(&target, MAX_REFERRALS))
                    .unwrap_or_default(),
            },
            Msg::Lookup { topic } => Msg::Holders {
                nodes: self.state.topics.holders(&topic),
            },
            Msg::DialBack { port, relay_id } => Msg::DialBackReply {
                reachable: self.check_caller(peer.ip(), port, relay_id).await,
            },
            _ => Msg::Error {
                code: "malformed".into(),
                msg: "not an info request".into(),
            },
        };
        stream.send(&reply).await
    }

    fn describe(&self, peer: SocketAddr) -> Msg {
        let limits = &self.state.config.limits;
        let mut caps = vec!["listen".to_string(), "connect".to_string()];
        if self.state.key.is_some() {
            caps.push(CAP_NK.to_string());
        }
        if self.state.network.get().is_some() {
            caps.push("network".to_string());
        }
        let domain = self.state.config.names.iter().find(|n| is_domain_name(n));
        if let Some(domain) = domain {
            caps.push(format!("{}{domain}", name::CAP_PREFIX));
        }
        Msg::InfoReply {
            version: VERSION,
            caps,
            limits: vec![
                ("max_listeners".into(), limits.max_listeners as u64),
                ("max_splices".into(), limits.max_splices as u64),
                ("idle_secs".into(), limits.idle.as_secs()),
            ],
            relay_id: self.state.relay_id,
            observed: peer.to_string(),
        }
    }

    /// Dial `port` on the caller's own address and expect the caller's relay.
    /// The address is never chosen by the request, so this cannot be used to
    /// probe third parties.
    async fn check_caller(&self, ip: IpAddr, port: u16, relay_id: [u8; 32]) -> bool {
        let Some(directory) = self.state.network.get() else {
            return false;
        };
        {
            let Ok(mut recent) = self.state.dial_backs.lock() else {
                return false;
            };
            let now = Instant::now();
            recent.retain(|_, at| now.duration_since(*at) < DIAL_BACK_EVERY);
            let spacing = directory.config().dial_back_spacing;
            if recent
                .get(&ip)
                .is_some_and(|at| now.duration_since(*at) < spacing)
                || recent.len() >= DIAL_BACK_CALLERS
            {
                return false;
            }
            recent.insert(ip, now);
        }
        let addr = SocketAddr::new(ip, port);
        let started = Instant::now();
        let answer = tokio::time::timeout(DIAL_BACK_TIMEOUT, client::info(&addr.to_string())).await;
        match answer {
            Ok(Ok(info)) if info.relay_id == relay_id && info.version == VERSION => {
                // A verified relay is worth knowing (REL-24, REL-26).
                directory.verified(addr, relay_id, started.elapsed());
                true
            }
            _ => false,
        }
    }
}
