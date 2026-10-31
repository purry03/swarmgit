#![no_main]

use libfuzzer_sys::fuzz_target;
use swarm_disc::lan::LanPacket;

fuzz_target!(|data: &[u8]| {
    if let Ok(packet) = LanPacket::decode(data) {
        // Strict decoding means every accepted packet re-encodes to itself.
        assert_eq!(packet.encode().as_deref(), Some(data));
    }
});
