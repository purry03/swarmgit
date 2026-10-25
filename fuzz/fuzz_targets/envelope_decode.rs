#![no_main]

use libfuzzer_sys::fuzz_target;
use swarm_core::{manifest::Manifest, verify_envelope};

fuzz_target!(|data: &[u8]| {
    let _ = verify_envelope(data);
    let _ = Manifest::decode(data);
});
