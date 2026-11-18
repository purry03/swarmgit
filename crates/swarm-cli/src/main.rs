//! The single `swarm` binary: CLI, remote helper and daemon entry.

#![forbid(unsafe_code)]

mod cli;
mod helper;

use std::path::Path;

fn init_tracing() {
    let filter = std::env::var("SWARM_LOG").unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() {
    // Settings come from the environment first, then the YAML config files
    // (see `swarm_store::config`). Load them before any thread exists.
    if let Err(err) = swarm_store::config::load_into_env() {
        eprintln!("swarm: {err}");
        std::process::exit(2);
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run()),
        Err(err) => {
            eprintln!("swarm: cannot start the async runtime: {err}");
            std::process::exit(1);
        }
    }
}

async fn run() {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let code = if name == "git-remote-swarm" {
        helper::run(args).await
    } else {
        cli::run(args).await
    };
    std::process::exit(code);
}
