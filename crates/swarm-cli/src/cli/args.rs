//! The command line itself (spec section 9.1).

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use swarm_relay::cli::RelayArgs;

#[derive(Parser)]
#[command(name = "swarm", version, about = "Zero-setup peer-to-peer git")]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Cmd,
}

#[derive(Subcommand)]
pub(super) enum MirrorCmd {
    /// Write DIR/<repo-id>/repo.swarmbundle; serve DIR with any web server.
    Export {
        /// A repo ID or link.
        link: String,
        /// Directory to write into.
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
pub(super) enum Cmd {
    /// Create a repo key and publish the first manifest.
    Init {
        /// Object format for a newly created git repo.
        #[arg(long, value_name = "FORMAT")]
        object_format: Option<String>,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Print the link for a repo.
    Link {
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
        repo: Option<String>,
    },
    /// Show the daemon, repos, relays and peers.
    Status {
        /// Print one JSON object on stdout.
        #[arg(long, global = true)]
        json: bool,
        /// Show one repo in detail.
        repo: Option<String>,
        #[command(subcommand)]
        view: Option<StatusCmd>,
    },
    /// Inspect signed repository metadata without downloading Git objects.
    Inspect {
        /// A `swarm://` link, optionally with hints.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
        /// Stream lookup progress as JSON lines.
        #[arg(long)]
        progress: bool,
    },
    /// Show or set a repo's name and description (root or maintainer key).
    About {
        /// A repo ID or link.
        repo: String,
        /// New name, at most 64 bytes. "" removes it.
        #[arg(long)]
        name: Option<String>,
        /// New one-line description, at most 512 bytes. "" removes it.
        #[arg(long)]
        description: Option<String>,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Find public relay servers through the Mainline DHT and relay gossip.
    DiscoverRelays {
        /// Print a JSON object containing protocol-verified public relays.
        #[arg(long)]
        json: bool,
    },
    /// Sync a repo and keep serving it.
    Seed {
        /// A `swarm://` link, optionally with hints.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Stop serving a repo but keep its store.
    Unseed {
        /// A repo ID or link.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Repack a repo's store and drop unreachable objects, when idle (ST-7).
    Gc {
        /// A repo ID or link.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Delete a repo's store and manifests, and stop serving it.
    Purge {
        /// A repo ID or link.
        repo: String,
        /// Do not ask for confirmation. The signing key is kept.
        #[arg(long)]
        yes: bool,
        /// Also delete the signing key. It cannot be recreated.
        #[arg(long)]
        delete_key: bool,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Create an independent repo from another one.
    Fork {
        /// The source link, optionally with hints.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Write a repo and its signed state to a bundle file for offline transfer.
    Export {
        /// A repo ID or link.
        link: String,
        /// Bundle file to write.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Publish a repo for static mirrors (plain file hosting).
    Mirror {
        #[command(subcommand)]
        command: MirrorCmd,
    },
    /// Read a bundle file, making its repo available offline.
    Import {
        /// Bundle file to read.
        file: PathBuf,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Add or remove maintainers, who may publish without the root key.
    Maintainers {
        #[command(subcommand)]
        command: MaintainersCmd,
    },
    /// Settle a ref conflict here by taking one signer's value.
    Accept {
        /// A repo ID or link.
        repo: String,
        /// The ref in conflict, for example refs/heads/main.
        refname: String,
        /// The signer (key ID, hex or base32) whose value to take.
        signer: String,
    },
    /// Key management.
    Key {
        #[command(subcommand)]
        command: KeyCmd,
    },
    /// Print a shell completion script (bash, zsh, fish, elvish, powershell).
    Completions {
        /// The shell to generate for.
        shell: Shell,
    },
    /// Run a relay server for peers that cannot accept connections.
    Relay(RelayArgs),
    /// Start, stop or restart the background daemon.
    Daemon {
        /// Run the daemon in this terminal (for service managers).
        #[arg(long)]
        foreground: bool,
        /// Run as an already-detached process (used by autostart).
        #[arg(long, hide = true)]
        detach: bool,
        /// Try direct UDP paths between relayed peers (experimental).
        #[arg(long, global = true)]
        experimental_udp: bool,
        #[command(subcommand)]
        action: Option<DaemonCmd>,
    },
}

#[derive(Subcommand)]
pub(super) enum MaintainersCmd {
    /// Authorize a key (root key only), optionally limited to ref globs.
    Add {
        repo: String,
        /// The maintainer's key ID, from `swarm key new` or `swarm key id`.
        key: String,
        /// Ref globs the key may publish (default refs/heads/*).
        globs: Vec<String>,
    },
    /// Remove a maintainer (root key only).
    Remove { repo: String, key: String },
    /// List the maintainers.
    List { repo: String },
}

#[derive(Subcommand)]
pub(super) enum KeyCmd {
    /// Create a recovery key, name it in the repo, and save it to FILE (keep it offline).
    Recovery {
        repo: String,
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
    },
    /// Move the root to a new key, authorized by the recovery key file.
    Rotate {
        repo: String,
        #[arg(long, value_name = "FILE")]
        recovery: PathBuf,
    },
    /// Create this machine's own key for a repo, to be authorized by the root.
    New { repo: String },
    /// Print the key ID of this machine's key for a repo.
    Id { repo: String },
    /// Copy a repo signing key to FILE with mode 0600.
    Backup {
        repo: String,
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
pub(super) enum DaemonCmd {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Stop the daemon if it is running, then start it.
    Restart,
}

#[derive(Subcommand)]
pub(super) enum StatusCmd {
    /// Repositories: sequence numbers, seeders and conflicts.
    Repos {
        /// Show one repo in detail.
        repo: Option<String>,
    },
    /// Relay registrations.
    Relays,
    /// Peers known for each repo, and which are connected.
    Peers {
        /// Only this repo.
        repo: Option<String>,
    },
}
