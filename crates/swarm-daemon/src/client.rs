//! IPC client and daemon autostart (DMN-2, DMN-4).

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::protocol::{ErrorBody, IpcError, Request, Response};
use swarm_store::Home;

/// Errors from the IPC client.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon could not be reached or started.
    #[error("daemon unreachable: {0}")]
    Unreachable(String),
    /// Socket I/O failed.
    #[error("ipc io: {0}")]
    Io(#[from] std::io::Error),
    /// The reply could not be parsed.
    #[error("ipc protocol: {0}")]
    Protocol(String),
    /// The daemon returned an error code.
    #[error("{0}")]
    Remote(#[from] IpcError),
}

/// A connected IPC client.
pub struct Client {
    reader: BufReader<UnixStream>,
    next_id: u64,
    home: Home,
}

fn swarm_binary() -> Result<std::path::PathBuf, ClientError> {
    if let Some(value) = std::env::var_os("SWARM_BIN") {
        if !value.is_empty() {
            return Ok(value.into());
        }
    }
    let exe = std::env::current_exe()
        .map_err(|err| ClientError::Unreachable(format!("cannot find swarm binary: {err}")))?;
    let name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name != "git-remote-swarm" {
        return Ok(exe);
    }
    let plain = if cfg!(windows) { "swarm.exe" } else { "swarm" };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join(plain);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(plain);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(ClientError::Unreachable(
        "cannot find the swarm binary next to git-remote-swarm".into(),
    ))
}

fn spawn_daemon(home: &Home) -> Result<(), ClientError> {
    let exe = swarm_binary()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.log_path())?;
    let stderr = log.try_clone()?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["daemon", "--detach"])
        .env("SWARM_HOME", home.root())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|err| ClientError::Unreachable(format!("cannot start daemon: {err}")))?;
    Ok(())
}

async fn open_or_start(home: &Home, autostart: bool) -> Result<UnixStream, ClientError> {
    let socket = home.socket_path();
    if let Ok(stream) = UnixStream::connect(&socket).await {
        return Ok(stream);
    }
    if !autostart {
        return Err(ClientError::Unreachable(format!(
            "no daemon at {}",
            socket.display()
        )));
    }
    spawn_daemon(home)?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(stream) = UnixStream::connect(&socket).await {
            return Ok(stream);
        }
    }
    Err(ClientError::Unreachable(format!(
        "daemon did not answer on {}; see {}",
        socket.display(),
        home.log_path().display()
    )))
}

async fn read_reply(reader: &mut BufReader<UnixStream>, id: u64) -> Result<Response, ClientError> {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Err(ClientError::Protocol("daemon closed the connection".into()));
        }
        if line.len() > 8 * 1024 * 1024 {
            return Err(ClientError::Protocol("reply line is too large".into()));
        }
        let value: Value =
            serde_json::from_str(&line).map_err(|err| ClientError::Protocol(err.to_string()))?;
        if value.get("event").is_some() {
            continue;
        }
        let response: Response =
            serde_json::from_value(value).map_err(|err| ClientError::Protocol(err.to_string()))?;
        if response.id == id {
            return Ok(response);
        }
    }
}

impl Client {
    /// Connect, starting the daemon when necessary, and check versions.
    pub async fn connect(home: Home) -> Result<Self, ClientError> {
        Self::connect_inner(home, true).await
    }

    /// Connect only if a daemon is already running.
    pub async fn connect_existing(home: Home) -> Result<Self, ClientError> {
        Self::connect_inner(home, false).await
    }

    async fn connect_inner(home: Home, autostart: bool) -> Result<Self, ClientError> {
        for attempt in 0..2 {
            let stream = open_or_start(&home, autostart).await?;
            let mut client = Self {
                reader: BufReader::new(stream),
                next_id: 1,
                home: home.clone(),
            };
            let hello = client
                .call(
                    "hello",
                    json!({ "client_version": env!("CARGO_PKG_VERSION") }),
                )
                .await?;
            let daemon_version = hello
                .get("daemon_version")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if daemon_version == env!("CARGO_PKG_VERSION") {
                return Ok(client);
            }
            if attempt == 0 && autostart {
                let _ = client.call("stop", json!({})).await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            return Err(ClientError::Protocol(format!(
                "daemon version {daemon_version} does not match client {}",
                env!("CARGO_PKG_VERSION")
            )));
        }
        Err(ClientError::Protocol("version restart loop".into()))
    }

    /// The home this client talks to.
    #[must_use]
    pub fn home(&self) -> &Home {
        &self.home
    }

    /// Send one command and wait for its reply.
    pub async fn call(&mut self, cmd: &str, args: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request {
            id,
            cmd: cmd.to_string(),
            args,
        };
        let mut line =
            serde_json::to_vec(&request).map_err(|err| ClientError::Protocol(err.to_string()))?;
        line.push(b'\n');
        self.reader.get_mut().write_all(&line).await?;
        self.reader.get_mut().flush().await?;
        let response = read_reply(&mut self.reader, id).await?;
        if response.ok {
            return Ok(response.result.unwrap_or(Value::Null));
        }
        let ErrorBody { code, msg } = response.error.unwrap_or_else(|| ErrorBody {
            code: "malformed".into(),
            msg: "daemon returned no error body".into(),
        });
        Err(ClientError::Remote(IpcError::new(code, msg)))
    }

    /// Send `stop` and ignore a closed socket.
    pub async fn stop(mut self) -> Result<(), ClientError> {
        let _ = self.call("stop", json!({})).await;
        Ok(())
    }
}

/// One-shot helper: connect, run one command, return the result.
pub async fn call(home: Home, cmd: &str, args: Value) -> Result<Value, ClientError> {
    let mut client = Client::connect(home).await?;
    client.call(cmd, args).await
}
