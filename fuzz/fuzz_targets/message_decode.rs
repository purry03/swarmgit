#![no_main]
//! Peer messages come from the network: decoding must never panic (SEC-9),
//! and what decodes must encode and decode again.

use libfuzzer_sys::fuzz_target;
use swarm_net::Message;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = Message::decode(data) {
        if let Ok(bytes) = message.encode() {
            assert_eq!(Message::decode(&bytes).ok(), Some(message));
        }
    }
});
