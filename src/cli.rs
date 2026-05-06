use clap::{Parser, Subcommand};

use crate::config::RuleSource;
use crate::sync::ChunkLimit;

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

fn parse_depth(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid number"))?;
    if n == 0 {
        Err("--depth must be at least 1 (omit the flag for unlimited)".to_string())
    } else {
        Ok(n)
    }
}

#[derive(Parser)]
#[command(name = "git-xf", about = "Transform git repositories commit-by-commit")]
pub struct Cli {
    /// Log verbosity. Overrides LOGLEVEL and -v/-vv. Must precede the subcommand.
    #[arg(long, value_name = "LEVEL",
          value_parser = ["error", "warn", "info", "debug", "trace"])]
    pub loglevel: Option<String>,

    /// Increase verbosity (-v = debug, -vv = trace). Must precede the subcommand.
    /// Not accepted by `diff` (forwarded to `git diff` instead — use LOGLEVEL).
    #[arg(short = 'v', action = clap::ArgAction::Count)]
    pub verbose: u8,

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
        /// Push after every N commits or N bytes of object data (e.g. 100, 50M). 0 = single push at the end.
        #[arg(long, default_value = "50M")]
        push_chunk: ChunkLimit,
        /// Transform at most N commits from each tip (BFS distance). Boundary commits become
        /// synthetic roots; the target graph will not be complete. Must be ≥ 1.
        #[arg(long, value_parser = parse_depth)]
        depth: Option<usize>,
        /// Use all refs/heads/* as tips instead of explicit REFs.
        #[arg(long, conflicts_with = "refs")]
        all_branches: bool,
        /// Skip rule execution for non-tip commits; apply changeless policy directly.
        #[arg(long)]
        tips_only: bool,
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
    /// Show diff between mapped commits in the target repository.
    /// Usage: git xf diff [-x <transform>] [<diff-options>] <revisions> [-- <path>...]
    /// -x <name> selects the transformation (required when more than one is configured).
    /// -x must be the very first argument after `git xf diff` if present.
    Diff {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the GitHub compare URL for a target-repo branch.
    Pr {
        /// Transformation name (required when more than one is configured)
        #[arg(short = 'x', long = "transform")]
        transform: Option<String>,
        /// Branch to compare (must exist in target repo)
        branch: String,
        /// Base branch for comparison (default: target repo's default branch)
        base: Option<String>,
    },
    /// Print the git-xf version
    Version,
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
