#![no_main]

use libfuzzer_sys::fuzz_target;
use swarm_core::ids::Link;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = Link::parse(text);
    }
});
