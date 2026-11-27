//! Text layout for `swarm status`.

use std::fmt::Write as _;
use std::net::SocketAddr;

use serde_json::Value;

const GREEN: &str = "32";
const YELLOW: &str = "33";
const RED: &str = "31";
const BOLD: &str = "1";

#[derive(Clone, Copy)]
pub(super) struct Style {
    pub(super) color: bool,
}

impl Style {
    fn paint(self, code: &str, text: &str) -> String {
        if self.color && !code.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub(super) fn bold(self, text: &str) -> String {
        self.paint(BOLD, text)
    }
}

/// A table cell: its text and a color code, empty for none.
type Cell = (String, &'static str);

fn plain(text: impl Into<String>) -> Cell {
    (text.into(), "")
}

/// Peers and relays choose some of this text, so keep control characters
/// (terminal escapes) out of it.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect()
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// The start of a long ID, for places that only need to tell them apart.
fn short(id: &str) -> String {
    match id.char_indices().nth(12) {
        Some((end, _)) => format!("{}…", &id[..end]),
        None => id.to_string(),
    }
}

fn table(out: &mut String, style: Style, indent: &str, header: &[&str], rows: &[Vec<Cell>]) {
    let mut widths: Vec<usize> = header.iter().map(|name| name.len()).collect();
    for row in rows {
        for (width, (value, _)) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.chars().count());
        }
    }
    let header: Vec<Cell> = header.iter().map(|name| (name.to_string(), BOLD)).collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut line = indent.to_string();
        for (column, ((value, code), width)) in row.iter().zip(&widths).enumerate() {
            line.push_str(&style.paint(code, value));
            // No trailing spaces after the last column.
            if column + 1 < row.len() {
                let pad = width - value.chars().count() + 2;
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

fn field(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "{name:<9} {value}");
}

pub(super) fn daemon(out: &mut String, style: Style, hello: &Value, status: &Value) {
    let mut about = Vec::new();
    if let Some(pid) = number(hello, "pid") {
        about.push(format!("pid {pid}"));
    }
    if let Some(version) = text(hello, "daemon_version") {
        about.push(format!("version {version}"));
    }
    field(
        out,
        "Daemon",
        &format!("{} ({})", style.paint(GREEN, "running"), about.join(", ")),
    );
    field(out, "Listen", text(hello, "listen").unwrap_or("?"));
    field(out, "Node", text(hello, "node_id").unwrap_or("?"));
    let lan = status.get("lan").unwrap_or(&Value::Null);
    match number(lan, "port") {
        Some(port) if flag(lan, "enabled") => field(out, "LAN", &format!("on (port {port})")),
        _ => field(out, "LAN", "off"),
    }
    let network = status.get("network").unwrap_or(&Value::Null);
    if let Some(proxy) = text(network, "proxy") {
        field(out, "Proxy", &clean(proxy));
    }
    if let Some(onion) = text(network, "onion") {
        field(out, "Onion", &clean(onion));
    }
    if let Some(error) = text(network, "onion_error") {
        let error = format!("not hosted: {}", clean(error));
        field(out, "Onion", &style.paint(RED, &error));
    }
}

fn seeders(repo: &Value) -> String {
    match number(repo, "seeders") {
        Some(1) if flag(repo, "seeding") => "1 (only you)".to_string(),
        Some(found) => found.to_string(),
        None if flag(repo, "checking") => "checking".to_string(),
        None => "?".to_string(),
    }
}

/// The most serious thing wrong with a repo.
fn state(repo: &Value) -> Cell {
    if text(repo, "error").is_some() {
        ("files missing".into(), RED)
    } else if !list(repo, "equivocations").is_empty() {
        ("equivocation".into(), RED)
    } else if !list(repo, "conflicts").is_empty() {
        ("conflict".into(), YELLOW)
    } else if !list(repo, "deleted_folders").is_empty() {
        ("folder deleted".into(), YELLOW)
    } else {
        ("ok".into(), GREEN)
    }
}

/// One line for each problem of a repo.
fn problems(repo: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(error) = text(repo, "error") {
        lines.push(clean(error));
    }
    for folder in list(repo, "deleted_folders") {
        let folder = clean(folder.as_str().unwrap_or("?"));
        lines.push(format!("project folder deleted: {folder}"));
    }
    for name in list(repo, "conflicts") {
        lines.push(format!("conflict {}", clean(name.as_str().unwrap_or("?"))));
    }
    for item in list(repo, "equivocations") {
        lines.push(format!(
            "equivocation {} seq={}",
            text(item, "signer").unwrap_or("?"),
            number(item, "seq").unwrap_or(0),
        ));
    }
    lines
}

pub(super) fn repos(out: &mut String, style: Style, status: &Value, indent: &str) {
    let repos = list(status, "repos");
    if repos.is_empty() {
        let _ = writeln!(out, "{indent}none; see `swarm init` and `swarm seed`");
        return;
    }
    let rows: Vec<Vec<Cell>> = repos
        .iter()
        .map(|repo| {
            vec![
                plain(text(repo, "repo").unwrap_or("?")),
                plain(number(repo, "seq").unwrap_or(0).to_string()),
                plain(number(repo, "refs").unwrap_or(0).to_string()),
                plain(if flag(repo, "seeding") { "yes" } else { "no" }),
                plain(seeders(repo)),
                plain(number(repo, "checked_secs_ago").map_or_else(|| "-".into(), age)),
                state(repo),
            ]
        })
        .collect();
    let header = [
        "REPO", "SEQ", "REFS", "SEEDING", "SEEDERS", "CHECKED", "STATE",
    ];
    table(out, style, indent, &header, &rows);
    for repo in repos {
        let id = short(text(repo, "repo").unwrap_or("?"));
        for problem in problems(repo) {
            let _ = writeln!(out, "{indent}! {id} {problem}");
        }
    }
}

pub(super) fn repo_detail(out: &mut String, style: Style, status: &Value) {
    for repo in list(status, "repos") {
        field(out, "Repo", text(repo, "repo").unwrap_or("?"));
        field(out, "Link", text(repo, "link").unwrap_or("?"));
        field(out, "Root", text(repo, "root").unwrap_or("?"));
        field(out, "Format", text(repo, "object_format").unwrap_or("?"));
        field(out, "Seq", &number(repo, "seq").unwrap_or(0).to_string());
        field(out, "Refs", &number(repo, "refs").unwrap_or(0).to_string());
        field(
            out,
            "Recovery",
            if flag(repo, "recovery") {
                "set"
            } else {
                "none"
            },
        );
        field(
            out,
            "Seeding",
            if flag(repo, "seeding") { "yes" } else { "no" },
        );
        let checked = number(repo, "checked_secs_ago")
            .map(|secs| format!(", checked {}", age(secs)))
            .unwrap_or_default();
        field(out, "Seeders", &format!("{}{checked}", seeders(repo)));
        let (label, code) = state(repo);
        field(out, "State", &style.paint(code, &label));
        for problem in problems(repo) {
            let _ = writeln!(out, "  ! {problem}");
        }
    }
}

pub(super) fn relays(out: &mut String, style: Style, status: &Value, indent: &str) {
    let relays = list(status, "relays");
    if relays.is_empty() {
        let _ = writeln!(out, "{indent}none in use");
        return;
    }
    let rows: Vec<Vec<Cell>> = relays
        .iter()
        .map(|relay| {
            let (state, detail) = if flag(relay, "registered") {
                let seen = text(relay, "observed").map(|addr| format!("sees us as {addr}"));
                (("registered".to_string(), GREEN), seen.unwrap_or_default())
            } else {
                let why = text(relay, "error").unwrap_or("connecting");
                (("not registered".to_string(), RED), why.to_string())
            };
            vec![
                plain(clean(&relay_label(relay))),
                state,
                plain(clean(&detail)),
            ]
        })
        .collect();
    table(out, style, indent, &["RELAY", "STATE", "DETAIL"], &rows);
}

/// The relay's name, followed by the IP it resolved to when the name is not
/// that IP already.
fn relay_label(relay: &Value) -> String {
    let name = text(relay, "relay").unwrap_or("?");
    let ip = text(relay, "address")
        .and_then(|addr| addr.parse::<SocketAddr>().ok())
        .map(|addr| addr.ip());
    let host = name.split('#').next().unwrap_or(name);
    match ip {
        Some(ip) if host.parse::<SocketAddr>().is_err() => format!("{name} ({ip})"),
        _ => name.to_string(),
    }
}

pub(super) fn peers(out: &mut String, style: Style, result: &Value) {
    let peers = list(result, "peers");
    if peers.is_empty() {
        let _ = writeln!(out, "no peers known yet");
        return;
    }
    // The daemon lists the peers of each repo together.
    let groups = peers.chunk_by(|a, b| text(a, "repo") == text(b, "repo"));
    for (index, group) in groups.enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let repo = group.first().and_then(|peer| text(peer, "repo"));
        let _ = writeln!(out, "{}", style.bold(repo.unwrap_or("?")));
        let rows: Vec<Vec<Cell>> = group
            .iter()
            .map(|peer| {
                let state = if flag(peer, "connected") {
                    let via = text(peer, "relay")
                        .map(|relay| format!(" via {}", clean(relay)))
                        .unwrap_or_default();
                    (format!("connected{via}"), GREEN)
                } else {
                    match number(peer, "fails") {
                        Some(0) | None => plain("idle"),
                        Some(fails) => (format!("failing ({fails})"), YELLOW),
                    }
                };
                vec![
                    plain(text(peer, "addr").map_or_else(|| "-".into(), clean)),
                    plain(text(peer, "node").map_or_else(|| "-".into(), short)),
                    state,
                    plain(number(peer, "last_ok_secs_ago").map_or_else(|| "never".into(), age)),
                ]
            })
            .collect();
        table(
            out,
            style,
            "  ",
            &["ADDRESS", "NODE", "STATE", "LAST OK"],
            &rows,
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const PLAIN: Style = Style { color: false };

    #[test]
    fn relays_are_one_aligned_row_each_without_escapes() {
        let mut out = String::new();
        let status = json!({ "relays": [
            { "relay": "relay:42425", "registered": true },
            { "relay": "b:1", "registered": false, "error": "no\u{1b}[2J" },
        ]});
        relays(&mut out, PLAIN, &status, "");
        assert_eq!(
            out,
            "RELAY        STATE           DETAIL\n\
             relay:42425  registered\n\
             b:1          not registered  no?[2J\n"
        );
    }

    #[test]
    fn a_named_relay_shows_the_ip_it_resolved_to() {
        let label =
            |relay: &str| relay_label(&json!({ "relay": relay, "address": "93.184.216.34:42425" }));
        assert_eq!(
            label("relay.example:42425"),
            "relay.example:42425 (93.184.216.34)"
        );
        assert_eq!(label("93.184.216.34:42425"), "93.184.216.34:42425");
    }

    #[test]
    fn a_repo_with_a_conflict_says_which_ref() {
        let mut out = String::new();
        let status = json!({ "repos": [{
            "repo": "abcdefghijklmnop", "seq": 3, "refs": 1, "seeding": true,
            "seeders": 1, "checked_secs_ago": 90,
            "conflicts": ["refs/heads/main"],
        }]});
        repos(&mut out, PLAIN, &status, "");
        assert!(out.contains("1 (only you)  1m ago   conflict\n"), "{out}");
        assert!(
            out.ends_with("! abcdefghijkl… conflict refs/heads/main\n"),
            "{out}"
        );
    }
}
