//! LAN multicast discovery (spec 6.3).

pub mod packet;
pub mod service;

pub use packet::{Kind, LanPacket, PacketError, RateLimiter};
pub use service::{LanConfig, LanRepos, LanService, RepoSource};
