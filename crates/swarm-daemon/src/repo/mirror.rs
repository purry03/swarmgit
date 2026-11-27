//! Static mirrors (spec 11.9, MIR-1..4).
//!
//! A mirror is a plain file host:
//!
//! ```text
//! <base>/<repo-id>/repo.swarmbundle     (a bundle, 6.6)
//! ```
//!
//! It is only a transport. Whatever it serves goes through bundle import, so a
//! tampered or stale mirror is rejected or ignored like any other source.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use swarm_core::ids::{Hint, HintScheme, RepoId};
use swarm_store::RepoStore;

use super::bundle::write_bundle;
use super::{arg_str, db_repo, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// Largest bundle a mirror may serve (MIR-3).
const MAX_BUNDLE: u64 = 4 << 30;
const FILE: &str = "repo.swarmbundle";

/// Base URLs from link hints plus `SWARM_MIRRORS`.
pub(crate) fn bases(hints: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in hints {
        let Some(hint) = Hint::parse(raw) else {
            continue;
        };
        let scheme = match hint.scheme {
            HintScheme::Https => "https",
            HintScheme::Http => "http",
            _ => continue,
        };
        out.push(format!("{scheme}://{}:{}", hint.host, hint.port));
    }
    if let Ok(list) = std::env::var("SWARM_MIRRORS") {
        out.extend(
            list.split(',')
                .map(|item| item.trim().trim_end_matches('/').to_string())
                .filter(|item| item.starts_with("https://") || item.starts_with("http://")),
        );
    }
    out.dedup();
    out
}

/// Download `<base>/<repo>/repo.swarmbundle` to `dest`.
pub(crate) async fn download(base: &str, repo: RepoId, dest: &Path) -> Result<u64, String> {
    let url = format!("{}/{}/{FILE}", base.trim_end_matches('/'), repo.to_base32());
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30));
        // M9: HTTP goes through the proxy too. ureq hands names to a SOCKS5
        // proxy unresolved.
        if let Some(proxy) = swarm_net::Proxy::from_env().map_err(|err| err.to_string())? {
            let auth = match &proxy.auth {
                Some((user, password)) => format!("{user}:{password}@"),
                None => String::new(),
            };
            let socks = ureq::Proxy::new(format!("socks5://{auth}{}", proxy.server))
                .map_err(|err| format!("proxy: {err}"))?;
            builder = builder.proxy(socks);
        }
        let agent = builder.build();
        let response = agent
            .get(&url)
            .call()
            .map_err(|err| format!("{url}: {err}"))?;
        let mut body = response.into_reader().take(MAX_BUNDLE + 1);
        let mut out = std::fs::File::create(&dest).map_err(|err| err.to_string())?;
        let bytes = std::io::copy(&mut body, &mut out).map_err(|err| format!("{url}: {err}"))?;
        if bytes > MAX_BUNDLE {
            return Err(format!("{url}: larger than the {MAX_BUNDLE} byte limit"));
        }
        Ok(bytes)
    })
    .await
    .map_err(|err| err.to_string())?
}

/// `mirror_export`: write `<dir>/<repo-id>/repo.swarmbundle` for a repo.
pub async fn mirror_export(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let dir = PathBuf::from(arg_str(args, "dir")?);
    let _ = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    if !store.exists() {
        return Err(IpcError::new("unknown_repo", "store does not exist"));
    }
    let target = dir.join(repo.to_base32());
    std::fs::create_dir_all(&target).map_err(|err| IpcError::new("malformed", err.to_string()))?;
    let file = target.join(FILE);
    let written = write_bundle(&store, &file).await?;
    Ok(json!({
        "path": file.to_string_lossy(),
        "bytes": written.bytes,
        "manifests": written.manifests,
    }))
}
