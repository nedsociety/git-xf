use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};

use crate::cache::Cache;
use crate::config::Config;
use crate::git;

/// Parse `url` as a GitHub remote URL and return `(org, repo)` with any
/// trailing `.git` stripped.  Accepts HTTPS (with or without credentials)
/// and SSH forms.
fn parse_github_url(url: &str) -> Option<(String, String)> {
    // HTTPS: https://[<user>[:<token>]@]github.com/<org>/<repo>[.git]
    if let Some(after_scheme) = url.strip_prefix("https://") {
        // Strip optional credentials (everything up to and including '@').
        let host_and_path = match after_scheme.find("@github.com/") {
            Some(at) => &after_scheme[at + "@github.com/".len()..],
            None => after_scheme.strip_prefix("github.com/")?,
        };
        return split_org_repo(host_and_path);
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
    #[cfg(windows)]
    {
        // `start` treats the first quoted argument as the window title; use an
        // empty title so `url` is opened as a document/URL.
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        for cmd in &["open", "xdg-open"] {
            if Command::new(cmd).arg(url).spawn().is_ok() {
                return true;
            }
        }
        false
    }
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
    if git::resolve_ref(&cache.path, &branch_ref).is_err() {
        bail!("branch '{branch}' not found in target repo for '{name}'");
    }

    // Verify <base> if provided.
    if let Some(ref b) = base {
        let base_ref = format!("refs/heads/{b}");
        if git::resolve_ref(&cache.path, &base_ref).is_err() {
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

    // Open when stdout looks interactive (skip headless/CI) and the OS opener works.
    if std::io::stdout().is_terminal() && try_open(&compare_url) {
        return Ok(());
    }
    println!("{compare_url}");
    Ok(())
}
