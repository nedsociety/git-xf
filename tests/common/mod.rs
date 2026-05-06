// Shared test infrastructure for integration tests.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BIN: &str = env!("CARGO_BIN_EXE_git-xf");

// ── test environment ──────────────────────────────────────────────────────────

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl Env {
    /// Create a fresh test environment.
    ///
    /// `config_yaml` is written to `<source>/.git-xf.yaml`.  Use
    /// `"../target.git"` as the target path — the source and target repos are
    /// siblings inside the same temp directory.
    pub fn new(config_yaml: &str) -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let target = tmp.path().join("target.git");

        bare_init(&target);

        fs::create_dir_all(&source).unwrap();
        git(&source, &["-c", "init.defaultBranch=main", "init"]);
        git(&source, &["config", "user.name", "Tester"]);
        git(&source, &["config", "user.email", "tester@example.com"]);
        git(&source, &["config", "commit.gpgsign", "false"]);

        fs::write(source.join(".git-xf.yaml"), config_yaml).unwrap();

        Self {
            _tmp: tmp,
            source,
            target,
        }
    }

    /// Stage and commit files; return the new HEAD SHA.
    pub fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
        for (name, content) in files {
            let p = self.source.join(name);
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(p, content).unwrap();
        }
        git(&self.source, &["add", "."]);
        git(&self.source, &["commit", "--allow-empty", "-m", msg]);
        git_read(&self.source, &["rev-parse", "HEAD"])
    }

    pub fn create_branch(&self, name: &str) {
        git(&self.source, &["checkout", "-b", name]);
    }

    pub fn checkout(&self, name: &str) {
        git(&self.source, &["checkout", name]);
    }

    pub fn tag(&self, name: &str) {
        git(&self.source, &["tag", name]);
    }

    pub fn merge_no_ff(&self, branch: &str, msg: &str) -> String {
        git(&self.source, &["merge", "--no-ff", "-m", msg, branch]);
        git_read(&self.source, &["rev-parse", "HEAD"])
    }

    pub fn merge_unrelated(&self, branch: &str, msg: &str) -> String {
        git(
            &self.source,
            &[
                "merge",
                "--no-ff",
                "--allow-unrelated-histories",
                "-m",
                msg,
                branch,
            ],
        );
        git_read(&self.source, &["rev-parse", "HEAD"])
    }

    /// Creates an orphan branch (no parents) with the given commit.
    pub fn commit_orphan(&self, branch: &str, msg: &str, files: &[(&str, &str)]) -> String {
        git(&self.source, &["checkout", "--orphan", branch]);
        // `--orphan` inherits the index and working tree from the previous checkout.
        // Remove all tracked content from the index and then clean untracked files
        // so only the files we explicitly provide end up in this commit.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.source)
            .args(["rm", "-rf", "--cached", "."])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.source)
            .args(["clean", "-fdx", "--", "."])
            .output();
        for (name, content) in files {
            let p = self.source.join(name);
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(p, content).unwrap();
        }
        git(&self.source, &["add", "."]);
        git(&self.source, &["commit", "--allow-empty", "-m", msg]);
        git_read(&self.source, &["rev-parse", "HEAD"])
    }

    // ── subcommand helpers ────────────────────────────────────────────────────

    pub fn run_status(&self, extra_args: &[&str]) -> String {
        let out = Command::new(BIN)
            .current_dir(&self.source)
            .arg("status")
            .args(extra_args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git-xf status failed\nstderr: {}",
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    pub fn run_hook_cmd(&self, subcommand: &str) -> std::process::Output {
        Command::new(BIN)
            .current_dir(&self.source)
            .args(["hook", subcommand])
            .output()
            .unwrap()
    }

    pub fn hook_path(&self) -> std::path::PathBuf {
        self.source.join(".git/hooks/pre-push")
    }

    // ── sync helpers ──────────────────────────────────────────────────────────

    pub fn sync(&self, extra_args: &[&str]) {
        let out = Command::new(BIN)
            .current_dir(&self.source)
            .arg("sync")
            .args(extra_args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git-xf sync failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    pub fn sync_output(&self, extra_args: &[&str]) -> (String, String) {
        let out = Command::new(BIN)
            .current_dir(&self.source)
            .arg("sync")
            .args(extra_args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    // ── target assertions ──────────────────────────────────────────────────────

    pub fn target_ref_sha(&self, refname: &str) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["rev-parse", "--verify", refname])
            .output()
            .unwrap();
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
        } else {
            None
        }
    }

    pub fn target_ref_exists(&self, refname: &str) -> bool {
        self.target_ref_sha(refname).is_some()
    }

    pub fn target_commit_count(&self, refname: &str) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["log", "--oneline", refname])
            .output()
            .unwrap();
        if !out.status.success() {
            return 0;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count()
    }

    pub fn target_commit_message(&self, sha: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["log", "-1", "--format=%B", sha])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    pub fn target_parent_count(&self, sha: &str) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["log", "-1", "--format=%P", sha])
            .output()
            .unwrap();
        let parents = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        if parents.is_empty() {
            0
        } else {
            parents.split_whitespace().count()
        }
    }

    pub fn target_tree_files(&self, sha: &str) -> Vec<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["ls-tree", "--name-only", sha])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }

    pub fn target_file_content(&self, tree_sha: &str, path: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["show", &format!("{tree_sha}:{path}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    // ── diff helper ───────────────────────────────────────────────────────────

    pub fn run_diff(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .current_dir(&self.source)
            .arg("diff")
            .args(args)
            .output()
            .unwrap()
    }

    // ── pr helper ─────────────────────────────────────────────────────────────

    pub fn run_pr(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .current_dir(&self.source)
            .arg("pr")
            .args(args)
            .output()
            .unwrap()
    }

    // ── push-chunk counter helpers ────────────────────────────────────────────

    /// Installs a post-receive hook in the target bare repo that appends one
    /// line to `push-log.txt` per push invocation.
    pub fn install_post_receive_counter(&self) {
        let hook = self.target.join("hooks/post-receive");
        fs::write(
            &hook,
            "#!/usr/bin/env sh\necho push >> \"$GIT_DIR/push-log.txt\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook, perms).unwrap();
        }
    }

    /// Returns the number of push invocations recorded by the hook counter.
    pub fn push_log_count(&self) -> usize {
        let log = self.target.join("push-log.txt");
        fs::read_to_string(&log)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }
}

// ── git helpers ───────────────────────────────────────────────────────────────

pub fn bare_init(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let out = Command::new("git")
        .args(["init", "--bare"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init --bare failed");
}

pub fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        repo.display(),
        String::from_utf8_lossy(&out.stderr),
    );
}

pub fn git_read(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        repo.display(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

// ── config templates ───────────────────────────────────────────────────────────

pub fn passthrough_config() -> &'static str {
    "test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n"
}

pub fn two_transform_config() -> &'static str {
    "xform1:\n  target: ../target.git\n  rule:\n    command: \"true\"\nxform2:\n  target: ../target2.git\n  rule:\n    command: \"true\"\n"
}

// ── misc helpers ──────────────────────────────────────────────────────────────

pub fn indoc(s: &str) -> String {
    // Strip common leading whitespace so inline test configs stay readable.
    let indent = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    s.lines()
        .map(|l| if l.len() >= indent { &l[indent..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn set_cache_remote_url(env: &Env, transform: &str, url: &str) {
    let cache = env
        .source
        .join(".git/git-xf")
        .join(format!("{transform}.git"));
    git(&cache, &["config", "remote.origin.url", url]);
}
