use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};

use crate::cache::Cache;
use crate::config::Config;

/// Parse `url` as a GitHub remote URL and return `(org, repo)` with any
/// trailing `.git` stripped.  Accepts HTTPS and SSH forms.
fn parse_github_url(url: &str) -> Option<(String, String)> {
    // HTTPS: https://github.com/<org>/<repo>[.git]
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        return split_org_repo(rest);
    }
    // SSH: git@github.com:<org>/<repo>[.git]
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return split_org_repo(rest);
    }
    None
}

fn split_org_repo(s: &str) -> Option<(String, String)> {
    let s = s.strip_suffix(".git").unwrap_or(s);
    let (org, repo) = s.split_once('/')?;
    if org.is_empty() || repo.is_empty() {
        return None;
    }
    Some((org.to_string(), repo.to_string()))
}

/// Try to open `url` with a platform opener (non-blocking).
/// Returns `true` if an opener was found and spawned successfully.
fn try_open(url: &str) -> bool {
    for cmd in &["open", "xdg-open"] {
        if Command::new(cmd).arg(url).spawn().is_ok() {
            return true;
        }
    }
    false
}

pub fn run(
    git_dir: &Path,
    config: &Config,
    transform: Option<String>,
    branch: String,
    base: Option<String>,
) -> Result<()> {
    // Select transformation.
    let name = match transform {
        Some(n) => {
            if !config.contains_key(&n) {
                bail!("no transformation named '{n}'");
            }
            n
        }
        None => {
            if config.len() > 1 {
                bail!("multiple transformations configured; use -x <name>");
            }
            config
                .keys()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no transformations configured"))?
                .clone()
        }
    };

    // Best-effort cache fetch.
    let cache = Cache::new(git_dir, &name);
    if let Err(e) = cache.fetch_and_prune() {
        eprintln!("warning: cache fetch failed: {e}");
    }

    // Verify <branch> exists in the target cache.
    let branch_ref = format!("refs/heads/{branch}");
    let branch_ok = Command::new("git")
        .arg("-C")
        .arg(&cache.path)
        .args(["rev-parse", "--verify", &branch_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !branch_ok {
        bail!("branch '{branch}' not found in target repo for '{name}'");
    }

    // Verify <base> if provided.
    if let Some(ref b) = base {
        let base_ref = format!("refs/heads/{b}");
        let base_ok = Command::new("git")
            .arg("-C")
            .arg(&cache.path)
            .args(["rev-parse", "--verify", &base_ref])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !base_ok {
            bail!("base branch '{b}' not found in target repo for '{name}'");
        }
    }

    // Read remote URL from the cache.
    let url_out = Command::new("git")
        .arg("-C")
        .arg(&cache.path)
        .args(["config", "remote.origin.url"])
        .output()?;
    let remote_url = String::from_utf8_lossy(&url_out.stdout)
        .trim_end()
        .to_string();

    // Parse as GitHub URL.
    let (org, repo) = parse_github_url(&remote_url).ok_or_else(|| {
        anyhow::anyhow!("target for '{name}' is not a GitHub repository (got: {remote_url})")
    })?;

    // Build compare URL.
    let compare_url = match base {
        Some(b) => format!("https://github.com/{org}/{repo}/compare/{b}...{branch}"),
        None => format!("https://github.com/{org}/{repo}/compare/{branch}"),
    };

    // Open if interactive TTY and opener is available; otherwise print.
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() && try_open(&compare_url) {
        return Ok(());
    }
    println!("{compare_url}");
    Ok(())
}
