//! A local Mainline DHT testnet for end-to-end tests (M4).
//!
//! Starts N nodes on 127.0.0.1 (default 10), prints their addresses as one
//! comma-separated line for `SWARM_DHT_BOOTSTRAP`, then runs until killed.

use std::io::Write;
use std::net::Ipv4Addr;

fn main() {
    let count = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10);
    let testnet = match mainline::Testnet::builder(count)
        .bind_address(Ipv4Addr::LOCALHOST)
        .build()
    {
        Ok(testnet) => testnet,
        Err(err) => {
            eprintln!("swarm-dht-testnet: {err}");
            std::process::exit(1);
        }
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", testnet.bootstrap.join(","));
    let _ = out.flush();
    loop {
        std::thread::park();
    }
}
