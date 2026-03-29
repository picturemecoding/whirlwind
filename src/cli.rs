use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "whirlwind",
    about = "Collaborative Reaper project sync for podcasters",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize whirlwind config and test R2 connection
    Init,
    /// List all projects and their lock/push status
    List,
    /// Show status of a project (lock info, last push)
    Status { project: String },
    /// Download a project from R2 to local working directory
    Pull {
        project: String,
        /// Force download even if local changes exist
        #[arg(long)]
        force: bool,
    },
    /// Upload local project changes to R2
    Push {
        project: String,
        /// Skip lock acquisition (use with caution)
        #[arg(long)]
        no_lock: bool,
    },
    /// Pull project, launch Reaper, push on exit
    Session { project: String },
    /// Break a stale lock on a project
    Unlock {
        project: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Create a new episode project from a Reaper template
    New {
        /// Episode name; a directory under working_dir will be created if needed,
        /// and any existing audio files there will be discovered
        episode: String,
        /// Template name to use (default: from config, else "default")
        #[arg(long)]
        template: Option<String>,
        /// Seconds to trim from project end (default: from config, else 0)
        #[arg(long)]
        trim_seconds: Option<f64>,
        /// Show what would happen without writing or pushing anything
        #[arg(long)]
        dry_run: bool,
        /// Assign a file to a named track: --assign <track>=<filename> (repeatable)
        #[arg(long, value_name = "TRACK=FILE")]
        assign: Vec<String>,
    },
}
