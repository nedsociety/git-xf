use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};

use crate::cache::Cache;
use crate::config::Config;
use crate::git;

/// A revision token found in the arg list.
struct RevToken {
    /// Index in `options_and_revs` where this token lives.
    idx: usize,
    lhs: String,
    rhs: Option<String>,
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
/// Returns the list of matched `RevToken`s (at most 2).
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
            // Both halves must verify.
            if verify_ref(repo, lhs).is_ok() && verify_ref(repo, rhs).is_ok() {
                tokens.push(RevToken {
                    idx: i,
                    lhs: lhs.to_string(),
                    rhs: Some(rhs.to_string()),
                    sep,
                });
            }
        } else if verify_ref(repo, tok).is_ok() {
            tokens.push(RevToken {
                idx: i,
                lhs: tok.to_string(),
                rhs: None,
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

    // Find revision tokens.
    let rev_tokens = find_rev_tokens(source_repo, &options_and_revs)?;
    if rev_tokens.is_empty() {
        bail!("no revision arguments found");
    }

    // Determine which refs to resolve and detect single-commit form.
    let single_commit_form = rev_tokens.len() == 1 && rev_tokens[0].sep.is_empty();

    // Validate single-commit preconditions and collect all (ref, position-info) pairs.
    struct Side {
        refname: String,
        sha: String,
        /// Index in `options_and_revs` and the sep/rhs to reconstruct.
        token_idx: usize,
        is_lhs: bool,
    }

    let mut sides: Vec<Side> = Vec::new();

    if single_commit_form {
        // Check working tree clean.
        let status_out = Command::new("git")
            .arg("-C")
            .arg(source_repo)
            .args(["status", "--porcelain", "--untracked-files=no"])
            .output()?;
        let status_text = String::from_utf8_lossy(&status_out.stdout);
        if !status_text.trim().is_empty() {
            bail!(
                "working tree is not clean; commit or stash all changes before using \
                 `git xf diff` with a single commit"
            );
        }
        // Resolve HEAD.
        let head_sha = git::resolve_ref(source_repo, "HEAD")?;
        let commit_sha = verify_ref(source_repo, &rev_tokens[0].lhs)?;

        sides.push(Side {
            refname: rev_tokens[0].lhs.clone(),
            sha: commit_sha,
            token_idx: rev_tokens[0].idx,
            is_lhs: true,
        });
        sides.push(Side {
            refname: "HEAD".to_string(),
            sha: head_sha,
            // HEAD is not a token in options_and_revs; handled specially below.
            token_idx: usize::MAX,
            is_lhs: false,
        });
    } else {
        for tok in &rev_tokens {
            let lhs_sha = verify_ref(source_repo, &tok.lhs)?;
            sides.push(Side {
                refname: tok.lhs.clone(),
                sha: lhs_sha,
                token_idx: tok.idx,
                is_lhs: true,
            });
            if let Some(rhs) = &tok.rhs {
                let rhs_sha = verify_ref(source_repo, rhs)?;
                sides.push(Side {
                    refname: rhs.clone(),
                    sha: rhs_sha,
                    token_idx: tok.idx,
                    is_lhs: false,
                });
            }
        }
    }

    // Best-effort cache fetch.
    let cache = Cache::new(git_dir, &name);
    let fetch_failed = cache.fetch_and_prune().is_err();

    // Look up mappings for every SHA.
    let mut sha_to_target: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for side in &sides {
        if sha_to_target.contains_key(&side.sha) {
            continue;
        }
        let mapping_ref = cache.mapping_ref(&side.sha);
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
                sha_to_target.insert(side.sha.clone(), t);
            }
            None => {
                let msg = if fetch_failed {
                    format!(
                        "{sha} ({refname}) has no mapping for transformation '{name}'.\n\
                         note: cache fetch failed — the mapping may exist in the remote.\n\
                         If not, run `git xf sync` to transform it first.",
                        sha = side.sha,
                        refname = side.refname,
                    )
                } else {
                    format!(
                        "{sha} ({refname}) has no mapping for transformation '{name}'; \
                         run `git xf sync` to transform it first.",
                        sha = side.sha,
                        refname = side.refname,
                    )
                };
                bail!("{}", msg);
            }
        }
    }

    // Reconstruct the arg list with substituted SHAs.
    let mut reconstructed: Vec<String> = options_and_revs.clone();
    for tok in &rev_tokens {
        let lhs_sha = &sides
            .iter()
            .find(|s| s.token_idx == tok.idx && s.is_lhs)
            .unwrap()
            .sha;
        let target_lhs = sha_to_target[lhs_sha].as_str();

        if tok.sep.is_empty() {
            // Single-commit form: lhs token in reconstructed, HEAD appended after.
            reconstructed[tok.idx] = target_lhs.to_string();
        } else {
            let rhs_sha = &sides
                .iter()
                .find(|s| s.token_idx == tok.idx && !s.is_lhs)
                .unwrap()
                .sha;
            let target_rhs = sha_to_target[rhs_sha].as_str();
            reconstructed[tok.idx] = format!("{target_lhs}{sep}{target_rhs}", sep = tok.sep);
        }
    }

    // For single-commit form, append the mapped HEAD SHA.
    if single_commit_form {
        let head_sha = &sides
            .iter()
            .find(|s| s.token_idx == usize::MAX)
            .unwrap()
            .sha;
        let target_head = sha_to_target[head_sha].as_str();
        reconstructed.push(target_head.to_string());
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
