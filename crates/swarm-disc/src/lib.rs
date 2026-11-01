//! Discovery routes (spec section 6). Route A (link hints) route B (LAN multicast)
//! and route C (Mainline DHT).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dht;
pub mod hints;
pub mod lan;

pub use hints::{resolve_hints, Candidate, HintError};
