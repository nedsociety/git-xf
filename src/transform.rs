use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::cache::Cache;
use crate::config::{ChangelessPolicy, IgnoreErrorPolicy, TransformConfig};
use crate::error::Error;
use crate::git::{self, CommitInfo, CommitTreeArgs};

// Well-known SHA of git's empty tree object.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub struct TransformCtx {
    /// Root of the source repository.
    pub source_repo: PathBuf,
    /// `.git` directory of the source repository (worktrees are placed inside here).
    pub git_dir: PathBuf,
    /// Source commit SHA to transform.
    pub source_sha: String,
    /// Local cache for this transformation's target repository.
    pub cache: Arc<Cache>,
    /// Transformation configuration.
    pub config: Arc<TransformConfig>,
    /// Transformation name.
    pub name: String,
    /// Resolved target-repo parent SHAs, in the same order as the source parents.
    pub target_parents: Vec<String>,
}

/// Transforms one source commit and returns the resulting target SHA.
///
/// Records the source→target mapping in the cache before returning.
/// Callers are responsible for acquiring any concurrency semaphore permit
/// before calling this.
pub fn transform_commit(ctx: &TransformCtx) -> Result<String> {
    let info = git::commit_info(&ctx.source_repo, &ctx.source_sha)?;
    let is_merge = info.parents.len() > 1;

    if !is_merge {
        for pattern in &ctx.config.skip_commit_messages {
            if info.message.contains(pattern.as_str()) {
                return skip_to_parent(ctx);
            }
        }
    }

    let src_wt = wt_path(&ctx.git_dir, &ctx.name, &ctx.source_sha, "src");
    std::fs::create_dir_all(src_wt.parent().unwrap())?;
    git::worktree_add(&ctx.source_repo, &src_wt, &ctx.source_sha)?;
    // force=true because rule.command leaves modified/untracked files behind
    let _src_guard = WorktreeGuard::new(&ctx.source_repo, &src_wt, true);

    match run_rule(&src_wt, &ctx.config.rule.command) {
        Ok(()) => {}
        Err(stderr) => match ctx.config.ignore_error {
            IgnoreErrorPolicy::Error => {
                return Err(Error::Transform {
                    name: ctx.name.clone(),
                    sha: ctx.source_sha.clone(),
                    stderr,
                }
                .into());
            }
            IgnoreErrorPolicy::Skip => {
                if is_merge {
                    // Skipping a merge commit would break target graph topology;
                    // fall back to empty-commit to preserve all parent edges.
                    let tree = parent_tree_sha(ctx)?;
                    let msg = error_message(&ctx.name, &ctx.source_sha, &stderr);
                    return create_and_record(ctx, &tree, &msg, &info);
                }
                return skip_to_parent(ctx);
            }
            IgnoreErrorPolicy::EmptyCommit => {
                let tree = parent_tree_sha(ctx)?;
                let msg = error_message(&ctx.name, &ctx.source_sha, &stderr);
                return create_and_record(ctx, &tree, &msg, &info);
            }
        },
    }

    let tgt_wt = wt_path(&ctx.git_dir, &ctx.name, &ctx.source_sha, "tgt");
    // If a previous run crashed before WorktreeGuard::drop could clean up,
    // the directory (and its git registration) may still exist.  Force-remove
    // it so the add below succeeds.  Errors are intentionally ignored: the
    // remove is best-effort and worktree_add_orphan will fail with a clear
    // message if the path is genuinely in use.
    let _ = Command::new("git")
        .arg("-C")
        .arg(&ctx.cache.path)
        .args(["worktree", "remove", "--force"])
        .arg(&tgt_wt)
        .output();
    let branch = format!("xf-work-{}", &ctx.source_sha);
    git::worktree_add_orphan(&ctx.cache.path, &tgt_wt, &branch)?;
    let _tgt_guard = WorktreeGuard::new(&ctx.cache.path, &tgt_wt, true).with_orphan_branch(branch);

    copy_output(&src_wt, &tgt_wt, &ctx.config.rule.output)?;
    git::git_add_all(&tgt_wt)?;
    let tree = git::write_tree(&tgt_wt)?;

    if !is_merge {
        if let Some(p_tree) = parent_tree_sha_opt(ctx)? {
            if tree == p_tree {
                match ctx.config.changeless {
                    ChangelessPolicy::Skip => return skip_to_parent(ctx),
                    ChangelessPolicy::EmptyCommit => {}
                }
            }
        }
    }

    let msg = normal_message(&info.message, &ctx.source_sha, &ctx.name);
    create_and_record(ctx, &tree, &msg, &info)
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Records source→parent mapping and returns the parent target SHA.
fn skip_to_parent(ctx: &TransformCtx) -> Result<String> {
    let sha = ctx.target_parents.first().cloned().ok_or_else(|| {
        anyhow!(
            "commit {} has no parent but a skip policy was applied",
            ctx.source_sha
        )
    })?;
    ctx.cache.set_mapping(&ctx.source_sha, &sha)?;
    Ok(sha)
}

/// Parent's tree SHA, or the empty-tree SHA for root commits.
fn parent_tree_sha(ctx: &TransformCtx) -> Result<String> {
    match ctx.target_parents.first() {
        Some(sha) => git::commit_tree_sha(&ctx.cache.path, sha),
        None => Ok(EMPTY_TREE_SHA.to_string()),
    }
}

/// Parent's tree SHA, or `None` for root commits (used for changeless check).
fn parent_tree_sha_opt(ctx: &TransformCtx) -> Result<Option<String>> {
    match ctx.target_parents.first() {
        Some(sha) => Ok(Some(git::commit_tree_sha(&ctx.cache.path, sha)?)),
        None => Ok(None),
    }
}

/// Calls `commit-tree`, records the mapping, and returns the target SHA.
fn create_and_record(
    ctx: &TransformCtx,
    tree: &str,
    message: &str,
    info: &CommitInfo,
) -> Result<String> {
    let target_sha = git::commit_tree(CommitTreeArgs {
        repo: &ctx.cache.path,
        tree,
        parents: &ctx.target_parents,
        message,
        author_name: &info.author_name,
        author_email: &info.author_email,
        author_date: &info.author_date,
        committer_name: &info.committer_name,
        committer_email: &info.committer_email,
        committer_date: &info.committer_date,
    })?;
    ctx.cache.set_mapping(&ctx.source_sha, &target_sha)?;
    Ok(target_sha)
}

fn normal_message(original: &str, source_sha: &str, name: &str) -> String {
    format!(
        "{}\n\ngit-xf-source: {}\ngit-xf-transform: {}",
        original.trim_end(),
        source_sha,
        name,
    )
}

fn error_message(name: &str, source_sha: &str, stderr: &str) -> String {
    format!(
        "[git-xf error] {} failed on {}\n\n{}\n\ngit-xf-source: {}\ngit-xf-transform: {}",
        name,
        source_sha,
        truncate(stderr, 4096),
        source_sha,
        name,
    )
}

fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

fn wt_path(git_dir: &Path, name: &str, sha: &str, kind: &str) -> PathBuf {
    git_dir
        .join("git-xf")
        .join("tmp")
        .join(format!("{name}-{kind}-{sha}"))
}

/// Runs `rule.command` in `wt_path` via `sh -c`. Returns `Err(combined_output)`
/// on non-zero exit, combining stdout and stderr so build-tool diagnostics
/// written to stdout are not silently lost.
fn run_rule(wt_path: &Path, command: &str) -> std::result::Result<(), String> {
    Command::new("sh")
        .args(["-c", command])
        .current_dir(wt_path)
        .output()
        .map_err(|e| e.to_string())
        .and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let combined = match (stdout.trim_end(), stderr.trim_end()) {
                    ("", s) => s.to_string(),
                    (o, "") => o.to_string(),
                    (o, s) => format!("{o}\n{s}"),
                };
                Err(combined)
            }
        })
}

