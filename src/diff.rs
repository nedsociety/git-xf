use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};

use crate::cache::Cache;
use crate::config::Config;
use crate::git;

/// A revision token found in the arg list, with SHAs pre-resolved.
struct RevToken {
    /// Index in `options_and_revs` where this token lives.
    idx: usize,
    lhs: String,
    lhs_sha: String,
    rhs: Option<String>,
    rhs_sha: Option<String>,
    /// "" | ".." | "..."
    sep: &'static str,
}

/// Returns the separator position if the token contains `...` or `..`.
fn find_range_sep(token: &str) -> Option<(usize, &'static str)> {
    if let Some(pos) = token.find("...") {
        return Some((pos, "..."));
    }
    if let Some(pos) = token.find("..") {
        return Some((pos, ".."));
    }
    None
}

/// Try to verify a ref in the source repo. Returns `Ok(sha)` on success.
fn verify_ref(repo: &Path, refname: &str) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", refname])
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        bail!("unknown revision '{refname}'")
    }
}

/// Walk `options_and_revs`, skip `-`-prefixed tokens, try each remaining
/// token as a revision spec (possibly containing `..` or `...`).
///
/// Returns the list of matched `RevToken`s (at most 2) with SHAs resolved.
fn find_rev_tokens(repo: &Path, options_and_revs: &[String]) -> Result<Vec<RevToken>> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < options_and_revs.len() {
        let tok = &options_and_revs[i];
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        // Try to identify as a revision spec.
        if let Some((pos, sep)) = find_range_sep(tok) {
            let lhs = &tok[..pos];
            let rhs = &tok[pos + sep.len()..];
            // Both halves must verify; store the resolved SHAs.
            if let (Ok(lhs_sha), Ok(rhs_sha)) = (verify_ref(repo, lhs), verify_ref(repo, rhs)) {
                tokens.push(RevToken {
                    idx: i,
                    lhs: lhs.to_string(),
                    lhs_sha,
                    rhs: Some(rhs.to_string()),
                    rhs_sha: Some(rhs_sha),
                    sep,
                });
            }
        } else if let Ok(sha) = verify_ref(repo, tok) {
            tokens.push(RevToken {
                idx: i,
                lhs: tok.to_string(),
                lhs_sha: sha,
                rhs: None,
                rhs_sha: None,
                sep: "",
            });
        }
        // Unverified non-flag tokens are left unchanged (bare paths before `--`).
        i += 1;
    }
    Ok(tokens)
}

pub fn run(
    source_repo: &Path,
    git_dir: &Path,
    config: &Config,
    transform: Option<String>,
    rest: Vec<String>,
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

    // Split on the first bare `--`.
    let dash_pos = rest.iter().position(|t| t == "--");
    let (options_and_revs, paths) = match dash_pos {
        Some(pos) => (rest[..pos].to_vec(), rest[pos..].to_vec()),
        None => (rest.clone(), vec![]),
    };

    // Find revision tokens (SHAs pre-resolved).
    let rev_tokens = find_rev_tokens(source_repo, &options_and_revs)?;
    if rev_tokens.is_empty() {
        bail!("no revision arguments found");
    }

    // Single-commit form: validate clean working tree, resolve HEAD.
    let single_commit_form = rev_tokens.len() == 1 && rev_tokens[0].sep.is_empty();
    let head_sha = if single_commit_form {
        let status_out = Command::new("git")
            .arg("-C")
            .arg(source_repo)
            .args(["status", "--porcelain", "--untracked-files=no"])
            .output()?;
        if !String::from_utf8_lossy(&status_out.stdout)
            .trim()
            .is_empty()
        {
            bail!(
                "working tree is not clean; commit or stash all changes before using \
                 `git xf diff` with a single commit"
            );
        }
        Some(git::resolve_ref(source_repo, "HEAD")?)
    } else {
        None
    };

    // Best-effort cache fetch.
    let cache = Cache::new(git_dir, &name);
    let fetch_failed = cache.fetch_and_prune().is_err();

    // Collect (refname, source_sha) pairs to map, in traversal order.
    let mut to_look_up: Vec<(&str, &str)> = Vec::new();
    for tok in &rev_tokens {
        to_look_up.push((&tok.lhs, &tok.lhs_sha));
        if let (Some(rhs), Some(rhs_sha)) = (&tok.rhs, &tok.rhs_sha) {
            to_look_up.push((rhs.as_str(), rhs_sha.as_str()));
        }
    }
    if let Some(ref h) = head_sha {
        to_look_up.push(("HEAD", h.as_str()));
    }

    // Look up target SHA for every source SHA.
    let mut sha_to_target: HashMap<String, String> = HashMap::new();
    for (refname, source_sha) in to_look_up {
        if sha_to_target.contains_key(source_sha) {
            continue;
        }
        let mapping_ref = cache.mapping_ref(source_sha);
        let target_sha = Command::new("git")
            .arg("-C")
            .arg(&cache.path)
            .args(["rev-parse", "--verify", &mapping_ref])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string());

        match target_sha {
            Some(t) => {
                sha_to_target.insert(source_sha.to_string(), t);
            }
            None => {
                let msg = if fetch_failed {
                    format!(
                        "{source_sha} ({refname}) has no mapping for transformation '{name}'.\n\
                         note: cache fetch failed — the mapping may exist in the remote.\n\
                         If not, run `git xf sync` to transform it first."
                    )
                } else {
                    format!(
                        "{source_sha} ({refname}) has no mapping for transformation '{name}'; \
                         run `git xf sync` to transform it first."
                    )
                };
                bail!("{}", msg);
            }
        }
    }

    // Reconstruct the arg list with substituted target SHAs.
    let mut reconstructed: Vec<String> = options_and_revs.clone();
    for tok in &rev_tokens {
        let target_lhs = &sha_to_target[&tok.lhs_sha];
        if tok.sep.is_empty() {
            reconstructed[tok.idx] = target_lhs.clone();
        } else {
            let target_rhs = &sha_to_target[tok.rhs_sha.as_ref().unwrap()];
            reconstructed[tok.idx] = format!("{target_lhs}{}{target_rhs}", tok.sep);
        }
    }

    // For single-commit form, append the mapped HEAD SHA.
    if let Some(ref h) = head_sha {
        reconstructed.push(sha_to_target[h].clone());
    }

    // Append paths (everything from `--` onward).
    reconstructed.extend(paths);

    // Run `git diff` in the cache repo and forward exit code.
    let status = Command::new("git")
        .arg("-C")
        .arg(&cache.path)
        .arg("diff")
        .args(&reconstructed)
        .status()?;

    std::process::exit(status.code().unwrap_or(1));
}
