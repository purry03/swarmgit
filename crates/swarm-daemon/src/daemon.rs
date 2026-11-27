//! Daemon lifecycle and IPC server (DMN-1 to DMN-7).

use std::fs::{self, OpenOptions};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs2::FileExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

use swarm_core::ids::RepoId;
use swarm_core::keyfile::NodeKey;
use swarm_store::{keyio, Db, Home};

use crate::handlers;
use crate::net::{self, NetState};
use crate::protocol::{Request, Response};
use crate::state::DaemonState;
use crate::sync::progress::Progress;

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

/// Wait for the daemon of `home` to release its lock, which it does on exit.
/// Returns false if it still holds it after `timeout`.
pub async fn wait_until_stopped(home: &Home, timeout: Duration) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if acquire_lock(home)?.is_some() {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The next progress event, or never when nothing reports progress.
async fn next_event(
    events: &mut Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
) -> Option<serde_json::Value> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

async fn write_line(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await?;
    write_half.flush().await
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
        let id = request.id;
        let (progress, mut events) = if handlers::wants_progress(&request) {
            let (progress, events) = Progress::channel();
            (progress, Some(events))
        } else {
            (Progress::none(), None)
        };
        let response = {
            let work = handlers::handle_with(&state, request, progress);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    response = &mut work => break response,
                    event = next_event(&mut events) => match event {
                        Some(mut event) => {
                            event["id"] = id.into();
                            if write_line(&mut write_half, &event).await.is_err() {
                                state.client_left();
                                return;
                            }
                        }
                        None => events = None,
                    },
                }
            }
        };
        // Events sent just before the work finished.
        if let Some(mut rest) = events {
            while let Ok(mut event) = rest.try_recv() {
                event["id"] = id.into();
                if write_line(&mut write_half, &event).await.is_err() {
                    break;
                }
            }
        }
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

/// Stop seeding a repo whose store was deleted from disk. `swarm seed` turns
/// it back on.
async fn stop_if_missing(net: &Arc<NetState>, repo: RepoId) -> bool {
    let lock = net.sync_lock(repo);
    let _held = lock.lock().await;
    if net.store(repo).exists() {
        return false;
    }
    if let Ok(db) = net.db.lock() {
        let _ = db.set_seeding(repo, false);
    }
    net.close_sessions(repo);
    tracing::warn!(repo = %repo.to_base32(), "store is missing from disk; no longer seeding");
    true
}

/// Log a repo whose project folders are all gone. It stays seeded: the
/// daemon's own copy is intact, and a folder may only have been moved.
fn warn_if_folders_deleted(net: &NetState, repo: RepoId) {
    let paths = match net.db.lock() {
        Ok(db) => db.workdirs(repo).unwrap_or_default(),
        Err(_) => return,
    };
    if let Some(gone) = crate::repo::deleted_workdirs(&paths) {
        tracing::warn!(repo = %repo.to_base32(), folders = ?gone, "project folder deleted");
    }
}

async fn seeded_sync_loop(state: Arc<DaemonState>) {
    // The first check runs soon after start, so `status` has seeder counts.
    let mut wait = Duration::from_secs(10);
    loop {
        tokio::time::sleep(wait).await;
        wait = Duration::from_secs(300);
        let Some(net) = state.net().cloned() else {
            continue;
        };
        let repos = {
            let Ok(db) = net.db.lock() else {
                continue;
            };
            db.repos().unwrap_or_default()
        };
        check_repos(&net, repos).await;
    }
}

/// Repos checked at once: most of a check is waiting for peers to answer.
const MAX_CHECKS: usize = 8;

/// Sync every seeded repo, several at a time.
async fn check_repos(net: &Arc<NetState>, repos: Vec<swarm_store::RepoRow>) {
    let queue: Vec<RepoId> = repos
        .into_iter()
        .filter(|row| row.seeding)
        .map(|row| row.id)
        .collect();
    for &repo in &queue {
        net.set_checking(repo, true);
    }
    let mut running = JoinSet::new();
    for repo in queue {
        if running.len() >= MAX_CHECKS {
            let _ = running.join_next().await;
        }
        let net = Arc::clone(net);
        running.spawn(async move {
            check_repo(&net, repo).await;
            net.set_checking(repo, false);
        });
    }
    while running.join_next().await.is_some() {}
}

async fn check_repo(net: &Arc<NetState>, repo: RepoId) {
    // A sync would download a deleted store again.
    if stop_if_missing(net, repo).await {
        return;
    }
    warn_if_folders_deleted(net, repo);
    if let Err(err) = crate::sync::sync_repo(net, repo, Vec::new(), &Progress::none()).await {
        tracing::debug!(code = err.code(), error = %err, "seeded sync failed");
    }
}

/// Run the daemon until stopped. A second instance exits successfully.
pub async fn run(home: Home) -> anyhow::Result<()> {
    home.ensure()?;
    crate::net::settings::Settings::from_env()?;
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

    if let Some(proxy) = &net.settings.proxy {
        // UDP and multicast cannot go through SOCKS5, so those routes stay
        // off: nothing may leave around the proxy.
        tracing::info!(?proxy, "using a proxy; lan, dht and upnp are off");
    } else {
        if lan_disabled() {
            tracing::info!("lan discovery disabled by SWARM_NO_LAN");
        } else {
            start_lan(&net).await;
        }
        net::dht::start(&net).await;
        net::upnp::start(&net);
    }
    net::relay::start(&net);
    net::onion::start(&net).await;

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
