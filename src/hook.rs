use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::Path;

use anyhow::Result;

use crate::config::{Config, RuleSource};
use crate::sync::{self, ChunkLimit};

const MARKER: &str = "# Installed by git-xf.";

const SCRIPT: &str = "#!/usr/bin/env sh
# Installed by git-xf. Re-run 'git xf hook install' after config changes.
exec git xf hook run
";

pub fn install(git_dir: &Path) -> Result<()> {
    let hook_path = git_dir.join("hooks").join("pre-push");
    std::fs::create_dir_all(hook_path.parent().unwrap())?;
    std::fs::write(&hook_path, SCRIPT)?;
    make_executable(&hook_path)?;
    eprintln!("Installed pre-push hook: {}", hook_path.display());
    Ok(())
}

pub fn uninstall(git_dir: &Path) -> Result<()> {
    let hook_path = git_dir.join("hooks").join("pre-push");
    if !hook_path.exists() {
        eprintln!("No pre-push hook found.");
        return Ok(());
    }
    let content = std::fs::read_to_string(&hook_path)?;
    if !content.contains(MARKER) {
        eprintln!(
            "Skipping: {} was not installed by git-xf.",
            hook_path.display()
        );
        return Ok(());
    }
    std::fs::remove_file(&hook_path)?;
    eprintln!("Removed pre-push hook: {}", hook_path.display());
    Ok(())
}

/// Invoked by the installed pre-push hook script.  Reads git's push
/// description from stdin and syncs any transformation whose `branches`
/// whitelist contains a pushed branch.
pub async fn run(source_repo: &Path, git_dir: &Path, config: &Config, jobs: usize) -> Result<()> {
    // stdin lines: "<local-ref> <local-sha> <remote-ref> <remote-sha>"
    let stdin = io::stdin();
    let mut per_transform: HashMap<&str, Vec<String>> = HashMap::new();

    for line in stdin.lock().lines() {
        let line = line?;
        let mut parts = line.split_whitespace();
        let local_ref = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let local_sha = match parts.next() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let branch = match local_ref.strip_prefix("refs/heads/") {
            Some(b) => b,
            None => continue,
        };
        for (name, cfg) in config {
            if cfg.branches.iter().any(|b| b == branch) {
                per_transform
                    .entry(name.as_str())
                    .or_default()
                    .push(local_sha.clone());
            }
        }
    }

    let mut entries: Vec<_> = per_transform.into_iter().collect();
    entries.sort_by_key(|(name, _)| *name);
    for (name, shas) in entries {
        // Build a single-transformation config slice so sync::run's loop
        // only touches this transformation.
        let single: Config =
            std::iter::once((name.to_string(), config.get(name).unwrap().clone())).collect();
        sync::run(
            source_repo,
            git_dir,
            &single,
            &shas,
            false,
            jobs,
            RuleSource::Commit,
            ChunkLimit::default(),
            None,
            false,
        )
        .await?;
    }

    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}
