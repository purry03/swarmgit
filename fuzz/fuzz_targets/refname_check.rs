#![no_main]

use libfuzzer_sys::fuzz_target;
use swarm_core::glob::glob_match;
use swarm_core::refname::is_valid_refname;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = is_valid_refname(text);
        let _ = glob_match(text, text);
    }
});
