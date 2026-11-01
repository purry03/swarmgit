//! Daemon lifecycle and IPC server (DMN-1 to DMN-7).

use std::fs::{self, OpenOptions};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs2::FileExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use swarm_core::keyfile::NodeKey;
use swarm_store::{keyio, Db, Home};

use crate::handlers;
use crate::net::{self, NetState};
use crate::protocol::{Request, Response};
use crate::state::DaemonState;

async fn ensure_node_key(home: &Home) -> anyhow::Result<Arc<NodeKey>> {
    let path = home.node_key_path();
    if let Some(bytes) = keyio::read_private(&path)? {
        let key = NodeKey::from_bytes(&bytes)?;
        return Ok(Arc::new(key));
    }
    let key = NodeKey::generate()?;
    keyio::write_private_new(&path, &key.to_bytes())?;
    Ok(Arc::new(key))
}

fn acquire_lock(home: &Home) -> anyhow::Result<Option<std::fs::File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.lock_path())?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err.into()),
    }
}

async fn handle_connection(state: Arc<DaemonState>, stream: UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if line.len() > 8 * 1024 * 1024 {
            let response = Response::error(0, "limit_exceeded", "request line is too large");
            if let Ok(mut bytes) = serde_json::to_vec(&response) {
                bytes.push(b'\n');
                let _ = write_half.write_all(&bytes).await;
            }
            break;
        }
        state.touch();
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                let response = Response::error(0, "malformed", err.to_string());
                if let Ok(mut bytes) = serde_json::to_vec(&response) {
                    bytes.push(b'\n');
                    let _ = write_half.write_all(&bytes).await;
                }
                break;
            }
        };
        let stop = handlers::is_stop(&request.cmd);
        let response = handlers::handle(&state, request).await;
        let mut bytes = match serde_json::to_vec(&response) {
            Ok(bytes) => bytes,
            Err(_) => break,
        };
        bytes.push(b'\n');
        if write_half.write_all(&bytes).await.is_err() {
            break;
        }
        let _ = write_half.flush().await;
        if stop {
            state.shutdown.notify_one();
            break;
        }
    }
    state.client_left();
}

async fn idle_monitor(state: Arc<DaemonState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let minutes = state.idle_exit_minutes();
        if minutes == 0 {
            continue;
        }
        if state.active_clients.load(Ordering::SeqCst) != 0 {
            continue;
        }
        let idle = state
            .last_activity
            .lock()
            .map(|last| last.elapsed())
            .unwrap_or_default();
        if idle < Duration::from_secs(minutes * 60) {
            continue;
        }
        let seeded = {
            let Ok(db) = state.db.lock() else {
                continue;
            };
            db.repos()
                .map(|rows| rows.iter().any(|row| row.seeding))
                .unwrap_or(true)
        };
        if !seeded {
            state.shutdown.notify_one();
            break;
        }
    }
}

async fn signal_monitor(state: Arc<DaemonState>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(_) => return,
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    state.shutdown.notify_one();
}

async fn wait_for_clients(state: &DaemonState, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while state.active_clients.load(Ordering::SeqCst) != 0 {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn seeded_sync_loop(state: Arc<DaemonState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        let Some(net) = state.net().cloned() else {
            continue;
        };
        let repos = {
            let Ok(db) = net.db.lock() else {
                continue;
            };
            db.repos().unwrap_or_default()
        };
        for row in repos.into_iter().filter(|row| row.seeding) {
            if let Err(err) = crate::sync::sync_repo(&net, row.id, Vec::new()).await {
                tracing::debug!(code = err.code(), error = %err, "seeded sync failed");
            }
        }
    }
}

/// Run the daemon until stopped. A second instance exits successfully.
pub async fn run(home: Home) -> anyhow::Result<()> {
    home.ensure()?;
    let Some(lock) = acquire_lock(&home)? else {
        return Ok(());
    };
    let node = ensure_node_key(&home).await?;
    let db = Arc::new(Mutex::new(Db::open(&home.db_path())?));
    let state = Arc::new(DaemonState::new(home.clone(), Arc::clone(&db), lock));

    // DMN-5: open sqlite and clear temporary refs before serving.
    let existing = {
        let db = state
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        db.repos()?
    };
    for row in existing {
        let store = swarm_store::RepoStore::new(home.clone(), row.id);
        let _ = store.remove_temp_refs().await;
        let _ = store.clean_temp_manifests();
    }

    let (tcp_listener, listen_addr) = net::bind(net::configured_listen()).await?;
    let net = NetState::new(
        home.clone(),
        Arc::clone(&db),
        node,
        listen_addr,
        net::max_pack_bytes(),
    )?;
    state.set_net(Arc::clone(&net));
    net::write_daemon_json(&home, listen_addr)?;
    tracing::info!(
        home = %home.root().display(),
        listen = %listen_addr,
        node = %net.node_id.to_hex(),
        "peer listener started"
    );
    let peer_shutdown = Arc::new(tokio::sync::Notify::new());
    {
        let net = Arc::clone(&net);
        let shutdown = Arc::clone(&peer_shutdown);
        tokio::spawn(async move { net::serve::serve(net, tcp_listener, shutdown).await });
    }

    if lan_disabled() {
        tracing::info!("lan discovery disabled by SWARM_NO_LAN");
    } else {
        start_lan(&net).await;
    }
    net::dht::start(&net).await;
    net::upnp::start(&net);

    let socket = home.socket_path();
    if socket.exists() {
        let _ = fs::remove_file(&socket);
    }
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(&socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    }

    tracing::info!(home = %home.root().display(), "daemon started");
    let accept_state = Arc::clone(&state);
    tokio::spawn(async move { signal_monitor(accept_state).await });
    let idle_state = Arc::clone(&state);
    tokio::spawn(async move { idle_monitor(idle_state).await });
    let seeded_state = Arc::clone(&state);
    tokio::spawn(async move { seeded_sync_loop(seeded_state).await });

    loop {
        tokio::select! {
            _ = state.shutdown.notified() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        state.client_entered();
                        let connection_state = Arc::clone(&state);
                        tokio::spawn(async move {
                            handle_connection(connection_state, stream).await;
                        });
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "ipc accept failed");
                    }
                }
            }
        }
    }

    peer_shutdown.notify_waiters();
    net.persist_dht_nodes().await;
    wait_for_clients(&state, Duration::from_secs(5)).await;
    drop(listener);
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(home.daemon_json_path());
    tracing::info!("daemon stopped");
    Ok(())
}

fn lan_disabled() -> bool {
    std::env::var("SWARM_NO_LAN").is_ok_and(|value| value == "1")
}

/// Start route B (6.3). A bind failure leaves the other routes working.
async fn start_lan(net: &Arc<NetState>) {
    let weak = Arc::downgrade(net);
    let repos: swarm_disc::lan::RepoSource = Arc::new(move || {
        let Some(net) = weak.upgrade() else {
            return swarm_disc::lan::LanRepos::default();
        };
        swarm_disc::lan::LanRepos {
            known: net.known_repos(),
            seeded: net.serving_repos(),
        }
    });
    let config = swarm_disc::lan::LanConfig::from_env(net.listen.port());
    match swarm_disc::lan::LanService::start(config, repos).await {
        Ok(lan) => {
            tracing::info!(port = lan.port(), "lan discovery started");
            net.set_lan(lan);
        }
        Err(err) => tracing::warn!(error = %err, "lan discovery unavailable"),
    }
}
