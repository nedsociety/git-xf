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
            let _ = (dry_run, jobs, refs);
            todo!("sync")
        }
        Commands::Init { target } => {
            let _ = target;
            todo!("init")
        }
        Commands::Status { branch } => {
            let _ = branch;
            todo!("status")
        }
        Commands::Hook { command } => match command {
            HookCommands::Install => todo!("hook install"),
            HookCommands::Uninstall => todo!("hook uninstall"),
        },
    }
}
