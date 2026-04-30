use clap::{Parser, Subcommand};

use crate::config::RuleSource;

fn parse_jobs(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid number"))?;
    if n < 1 {
        Err("--jobs must be at least 1".to_string())
    } else {
        Ok(n)
    }
}

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
        #[arg(long, short, value_parser = parse_jobs)]
        jobs: Option<usize>,
        /// How to source the rule for each commit (default: commit)
        #[arg(long, default_value = "commit")]
        rule: RuleSource,
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
    /// Called by the installed pre-push hook (not for direct use)
    #[command(hide = true)]
    Run,
}
