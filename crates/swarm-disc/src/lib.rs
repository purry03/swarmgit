//! Discovery routes (spec section 6). M2 implements route A, link hints.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod hints;

pub use hints::{resolve_hints, Candidate, HintError};
