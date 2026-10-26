//! Shared daemon state.

use std::fs::File;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use tokio::sync::Notify;

use swarm_store::{Db, Home};

/// State shared by every IPC connection.
pub struct DaemonState {
    /// Data directory.
    pub home: Home,
    /// Sqlite handle, serialized by a mutex.
    pub db: Mutex<Db>,
    /// Signals the accept loop to stop.
    pub shutdown: Notify,
    /// Number of connected IPC clients.
    pub active_clients: AtomicUsize,
    /// Last request time, used by DMN-6.
    pub last_activity: Mutex<Instant>,
    /// Held for the process lifetime to enforce one daemon per home.
    _lock: File,
}

impl DaemonState {
    /// Build state from an already-locked file.
    #[must_use]
    pub fn new(home: Home, db: Db, lock: File) -> Self {
        Self {
            home,
            db: Mutex::new(db),
            shutdown: Notify::new(),
            active_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            _lock: lock,
        }
    }

    /// Mark activity for the idle timer.
    pub fn touch(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    /// Client connected.
    pub fn client_entered(&self) {
        self.active_clients.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    /// Client disconnected.
    pub fn client_left(&self) {
        self.active_clients.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    /// Idle minutes before exit, with a test override.
    #[must_use]
    pub fn idle_exit_minutes(&self) -> u64 {
        std::env::var("SWARM_IDLE_EXIT_MINUTES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30)
    }
}
