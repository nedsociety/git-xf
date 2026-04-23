mod cache;
mod cli;
mod config;
mod error;
mod git;
mod hook;
mod init;
mod status;
mod sync;
mod transform;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, HookCommands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Sync {
            dry_run,
            jobs,
            refs,
        } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            let jobs = jobs.unwrap_or_else(default_jobs);
            sync::run(&source_repo, &git_dir, &config, &refs, dry_run, jobs).await?;
        }
        Commands::Init { target } => {
            let _ = target;
            todo!("init")
        }
        Commands::Status { branch } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            status::run(&source_repo, &git_dir, &config, branch.as_deref())?;
        }
        Commands::Hook { command } => match command {
            HookCommands::Install => todo!("hook install"),
            HookCommands::Uninstall => todo!("hook uninstall"),
        },
    }
    Ok(())
}

fn locate_repo() -> Result<(PathBuf, PathBuf)> {
    let source_repo = PathBuf::from(git::repo_root()?);
    let raw_git_dir = git::git_dir()?;
    let git_dir = if Path::new(&raw_git_dir).is_absolute() {
        PathBuf::from(raw_git_dir)
    } else {
        source_repo.join(raw_git_dir)
    };
    Ok((source_repo, git_dir))
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
