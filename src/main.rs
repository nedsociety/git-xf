mod cache;
mod cli;
mod config;
mod diff;
mod error;
mod git;
mod hook;
mod init;
mod pr;
mod status;
mod sync;
mod transform;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use cli::{Cli, Commands, HookCommands};

fn resolve_loglevel(cli: &Cli) -> Result<log::LevelFilter> {
    if let Some(s) = &cli.loglevel {
        return parse_level(s);
    }
    if cli.verbose >= 2 {
        return Ok(log::LevelFilter::Trace);
    }
    if cli.verbose == 1 {
        return Ok(log::LevelFilter::Debug);
    }
    if let Ok(s) = std::env::var("LOGLEVEL") {
        if !s.is_empty() {
            return parse_level(&s);
        }
    }
    Ok(log::LevelFilter::Info)
}

fn parse_level(s: &str) -> Result<log::LevelFilter> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Ok(log::LevelFilter::Error),
        "warn" => Ok(log::LevelFilter::Warn),
        "info" => Ok(log::LevelFilter::Info),
        "debug" => Ok(log::LevelFilter::Debug),
        "trace" => Ok(log::LevelFilter::Trace),
        other => bail!("invalid log level '{other}' (expected error|warn|info|debug|trace)"),
    }
}

fn init_logger(level: log::LevelFilter) {
    env_logger::Builder::new()
        .filter_level(level)
        .format(|buf, record| {
            use std::io::Write;
            match record.level() {
                log::Level::Info => writeln!(buf, "{}", record.args()),
                log::Level::Warn => writeln!(buf, "warning: {}", record.args()),
                log::Level::Error => writeln!(buf, "error: {}", record.args()),
                level => {
                    let ts = buf.timestamp_millis();
                    writeln!(buf, "[{ts} {level:5}] {}", record.args())
                }
            }
        })
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = resolve_loglevel(&cli)?;
    init_logger(level);
    match cli.command {
        Commands::Sync {
            dry_run,
            jobs,
            rule,
            push_chunk,
            depth,
            all_branches,
            tips_only,
            refs,
        } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            let jobs = jobs.unwrap_or_else(default_jobs);
            sync::run(
                &source_repo,
                &git_dir,
                &config,
                &refs,
                dry_run,
                jobs,
                rule,
                push_chunk,
                depth,
                all_branches,
                tips_only,
            )
            .await?;
        }
        Commands::Init { target } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            init::run(&git_dir, &config, target.as_deref())?;
        }
        Commands::Status { branch } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            status::run(&source_repo, &git_dir, &config, branch.as_deref())?;
        }
        Commands::Pr {
            transform,
            branch,
            base,
        } => {
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            pr::run(&git_dir, &config, transform, branch, base)?;
        }
        Commands::Diff { args } => {
            let (transform, rest) = match args.first().map(|s| s.as_str()) {
                Some("-x") => {
                    let name = args
                        .get(1)
                        .ok_or_else(|| anyhow!("missing argument for -x"))?
                        .clone();
                    (Some(name), args[2..].to_vec())
                }
                Some(s) if s.starts_with("-x") => {
                    bail!("-x requires a separate argument and must be the first flag");
                }
                _ => (None, args),
            };
            let (source_repo, git_dir) = locate_repo()?;
            let config = config::load(&source_repo)?;
            diff::run(&source_repo, &git_dir, &config, transform, rest)?;
        }
        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Commands::Hook { command } => match command {
            HookCommands::Install => {
                hook::install(&common_git_dir()?)?;
            }
            HookCommands::Uninstall => {
                hook::uninstall(&common_git_dir()?)?;
            }
            HookCommands::Run => {
                let (source_repo, git_dir) = locate_repo()?;
                let config = config::load(&source_repo)?;
                let jobs = default_jobs();
                hook::run(&source_repo, &git_dir, &config, jobs).await?;
            }
        },
    }
    Ok(())
}

fn common_git_dir() -> Result<PathBuf> {
    Ok(locate_repo()?.1)
}

fn locate_repo() -> Result<(PathBuf, PathBuf)> {
    let source_repo = PathBuf::from(git::repo_root()?);
    let raw_git_dir = git::git_common_dir()?;
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
