//! SEC-16: a peer that skips Phase B or reorders the handshake is dropped.

use std::time::Duration;

use swarm_core::ids::RepoId;
use swarm_store::Home;
use swarm_testpeer::modes::{probe, Mode};

fn free_port_file(home: &Home) -> std::path::PathBuf {
    home.daemon_json_path()
}

async fn wait_for_port(home: &Home) -> u16 {
    let path = free_port_file(home);
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(port) = value.get("port").and_then(serde_json::Value::as_u64) {
                    if port != 0 {
                        return port as u16;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not write daemon.json");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_phase_b_clients_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::from_root(dir.path());
    std::env::set_var("SWARM_LISTEN", "127.0.0.1:0");
    std::env::set_var("SWARM_IDLE_EXIT_MINUTES", "0");

    let run_home = home.clone();
    let handle = tokio::spawn(async move { swarm_daemon::daemon::run(run_home).await });
    let port = wait_for_port(&home).await;
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let repo = RepoId::from_bytes([1u8; 32]);

    for mode in [
        Mode::HelloFirst,
        Mode::SkipPhaseB,
        Mode::DoublePqInit,
        Mode::HelloBeforeAuth,
    ] {
        let outcome = probe(addr, mode, repo).await;
        match outcome {
            Ok(probe) => {
                assert!(probe.closed, "{} was not dropped", mode.name());
                assert!(
                    !probe.got_application_message,
                    "{} received an application message",
                    mode.name()
                );
            }
            Err(err) => panic!("{} probe failed: {err}", mode.name()),
        }
    }

    let mut client = swarm_daemon::Client::connect(home).await.unwrap();
    let _ = client.call("stop", serde_json::json!({})).await;
    let _ = handle.await;
}
