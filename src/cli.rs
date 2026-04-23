use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "git-xf", about = "Transform git repositories commit-by-commit")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Transform commits and push to target repositories
    Sync {
        #[arg(long)]
        dry_run: bool,
        /// Maximum parallel workers (default: logical CPU count)
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Refs to sync (default: HEAD)
        refs: Vec<String>,
    },
    /// Initialize local caches for all configured transformations
    Init {
        /// Override target path (single-transformation repos only)
        #[arg(long)]
        target: Option<String>,
    },
    /// Show mapping status per commit
    Status {
        #[arg(long)]
        branch: Option<String>,
    },
    /// Manage the pre-push hook
    Hook {
        #[command(subcommand)]
        command: HookCommands,
    },
}

#[derive(Subcommand)]
pub enum HookCommands {
    /// Install the pre-push hook
    Install,
    /// Uninstall the pre-push hook
    Uninstall,
}
