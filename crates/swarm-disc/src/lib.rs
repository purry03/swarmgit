//! Discovery routes (spec section 6). Route A (link hints) and route B (LAN multicast).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod hints;
pub mod lan;

pub use hints::{resolve_hints, Candidate, HintError};
