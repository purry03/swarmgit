//! YAML configuration files, so settings do not have to be passed as
//! environment variables.
//!
//! Files are read in this order, and the first source to set a value wins:
//!
//! 1. the environment (`SWARM_*`),
//! 2. `$SWARM_CONFIG`, when set (it must exist), otherwise
//!    `$XDG_CONFIG_HOME/swarmgit/config.yaml` (`~/.config/swarmgit/config.yaml`),
//! 3. `/etc/swarmgit/config.yaml` (Linux and other Unix systems).
//!
//! Each key maps onto the environment variable the rest of the code reads, so
//! `listen: 0.0.0.0:42424` means `SWARM_LISTEN=0.0.0.0:42424`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// System-wide config file.
pub const SYSTEM_CONFIG: &str = "/etc/swarmgit/config.yaml";

/// A scalar or a list of scalars (`relays: a:1` or `relays: [a:1, b:2]`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Many {
    One(Scalar),
    List(Vec<Scalar>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl Scalar {
    fn text(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }

    fn flag(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::Text(text) => match text.as_str() {
                "true" | "yes" | "on" | "1" => Some(true),
                "false" | "no" | "off" | "0" => Some(false),
                _ => None,
            },
            Self::Int(0) => Some(false),
            Self::Int(1) => Some(true),
            Self::Int(_) => None,
        }
    }
}

impl Many {
    fn joined(&self) -> String {
        match self {
            Self::One(one) => one.text(),
            Self::List(items) => items.iter().map(Scalar::text).collect::<Vec<_>>().join(","),
        }
    }

    fn first(&self) -> Option<&Scalar> {
        match self {
            Self::One(one) => Some(one),
            Self::List(items) => items.first(),
        }
    }
}

/// The settings a config file may hold.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Data directory (`SWARM_HOME`).
    home: Option<Many>,
    /// Peer listen address (`SWARM_LISTEN`).
    listen: Option<Many>,
    /// Host or IP placed in links (`SWARM_ADVERTISE`).
    advertise: Option<Many>,
    /// Relays to register with and dial through (`SWARM_RELAYS`).
    relays: Option<Many>,
    /// `auto`, `always` or `off` (`SWARM_RELAY_MODE`).
    relay_mode: Option<Many>,
    /// Milliseconds before a relay dial (`SWARM_RELAY_DELAY_MS`).
    relay_delay_ms: Option<Many>,
    /// LAN discovery on or off (`SWARM_NO_LAN`).
    lan: Option<Many>,
    /// LAN discovery UDP port (`SWARM_LAN_PORT`).
    lan_port: Option<Many>,
    /// Mainline DHT on or off (`SWARM_NO_DHT`).
    dht: Option<Many>,
    /// DHT bootstrap nodes (`SWARM_DHT_BOOTSTRAP`).
    dht_bootstrap: Option<Many>,
    /// DHT bind address (`SWARM_DHT_BIND`).
    dht_bind: Option<Many>,
    /// DHT UDP port (`SWARM_DHT_PORT`).
    dht_port: Option<Many>,
    /// UPnP port mapping on or off (`SWARM_NO_UPNP`).
    upnp: Option<Many>,
    /// Seconds a first clone waits for LAN answers (`SWARM_FIND_TIMEOUT`).
    find_timeout: Option<Many>,
    /// Largest pack in bytes (`SWARM_MAX_PACK_BYTES`).
    max_pack_bytes: Option<Many>,
    /// Idle shutdown in minutes (`SWARM_IDLE_EXIT_MINUTES`).
    idle_exit_minutes: Option<Many>,
    /// Tracing filter (`SWARM_LOG`).
    log: Option<Many>,
    /// Static mirror base URLs (`SWARM_MIRRORS`).
    mirrors: Option<Many>,
    /// Anything else is reported, not silently ignored.
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_yaml::Value>,
}

/// An error reading a config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("config {path}: {message}")]
    Read {
        /// File path.
        path: String,
        /// Reason.
        message: String,
    },
    /// The file is not valid YAML of the expected shape.
    #[error("config {path}: {message}")]
    Parse {
        /// File path.
        path: String,
        /// Reason.
        message: String,
    },
    /// A value has the wrong type for its key.
    #[error("config {path}: {message}")]
    Invalid {
        /// File path.
        path: String,
        /// Reason.
        message: String,
    },
}

