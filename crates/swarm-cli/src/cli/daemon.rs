//! `swarm daemon`: run, start, stop and restart.

use std::time::Duration;

use clap::CommandFactory;
use serde_json::{json, Value};
use swarm_daemon::client::ClientError;
use swarm_daemon::Client;
use swarm_store::Home;

use super::args::{Cli, DaemonCmd};
use super::client_code;

/// How long a daemon may take to finish its work and exit.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

fn fail(err: &ClientError) -> i32 {
    eprintln!("swarm: {err}");
    client_code(err)
}

fn pid_text(hello: &Value) -> String {
    hello
        .get("pid")
        .and_then(Value::as_u64)
        .map(|pid| format!(" (pid {pid})"))
        .unwrap_or_default()
}

/// Ask a running daemon to exit and wait until it has. False if none ran.
async fn stop(home: &Home) -> Result<bool, i32> {
    let mut client = match Client::attach(home.clone()).await {
        Ok(client) => client,
        Err(ClientError::Unreachable(_)) => return Ok(false),
        Err(err) => return Err(fail(&err)),
    };
    if let Err(err) = client.call("stop", json!({})).await {
        return Err(fail(&err));
    }
    drop(client);
    match swarm_daemon::daemon::wait_until_stopped(home, STOP_TIMEOUT).await {
        Ok(true) => Ok(true),
        Ok(false) => {
            eprintln!(
                "swarm: daemon is still running after {}s",
                STOP_TIMEOUT.as_secs()
            );
            Err(1)
        }
        Err(err) => {
            eprintln!("swarm: {err}");
            Err(1)
        }
    }
}

async fn start(home: Home) -> i32 {
    match Client::attach(home.clone()).await {
        Ok(mut client) => {
            let hello = match client.call("hello", json!({})).await {
                Ok(hello) => hello,
                Err(err) => return fail(&err),
            };
            let version = hello.get("daemon_version").and_then(Value::as_str);
            if version != Some(env!("CARGO_PKG_VERSION")) {
                eprintln!(
                    "swarm: daemon is already running{}; run `swarm daemon restart` to apply changes",
                    pid_text(&hello)
                );
            } else {
                eprintln!("swarm: daemon is already running{}", pid_text(&hello));
            }
            return 0;
        }
        Err(ClientError::Unreachable(_)) => {}
        Err(err) => return fail(&err),
    }
    let mut client = match Client::connect(home).await {
        Ok(client) => client,
        Err(err) => return fail(&err),
    };
    match client.call("hello", json!({})).await {
        Ok(hello) => {
            eprintln!("swarm: daemon started{}", pid_text(&hello));
            0
        }
        Err(err) => fail(&err),
    }
}

/// `run_here` is `--foreground`, or `--detach` as used by autostart.
pub(super) async fn daemon(run_here: bool, action: Option<DaemonCmd>) -> i32 {
    if action.is_none() && !run_here {
        let mut command = Cli::command();
        command.build();
        if let Some(daemon) = command.find_subcommand_mut("daemon") {
            eprint!("{}", daemon.render_help());
        }
        return 2;
    }
    if action.is_some() && run_here {
        eprintln!("swarm: --foreground cannot be combined with start, stop or restart");
        return 2;
    }
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    match action {
        None => match swarm_daemon::daemon::run(home).await {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("swarm: {err}");
                1
            }
        },
        Some(DaemonCmd::Start) => start(home).await,
        Some(DaemonCmd::Stop) => match stop(&home).await {
            Ok(true) => {
                eprintln!("swarm: daemon stopped");
                0
            }
            Ok(false) => {
                eprintln!("swarm: daemon is not running");
                0
            }
            Err(code) => code,
        },
        Some(DaemonCmd::Restart) => match stop(&home).await {
            Ok(_) => start(home).await,
            Err(code) => code,
        },
    }
}
