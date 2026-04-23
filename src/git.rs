use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::error::Error;

fn run(cmd: &mut Command, repo: &Path) -> Result<String> {
    let out = cmd.output().map_err(|e| Error::Git {
        repo: repo.display().to_string(),
        message: e.to_string(),
        stderr: String::new(),
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(Error::Git {
            repo: repo.display().to_string(),
            message: format!("{:?} failed", cmd.get_program()),
            stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        }
        .into())
    }
}

pub struct CommitInfo {
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub message: String,
}

pub fn resolve_ref(repo: &Path, refname: &str) -> Result<String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", refname]),
        repo,
    )
}

pub fn commit_info(repo: &Path, sha: &str) -> Result<CommitInfo> {
    let fmt = "%P%n%an%n%ae%n%aI%n%cn%n%ce%n%cI%n%B";
    let raw = run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["show", "-s", &format!("--pretty=tformat:{fmt}"), sha]),
        repo,
    )?;

    let mut lines = raw.splitn(8, '\n');
    let parents_line = lines.next().unwrap_or("").trim().to_string();
    let parents = if parents_line.is_empty() {
        vec![]
    } else {
        parents_line.split_whitespace().map(str::to_owned).collect()
    };

    Ok(CommitInfo {
        parents,
        author_name: lines.next().unwrap_or("").to_string(),
        author_email: lines.next().unwrap_or("").to_string(),
        author_date: lines.next().unwrap_or("").to_string(),
        committer_name: lines.next().unwrap_or("").to_string(),
        committer_email: lines.next().unwrap_or("").to_string(),
        committer_date: lines.next().unwrap_or("").to_string(),
        message: lines.next().unwrap_or("").to_string(),
    })
}

pub fn worktree_add(repo: &Path, wt_path: &Path, sha: &str) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add"])
            .arg(wt_path)
            .arg(sha),
        repo,
    )?;
    Ok(())
}

pub fn worktree_add_orphan(repo: &Path, wt_path: &Path, branch: &str) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "--orphan", "-b", branch])
            .arg(wt_path),
        repo,
    )?;
    Ok(())
}

pub fn worktree_prune(repo: &Path) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "prune"]),
        repo,
    )?;
    Ok(())
}

pub fn write_tree(wt_path: &Path) -> Result<String> {
    run(
        Command::new("git").arg("-C").arg(wt_path).arg("write-tree"),
        wt_path,
    )
}

pub struct CommitTreeArgs<'a> {
    pub repo: &'a Path,
    pub tree: &'a str,
    pub parents: &'a [String],
    pub message: &'a str,
    pub author_name: &'a str,
    pub author_email: &'a str,
    pub author_date: &'a str,
    pub committer_name: &'a str,
    pub committer_email: &'a str,
    pub committer_date: &'a str,
}

pub fn commit_tree(args: CommitTreeArgs<'_>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(args.repo)
        .arg("commit-tree")
        .arg(args.tree);
    for p in args.parents {
        cmd.args(["-p", p]);
    }
    cmd.args(["-m", args.message]);
    cmd.env("GIT_AUTHOR_NAME", args.author_name)
        .env("GIT_AUTHOR_EMAIL", args.author_email)
        .env("GIT_AUTHOR_DATE", args.author_date)
        .env("GIT_COMMITTER_NAME", args.committer_name)
        .env("GIT_COMMITTER_EMAIL", args.committer_email)
        .env("GIT_COMMITTER_DATE", args.committer_date);
    run(&mut cmd, args.repo)
}

pub fn update_ref(repo: &Path, refname: &str, sha: &str) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["update-ref", refname, sha]),
        repo,
    )?;
    Ok(())
}

pub fn push(repo: &Path, refspecs: &[String]) -> Result<()> {
    if refspecs.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).arg("push").arg("origin");
    for rs in refspecs {
        cmd.arg(rs);
    }
    run(&mut cmd, repo)?;
    Ok(())
}

pub fn git_add_all(wt_path: &Path) -> Result<()> {
    run(
        Command::new("git").arg("-C").arg(wt_path).args(["add", "."]),
        wt_path,
    )?;
    Ok(())
}

pub fn commit_tree_sha(repo: &Path, sha: &str) -> Result<String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", &format!("{sha}^{{tree}}")]),
        repo,
    )
}