impl Config {
    /// Parse YAML text.
    pub fn parse(text: &str, path: &str) -> Result<Self, ConfigError> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_yaml::from_str(text).map_err(|err| ConfigError::Parse {
            path: path.to_string(),
            message: err.to_string(),
        })
    }

    /// Keys this version does not know.
    #[must_use]
    pub fn unknown_keys(&self) -> Vec<&str> {
        self.unknown.keys().map(String::as_str).collect()
    }

    /// The environment variables this file stands for.
    pub fn variables(&self, path: &str) -> Result<Vec<(&'static str, String)>, ConfigError> {
        let mut out = Vec::new();
        let text: [(&'static str, &Option<Many>); 12] = [
            ("SWARM_HOME", &self.home),
            ("SWARM_LISTEN", &self.listen),
            ("SWARM_ADVERTISE", &self.advertise),
            ("SWARM_RELAYS", &self.relays),
            ("SWARM_RELAY_MODE", &self.relay_mode),
            ("SWARM_RELAY_DELAY_MS", &self.relay_delay_ms),
            ("SWARM_LAN_PORT", &self.lan_port),
            ("SWARM_DHT_BOOTSTRAP", &self.dht_bootstrap),
            ("SWARM_DHT_BIND", &self.dht_bind),
            ("SWARM_DHT_PORT", &self.dht_port),
            ("SWARM_FIND_TIMEOUT", &self.find_timeout),
            ("SWARM_MAX_PACK_BYTES", &self.max_pack_bytes),
        ];
        for (name, value) in text {
            if let Some(value) = value {
                out.push((name, value.joined()));
            }
        }
        for (name, value) in [
            ("SWARM_IDLE_EXIT_MINUTES", &self.idle_exit_minutes),
            ("SWARM_LOG", &self.log),
            ("SWARM_MIRRORS", &self.mirrors),
        ] {
            if let Some(value) = value {
                out.push((name, value.joined()));
            }
        }
        // `lan: false` reads better than `no_lan: true`; the variables are negative.
        for (name, key, value) in [
            ("SWARM_NO_LAN", "lan", &self.lan),
            ("SWARM_NO_DHT", "dht", &self.dht),
            ("SWARM_NO_UPNP", "upnp", &self.upnp),
        ] {
            let Some(value) = value else { continue };
            let enabled =
                value
                    .first()
                    .and_then(Scalar::flag)
                    .ok_or_else(|| ConfigError::Invalid {
                        path: path.to_string(),
                        message: format!("{key} must be true or false"),
                    })?;
            if !enabled {
                out.push((name, "1".to_string()));
            }
        }
        Ok(out)
    }
}

/// The files to read, highest precedence first.
fn sources(env: impl Fn(&str) -> Option<String>) -> (Vec<PathBuf>, bool) {
    if let Some(path) = env("SWARM_CONFIG").filter(|path| !path.is_empty()) {
        return (vec![PathBuf::from(path)], true);
    }
    let mut files = Vec::new();
    let user = env("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")));
    if let Some(dir) = user {
        files.push(dir.join("swarmgit/config.yaml"));
    }
    #[cfg(unix)]
    files.push(PathBuf::from(SYSTEM_CONFIG));
    (files, false)
}

fn read(path: &Path, required: bool) -> Result<Option<Config>, ConfigError> {
    let shown = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let config = Config::parse(&text, &shown)?;
            for key in config.unknown_keys() {
                eprintln!("swarm: config {shown}: ignoring unknown setting `{key}`");
            }
            Ok(Some(config))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => Ok(None),
        Err(err) => Err(ConfigError::Read {
            path: shown,
            message: err.to_string(),
        }),
    }
}

/// Resolve the settings the config files add to `env`, without applying them.
pub fn resolve(
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(&'static str, String)>, ConfigError> {
    let (files, required) = sources(&env);
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for path in files {
        let Some(config) = read(&path, required)? else {
            continue;
        };
        for (name, value) in config.variables(&path.display().to_string())? {
            let taken = env(name).is_some() || out.iter().any(|(set, _)| *set == name);
            if !taken {
                out.push((name, value));
            }
        }
    }
    Ok(out)
}

/// Export the config files' settings as environment variables, leaving any
/// variable that is already set alone. Call it once, before starting threads.
pub fn load_into_env() -> Result<(), ConfigError> {
    for (name, value) in resolve(|key| std::env::var(key).ok())? {
        std::env::set_var(name, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(text: &str) -> Vec<(&'static str, String)> {
        Config::parse(text, "test")
            .unwrap()
            .variables("test")
            .unwrap()
    }

    #[test]
    fn maps_keys_onto_variables() {
        let got = vars(
            "listen: 0.0.0.0:42424\nrelays:\n  - a.example:42425\n  - b.example:42425\nlan: false\nrelay_delay_ms: 500\ndht: true\n",
        );
        assert!(got.contains(&("SWARM_LISTEN", "0.0.0.0:42424".into())));
        assert!(got.contains(&("SWARM_RELAYS", "a.example:42425,b.example:42425".into())));
        assert!(got.contains(&("SWARM_NO_LAN", "1".into())));
        assert!(got.contains(&("SWARM_RELAY_DELAY_MS", "500".into())));
        assert!(!got.iter().any(|(name, _)| *name == "SWARM_NO_DHT"));
    }

    #[test]
    fn empty_file_and_unknown_keys() {
        assert!(vars("").is_empty());
        let config = Config::parse("bogus: 1\nlog: debug\n", "test").unwrap();
        assert_eq!(config.unknown_keys(), vec!["bogus"]);
    }

    #[test]
    fn bad_flag_and_bad_yaml_are_errors() {
        assert!(Config::parse("lan: maybe\n", "t")
            .unwrap()
            .variables("t")
            .is_err());
        assert!(Config::parse("listen: [\n", "t").is_err());
    }

    #[test]
    fn environment_beats_files_and_user_beats_system() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.yaml");
        std::fs::write(&user, "listen: 1.1.1.1:1\nlog: debug\n").unwrap();
        let env = |key: &str| match key {
            "SWARM_CONFIG" => Some(user.display().to_string()),
            "SWARM_LOG" => Some("warn".to_string()),
            _ => None,
        };
        let got = resolve(env).unwrap();
        assert_eq!(got, vec![("SWARM_LISTEN", "1.1.1.1:1".to_string())]);
    }

    #[test]
    fn explicit_config_must_exist() {
        let env = |key: &str| (key == "SWARM_CONFIG").then(|| "/nonexistent/x.yaml".to_string());
        assert!(resolve(env).is_err());
    }
}
