//! Hidden-service hosting through the Tor control port (spec 11.10).
//!
//! The daemon asks Tor for an onion service that forwards to its peer
//! listener, keeps the control connection open (the service lives as long as
//! it does) and puts an `onion:` hint in its links. The service key is kept in
//! `<home>/keys/onion.key`, so the address survives restarts.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use swarm_store::keyio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::NetState;

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long startup waits, so the first link already names the service.
const STARTUP_WAIT: Duration = Duration::from_secs(10);
const MAX_REPLY_LINES: usize = 64;

struct Control {
    stream: BufReader<TcpStream>,
}

impl Control {
    /// Send one command and collect the reply lines of a `250` answer.
    async fn command(&mut self, command: &str) -> anyhow::Result<Vec<String>> {
        let line = format!("{command}\r\n");
        self.stream.get_mut().write_all(line.as_bytes()).await?;
        let mut lines = Vec::new();
        for _ in 0..MAX_REPLY_LINES {
            let mut line = String::new();
            let read = tokio::time::timeout(REPLY_TIMEOUT, self.stream.read_line(&mut line));
            if read.await?? == 0 {
                anyhow::bail!("Tor closed the control connection");
            }
            let line = line.trim_end().to_string();
            let Some(rest) = line.strip_prefix("250") else {
                // The command itself may hold a password or key.
                anyhow::bail!("Tor refused the request: {line}");
            };
            let done = rest.starts_with(' ');
            lines.push(rest.get(1..).unwrap_or_default().to_string());
            if done {
                return Ok(lines);
            }
        }
        anyhow::bail!("Tor reply is too long")
    }
}

/// The `AUTHENTICATE` argument: a password, the cookie file or nothing.
async fn credentials(control: &mut Control, password: Option<&str>) -> anyhow::Result<String> {
    if let Some(password) = password {
        let escaped = password.replace('\\', "\\\\").replace('"', "\\\"");
        return Ok(format!(" \"{escaped}\""));
    }
    let info = control.command("PROTOCOLINFO 1").await?;
    let Some(auth) = info.iter().find(|line| line.starts_with("AUTH ")) else {
        return Ok(String::new());
    };
    if auth
        .split_whitespace()
        .any(|word| word.strip_prefix("METHODS=").is_some_and(has_null))
    {
        return Ok(String::new());
    }
    let Some(cookie) = cookie_path(auth) else {
        anyhow::bail!("Tor wants a password: set onion_password");
    };
    // A 32-byte file: not worth leaving the task for.
    let bytes = std::fs::read(&cookie)
        .map_err(|err| anyhow::anyhow!("cannot read the Tor cookie {cookie}: {err}"))?;
    Ok(format!(" {}", hex::encode(bytes)))
}

fn has_null(methods: &str) -> bool {
    methods.split(',').any(|method| method == "NULL")
}

fn cookie_path(auth: &str) -> Option<String> {
    let rest = auth.split_once("COOKIEFILE=\"")?.1;
    let mut path = String::new();
    let mut chars = rest.chars();
    while let Some(next) = chars.next() {
        match next {
            '"' => return Some(path),
            '\\' => path.push(chars.next()?),
            other => path.push(other),
        }
    }
    None
}

fn key_path(net: &NetState) -> PathBuf {
    net.home.root().join("keys").join("onion.key")
}

/// Open the control connection and add the service, forwarding to `target`.
/// Returns the connection and the `<id>.onion` host.
async fn publish(
    net: &NetState,
    server: &str,
    target: SocketAddr,
) -> anyhow::Result<(Control, String)> {
    let stream = tokio::time::timeout(REPLY_TIMEOUT, TcpStream::connect(server)).await??;
    let mut control = Control {
        stream: BufReader::new(stream),
    };
    let auth = credentials(&mut control, net.settings.onion_password.as_deref()).await?;
    control.command(&format!("AUTHENTICATE{auth}")).await?;
    let path = key_path(net);
    let saved = keyio::read_private(&path)?.and_then(|bytes| String::from_utf8(bytes).ok());
    let key = saved
        .as_deref()
        .map_or("NEW:ED25519-V3", str::trim)
        .to_string();
    let reply = control
        .command(&format!(
            "ADD_ONION {key} Port={},{target}",
            net.listen.port()
        ))
        .await?;
    let field = |name: &str| {
        reply
            .iter()
            .find_map(|line| line.strip_prefix(name).map(str::to_string))
    };
    let Some(id) = field("ServiceID=") else {
        anyhow::bail!("Tor did not name the new service");
    };
    if !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) || id.len() > 56 {
        anyhow::bail!("Tor named the service strangely");
    }
    if saved.is_none() {
        if let Some(private) = field("PrivateKey=") {
            keyio::write_private_new(&path, private.as_bytes())?;
        }
    }
    Ok((control, format!("{id}.onion")))
}

/// Host the service for as long as the daemon runs. Returns once the first
/// attempt has succeeded, failed, or taken too long.
///
/// Tor forwards to a loopback listener of its own, so onion visitors are told
/// apart from local tools. That means Tor must run on this machine.
pub async fn start(net: &Arc<NetState>) {
    let Some(server) = net.settings.onion_control.clone() else {
        return;
    };
    let bound = async {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let target = listener.local_addr()?;
        std::io::Result::Ok((listener, target))
    };
    let (listener, target) = match bound.await {
        Ok(bound) => bound,
        Err(err) => {
            tracing::warn!(error = %err, "cannot host the hidden service");
            net.set_onion_error(err.to_string());
            return;
        }
    };
    tokio::spawn(super::serve::serve_onion(Arc::clone(net), listener));
    let (first, answered) = oneshot::channel();
    tokio::spawn(maintain(Arc::clone(net), server, target, first));
    let _ = tokio::time::timeout(STARTUP_WAIT, answered).await;
}

async fn maintain(
    net: Arc<NetState>,
    server: String,
    target: SocketAddr,
    first: oneshot::Sender<()>,
) {
    let mut first = Some(first);
    let mut delay = RETRY_MIN;
    loop {
        match publish(&net, &server, target).await {
            Ok((mut control, host)) => {
                tracing::info!(onion = %host, "hidden service is up");
                net.set_onion(Some(host));
                if let Some(first) = first.take() {
                    let _ = first.send(());
                }
                delay = RETRY_MIN;
                // The service lasts until this connection ends.
                let mut line = String::new();
                while control.stream.read_line(&mut line).await.unwrap_or(0) > 0 {
                    line.clear();
                }
                net.set_onion(None);
                tracing::warn!("lost the Tor control connection; hidden service is down");
            }
            Err(err) => {
                tracing::warn!(error = %err, "cannot host the hidden service");
                net.set_onion_error(err.to_string());
                if let Some(first) = first.take() {
                    let _ = first.send(());
                }
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_cookie_path_with_escapes() {
        let auth = r#"AUTH METHODS=COOKIE,SAFECOOKIE COOKIEFILE="/run/tor/a \"b\"/c""#;
        assert_eq!(cookie_path(auth).as_deref(), Some(r#"/run/tor/a "b"/c"#));
        assert_eq!(cookie_path("AUTH METHODS=HASHEDPASSWORD"), None);
        assert!(has_null("NULL,COOKIE") && !has_null("COOKIE"));
    }
}