pub fn clone_bare(target_url: &str, dest: &Path) -> Result<()> {
    run(
        Command::new("git")
            .args(["clone", "--bare", "--filter=tree:0", target_url])
            .arg(dest),
        dest,
    )?;
    Ok(())
}

pub fn config_add(repo: &Path, key: &str, value: &str) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["config", "--add", key, value]),
        repo,
    )?;
    Ok(())
}

/// Returns all values for the given config key; empty vec if the key is absent.
pub fn config_get_all(repo: &Path, key: &str) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get-all", key])
        .output()
        .map_err(|e| Error::Git {
            repo: repo.display().to_string(),
            message: e.to_string(),
            stderr: String::new(),
        })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect());
    }
    // exit 1 with no output means key not found — not an error
    if out.status.code() == Some(1) && out.stdout.is_empty() {
        return Ok(vec![]);
    }
    Err(Error::Git {
        repo: repo.display().to_string(),
        message: format!("git config --get-all {key} failed"),
        stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
    }
    .into())
}

pub fn fetch(repo: &Path) -> Result<()> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["fetch", "--filter=tree:0", "origin"]),
        repo,
    )?;
    Ok(())
}

/// Returns `(full_refname, commit_sha)` for all refs under `prefixes`.
/// Annotated tags are resolved to their tagged commit via `%(*objectname)`.
pub fn for_each_ref(repo: &Path, prefixes: &[&str]) -> Result<Vec<(String, String)>> {
    let fmt = "%(refname)\t%(*objectname)\t%(objectname)";
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(["for-each-ref", &format!("--format={fmt}")]);
    for p in prefixes {
        cmd.arg(p);
    }
    let out = run(&mut cmd, repo)?;
    let mut result = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(3, '\t');
        let refname = parts.next().unwrap_or("");
        let deref_sha = parts.next().unwrap_or("");
        let obj_sha = parts.next().unwrap_or("");
        if refname.is_empty() {
            continue;
        }
        let sha = if deref_sha.is_empty() { obj_sha } else { deref_sha };
        if !sha.is_empty() {
            result.push((refname.to_string(), sha.to_string()));
        }
    }
    Ok(result)
}

/// Returns the short name of the branch HEAD points to, or `None` for
/// detached HEAD.
pub fn current_branch(repo: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .map_err(|e| Error::Git {
            repo: repo.display().to_string(),
            message: e.to_string(),
            stderr: String::new(),
        })?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Returns `(full_sha, subject_line)` for every commit reachable from
/// `refname`, in reverse-chronological order.
pub fn log_commits(repo: &Path, refname: &str) -> Result<Vec<(String, String)>> {
    let out = run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "--format=%H\t%s", refname]),
        repo,
    )?;
    let mut result = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(2, '\t');
        let sha = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        if !sha.is_empty() {
            result.push((sha, subject));
        }
    }
    Ok(result)
}

/// Returns `full_sha → subject_line` for the given commits (no parent
/// traversal).  Reads all commits in one subprocess.
pub fn commit_subjects(repo: &Path, shas: &[&str]) -> Result<HashMap<String, String>> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["log", "--no-walk", "--format=%H\t%s"]);
    for sha in shas {
        cmd.arg(sha);
    }
    let out = run(&mut cmd, repo)?;
    let mut result = HashMap::new();
    for line in out.lines() {
        let mut parts = line.splitn(2, '\t');
        let sha = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        if !sha.is_empty() {
            result.insert(sha, subject);
        }
    }
    Ok(result)
}

pub fn repo_root() -> Result<String> {
    let cwd = Path::new(".");
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::Git {
            repo: cwd.display().to_string(),
            message: e.to_string(),
            stderr: String::new(),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(Error::Git {
            repo: cwd.display().to_string(),
            message: "not inside a git repository".to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        }
        .into())
    }
}

/// Returns the common git directory — the one that contains `hooks/`, `config`,
/// etc.  In a linked worktree `--git-dir` points to `.git/worktrees/<name>`,
/// while `--git-common-dir` always points to the root `.git`.  In a normal
/// (non-worktree) checkout the two values are identical.
pub fn git_common_dir() -> Result<String> {
    let cwd = Path::new(".");
    let out = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| Error::Git {
            repo: cwd.display().to_string(),
            message: e.to_string(),
            stderr: String::new(),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(Error::Git {
            repo: cwd.display().to_string(),
            message: "not inside a git repository".to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        }
        .into())
    }
}

