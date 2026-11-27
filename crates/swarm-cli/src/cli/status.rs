//! `swarm status` and its `repos`, `relays` and `peers` views.

use std::fmt::Write as _;
use std::io::{IsTerminal as _, Write as _};

use serde_json::{json, Value};
use swarm_daemon::client::ClientError;
use swarm_daemon::Client;
use swarm_store::Home;

use super::args::StatusCmd;
use super::{client_code, parse_repo};

mod render;
use render::Style;

fn fail(err: &ClientError) -> i32 {
    eprintln!("swarm: {err}");
    client_code(err)
}

/// Write the whole report at once. A reader that stops early, such as
/// `grep -q`, is not an error.
fn emit(text: &str) {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

pub(super) async fn status(
    json_output: bool,
    repo: Option<String>,
    view: Option<StatusCmd>,
) -> i32 {
    if repo.is_some() && view.is_some() {
        eprintln!("swarm: put the repo after the subcommand, as in `swarm status peers <repo>`");
        return 2;
    }
    let wanted = match &view {
        Some(StatusCmd::Repos { repo } | StatusCmd::Peers { repo }) => repo.clone(),
        Some(StatusCmd::Relays) => None,
        None => repo,
    };
    let mut args = json!({});
    if let Some(repo) = &wanted {
        match parse_repo(repo) {
            Ok(repo) => args["repo"] = Value::String(repo.to_base32()),
            Err(err) => {
                eprintln!("swarm: {err}");
                return 2;
            }
        }
    }
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    let mut client = match Client::connect(home).await {
        Ok(client) => client,
        Err(err) => return fail(&err),
    };
    let command = match view {
        Some(StatusCmd::Peers { .. }) => "peers",
        _ => "status",
    };
    let result = match client.call(command, args).await {
        Ok(result) => result,
        Err(err) => return fail(&err),
    };
    if json_output {
        // A view prints only its own section.
        let section = match view {
            Some(StatusCmd::Repos { .. }) => Some("repos"),
            Some(StatusCmd::Relays) => Some("relays"),
            _ => None,
        };
        match section {
            Some(key) => emit(&format!("{}\n", json!({ key: result.get(key) }))),
            None => emit(&format!("{result}\n")),
        }
        return 0;
    }
    let style = Style {
        color: std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
    };
    let mut out = String::new();
    match view {
        Some(StatusCmd::Peers { .. }) => render::peers(&mut out, style, &result),
        Some(StatusCmd::Relays) => render::relays(&mut out, style, &result, ""),
        Some(StatusCmd::Repos { .. }) if wanted.is_some() => {
            render::repo_detail(&mut out, style, &result);
        }
        Some(StatusCmd::Repos { .. }) => render::repos(&mut out, style, &result, ""),
        None if wanted.is_some() => render::repo_detail(&mut out, style, &result),
        None => {
            let hello = match client.call("hello", json!({})).await {
                Ok(hello) => hello,
                Err(err) => return fail(&err),
            };
            render::daemon(&mut out, style, &hello, &result);
            for (title, key) in [("Repositories", "repos"), ("Relays", "relays")] {
                let count = result
                    .get(key)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let _ = writeln!(out, "\n{}", style.bold(&format!("{title} ({count})")));
                if key == "repos" {
                    render::repos(&mut out, style, &result, "  ");
                } else {
                    render::relays(&mut out, style, &result, "  ");
                }
            }
        }
    }
    emit(&out);
    0
}