/// Copies `output` paths from source worktree to target worktree.
/// If `output` is empty, copies the entire source worktree (excluding `.git`).
fn copy_output(src_wt: &Path, tgt_wt: &Path, output: &[String]) -> Result<()> {
    if output.is_empty() {
        return copy_recursive(src_wt, tgt_wt, true);
    }
    for rel in output {
        let src = src_wt.join(rel);
        let tgt = tgt_wt.join(rel);
        let meta = match std::fs::symlink_metadata(&src) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "warning: declared output path '{}' not found in source worktree; \
                     it will be absent from the target commit",
                    rel,
                );
                continue;
            }
            other => other?,
        };
        if meta.file_type().is_symlink() {
            if let Some(p) = tgt.parent() {
                std::fs::create_dir_all(p)?;
            }
            recreate_symlink(&src, &tgt)?;
        } else if meta.is_dir() {
            copy_recursive(&src, &tgt, false)?;
        } else {
            if let Some(p) = tgt.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&src, &tgt)?;
        }
    }
    Ok(())
}

fn copy_recursive(src: &Path, tgt: &Path, skip_git: bool) -> Result<()> {
    std::fs::create_dir_all(tgt)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if skip_git && name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let tgt_path = tgt.join(&name);
        let meta = std::fs::symlink_metadata(&src_path)?;
        if meta.file_type().is_symlink() {
            recreate_symlink(&src_path, &tgt_path)?;
        } else if meta.is_dir() {
            copy_recursive(&src_path, &tgt_path, false)?;
        } else {
            std::fs::copy(&src_path, &tgt_path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn recreate_symlink(src: &Path, tgt: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    Ok(symlink(std::fs::read_link(src)?, tgt)?)
}

#[cfg(not(unix))]
fn recreate_symlink(src: &Path, tgt: &Path) -> Result<()> {
    eprintln!(
        "warning: symlink {} cannot be recreated on this platform; copying file content instead",
        src.display()
    );
    std::fs::copy(src, tgt)?;
    Ok(())
}

// ── worktree cleanup guard ────────────────────────────────────────────────

struct WorktreeGuard {
    repo: PathBuf,
    path: PathBuf,
    force: bool,
    /// Orphan branch created alongside this worktree; deleted after removal.
    orphan_branch: Option<String>,
}

impl WorktreeGuard {
    fn new(repo: &Path, path: &Path, force: bool) -> Self {
        Self {
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
            force,
            orphan_branch: None,
        }
    }

    fn with_orphan_branch(mut self, branch: String) -> Self {
        self.orphan_branch = Some(branch);
        self
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo).args(["worktree", "remove"]);
        if self.force {
            cmd.arg("--force");
        }
        cmd.arg(&self.path);
        let removed = match cmd.output() {
            Ok(out) if !out.status.success() => {
                eprintln!(
                    "warning: git worktree remove failed for {}: {}",
                    self.path.display(),
                    String::from_utf8_lossy(&out.stderr).trim_end(),
                );
                false
            }
            Err(e) => {
                eprintln!(
                    "warning: could not run git worktree remove for {}: {e}",
                    self.path.display(),
                );
                false
            }
            Ok(_) => true,
        };
        // Delete the orphan branch only after the worktree is gone; git refuses
        // to delete a branch that is currently checked out.
        if removed {
            if let Some(branch) = &self.orphan_branch {
                let refname = format!("refs/heads/{branch}");
                let _ = Command::new("git")
                    .arg("-C")
                    .arg(&self.repo)
                    .args(["update-ref", "-d", &refname])
                    .output();
            }
        }
    }
}
