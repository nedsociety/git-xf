// Integration tests: spin up real git repos, run git-xf as a subprocess,
// assert the target repo state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git-xf");

// ── test environment ──────────────────────────────────────────────────────────

struct Env {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Env {
    /// Create a fresh test environment.
    ///
    /// `config_yaml` is written to `<source>/.git-xf.yaml`.  Use
    /// `"../target.git"` as the target path — the source and target repos are
    /// siblings inside the same temp directory.
    fn new(config_yaml: &str) -> Self {
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

        Self { _tmp: tmp, source, target }
    }

    /// Stage and commit files; return the new HEAD SHA.
    fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
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

    fn create_branch(&self, name: &str) {
        git(&self.source, &["checkout", "-b", name]);
    }

    fn checkout(&self, name: &str) {
        git(&self.source, &["checkout", name]);
    }

    fn tag(&self, name: &str) {
        git(&self.source, &["tag", name]);
    }

    fn merge_no_ff(&self, branch: &str, msg: &str) -> String {
        git(&self.source, &["merge", "--no-ff", "-m", msg, branch]);
        git_read(&self.source, &["rev-parse", "HEAD"])
    }

    // ── subcommand helpers ────────────────────────────────────────────────────

    fn run_status(&self, extra_args: &[&str]) -> String {
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

    fn run_hook_cmd(&self, subcommand: &str) -> std::process::Output {
        Command::new(BIN)
            .current_dir(&self.source)
            .args(["hook", subcommand])
            .output()
            .unwrap()
    }

    fn hook_path(&self) -> std::path::PathBuf {
        self.source.join(".git/hooks/pre-push")
    }

    // ── sync helpers ──────────────────────────────────────────────────────────

    fn sync(&self, extra_args: &[&str]) {
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

    fn sync_output(&self, extra_args: &[&str]) -> (String, String) {
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

    fn target_ref_sha(&self, refname: &str) -> Option<String> {
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

    fn target_ref_exists(&self, refname: &str) -> bool {
        self.target_ref_sha(refname).is_some()
    }

    fn target_commit_count(&self, refname: &str) -> usize {
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

    fn target_commit_message(&self, sha: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["log", "-1", "--format=%B", sha])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn target_parent_count(&self, sha: &str) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["log", "-1", "--format=%P", sha])
            .output()
            .unwrap();
        let parents = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        if parents.is_empty() { 0 } else { parents.split_whitespace().count() }
    }

    fn target_tree_files(&self, sha: &str) -> Vec<String> {
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

    fn target_file_content(&self, tree_sha: &str, path: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.target)
            .args(["show", &format!("{tree_sha}:{path}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

// ── git helpers ───────────────────────────────────────────────────────────────

fn bare_init(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let out = Command::new("git")
        .args(["init", "--bare"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init --bare failed");
}

fn git(repo: &Path, args: &[&str]) {
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

fn git_read(repo: &Path, args: &[&str]) -> String {
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

fn passthrough_config() -> &'static str {
    "test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n"
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Three-commit linear history is fully transformed with correct message trailers.
#[test]
fn test_sync_linear() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    let sha3 = env.commit("third", &[("c.txt", "ccc")]);

    env.sync(&[]);

    // Three target commits on main.
    assert_eq!(env.target_commit_count("refs/heads/main"), 3);

    // Tip mapping ref pushed to target.
    let mapping_ref = format!("refs/git-xf/test/{sha3}");
    assert!(env.target_ref_exists(&mapping_ref), "mapping ref missing: {mapping_ref}");

    // Target tip commit contains the expected trailers.
    let tip_sha = env.target_ref_sha("refs/heads/main").unwrap();
    let msg = env.target_commit_message(&tip_sha);
    assert!(msg.contains("git-xf-source:"), "missing git-xf-source trailer: {msg}");
    assert!(msg.contains("git-xf-transform: test"), "missing git-xf-transform trailer: {msg}");
    assert!(msg.contains("third"), "source message not preserved: {msg}");
}

/// Second sync run skips commits that are already mapped.
#[test]
fn test_sync_incremental() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);

    // Record the target SHA that sha1 mapped to.
    let mapping1 = format!("refs/git-xf/test/{sha1}");
    let target_sha1_after_first = env.target_ref_sha(&mapping1).unwrap();

    // Add another commit and sync again.
    env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    // Two commits in target now.
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);

    // The first commit's mapping is unchanged — it was not re-transformed.
    let target_sha1_after_second = env.target_ref_sha(&mapping1).unwrap();
    assert_eq!(
        target_sha1_after_first, target_sha1_after_second,
        "first commit was re-transformed on the second sync"
    );
}

/// `--dry-run` prints the list of commits but leaves the target empty.
#[test]
fn test_sync_dry_run() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("alpha", &[("a.txt", "aaa")]);
    let sha2 = env.commit("beta", &[("b.txt", "bbb")]);

    let (stdout, _stderr) = env.sync_output(&["--dry-run"]);

    // Both source SHAs appear in stdout.
    assert!(stdout.contains(&sha1), "stdout missing sha1:\n{stdout}");
    assert!(stdout.contains(&sha2), "stdout missing sha2:\n{stdout}");

    // Target has no refs (nothing was pushed).
    assert!(!env.target_ref_exists("refs/heads/main"), "target main should not exist after dry-run");

    // Running a second dry-run after a real sync should produce no output.
    env.sync(&[]);
    let (stdout2, _) = env.sync_output(&["--dry-run"]);
    assert!(stdout2.is_empty(), "dry-run after full sync should produce no output:\n{stdout2}");
}

/// All source branches are mirrored to the target after sync.
///
/// Feature branch is created before the final main commit so its tip is a
/// commit that main's ancestry also passes through.  Syncing HEAD (main)
/// transforms all of main's history, which includes the feature tip, so the
/// mirror step can propagate the feature ref without requiring it to be an
/// explicit REF argument.
#[test]
fn test_sync_branch_mirror() {
    let env = Env::new(passthrough_config());
    env.commit("shared base", &[("base.txt", "base")]);
    // feature stays at "shared base"; main advances past it.
    env.create_branch("feature");
    env.checkout("main");
    env.commit("main only", &[("main.txt", "m")]);

    env.sync(&[]);

    assert!(env.target_ref_exists("refs/heads/main"), "main missing from target");
    assert!(env.target_ref_exists("refs/heads/feature"), "feature missing from target");

    // feature points to "shared base" (1 commit), main points to "main only" (2 commits).
    let main_tip = env.target_ref_sha("refs/heads/main").unwrap();
    let feat_tip = env.target_ref_sha("refs/heads/feature").unwrap();
    assert_ne!(main_tip, feat_tip, "main and feature tips should differ");
}

/// Source tags are mirrored to the target after sync.
#[test]
fn test_sync_tag_mirror() {
    let env = Env::new(passthrough_config());
    env.commit("initial", &[("a.txt", "a")]);
    env.tag("v1.0");
    env.commit("after tag", &[("b.txt", "b")]);

    env.sync(&[]);

    assert!(env.target_ref_exists("refs/tags/v1.0"), "tag v1.0 missing from target");

    // The tag tip should differ from the main tip (it points to the first commit).
    let tag_sha = env.target_ref_sha("refs/tags/v1.0").unwrap();
    let main_sha = env.target_ref_sha("refs/heads/main").unwrap();
    assert_ne!(tag_sha, main_sha, "v1.0 tag should point to first commit, not tip");
}

/// A merge commit in the source produces a merge commit (two parents) in the target.
#[test]
fn test_sync_merge_commit() {
    let env = Env::new(passthrough_config());
    env.commit("root", &[("root.txt", "r")]);

    env.create_branch("side");
    env.commit("side commit", &[("side.txt", "s")]);
    env.checkout("main");
    env.commit("main commit", &[("main.txt", "m")]);

    let merge_sha = env.merge_no_ff("side", "merge side into main");

    env.sync(&[]);

    // Find the target SHA for the merge commit.
    let mapping_ref = format!("refs/git-xf/test/{merge_sha}");
    let target_merge_sha = env.target_ref_sha(&mapping_ref)
        .unwrap_or_else(|| panic!("merge mapping ref missing: {mapping_ref}"));

    assert_eq!(
        env.target_parent_count(&target_merge_sha), 2,
        "target merge commit should have 2 parents"
    );
}

/// `skip-commit-messages`: commits whose message matches are mapped to their
/// parent's target SHA and elided from target history.
#[test]
fn test_skip_commit_messages() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n  skip-commit-messages:\n    - \"[skip-xf]\"\n";
    let env = Env::new(config);

    let sha_base = env.commit("base", &[("a.txt", "aaa")]);
    let sha_skip = env.commit("fix thing [skip-xf]", &[("b.txt", "bbb")]);
    let sha_after = env.commit("after skip", &[("c.txt", "ccc")]);

    env.sync(&[]);

    // The skipped commit maps to the same target as its parent.
    let base_target = env.target_ref_sha(&format!("refs/git-xf/test/{sha_base}")).unwrap();
    let skip_target = env.target_ref_sha(&format!("refs/git-xf/test/{sha_skip}")).unwrap();
    assert_eq!(
        base_target, skip_target,
        "skipped commit should map to parent's target SHA"
    );

    // The commit after the skip has a non-skip mapping.
    let after_target = env.target_ref_sha(&format!("refs/git-xf/test/{sha_after}")).unwrap();
    assert_ne!(after_target, skip_target, "commit after skip should have its own target SHA");

    // Target has 2 real commits (base + after), not 3.
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);
}

/// `changeless: skip` — a commit that produces the same tree as its parent
/// target is mapped to the parent's SHA rather than creating an empty commit.
#[test]
fn test_changeless_skip() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n  changeless: skip\n";
    let env = Env::new(config);

    let sha1 = env.commit("real change", &[("a.txt", "content")]);
    // This commit changes nothing in the tracked tree from git's perspective,
    // but we use --allow-empty to force a new source SHA with same tree.
    let sha2 = env.commit("empty source commit", &[]);

    env.sync(&[]);

    let target1 = env.target_ref_sha(&format!("refs/git-xf/test/{sha1}")).unwrap();
    let target2 = env.target_ref_sha(&format!("refs/git-xf/test/{sha2}")).unwrap();
    assert_eq!(
        target1, target2,
        "changeless:skip commit should map to parent's target SHA"
    );

    // Only one real target commit.
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `changeless: empty-commit` (the default) — produces a real target commit
/// even when the tree is unchanged.
#[test]
fn test_changeless_empty_commit() {
    let env = Env::new(passthrough_config()); // default: changeless = empty-commit
    env.commit("real change", &[("a.txt", "content")]);
    env.commit("no-op commit", &[]);

    env.sync(&[]);

    // Both source commits produce distinct target commits.
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);
}

/// `ignore-error: empty-commit` — a failing command creates a target commit
/// with the `[git-xf error]` marker in its message.
#[test]
fn test_ignore_error_empty_commit() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"exit 1\"\n  ignore-error: empty-commit\n";
    let env = Env::new(config);

    let sha = env.commit("will fail", &[("a.txt", "content")]);
    env.sync(&[]);

    let target_sha = env.target_ref_sha(&format!("refs/git-xf/test/{sha}")).unwrap();
    let msg = env.target_commit_message(&target_sha);
    assert!(
        msg.contains("[git-xf error]"),
        "error commit message missing [git-xf error] marker: {msg}"
    );
    assert!(
        msg.contains("test"),
        "error commit message missing transform name: {msg}"
    );
}

/// `ignore-error: skip` — a failing commit is mapped to its parent and
/// does not appear in target history.
///
/// The command `[ ! -f fail-me.txt ]` succeeds for the base commit (no such
/// file) and fails for the second commit (which adds that file), so only the
/// second commit triggers the ignore-error policy.
#[test]
fn test_ignore_error_skip() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"[ ! -f fail-me.txt ]\"\n  ignore-error: skip\n";
    let env = Env::new(config);

    let sha_base = env.commit("base", &[("a.txt", "base")]);
    let sha_fail = env.commit("will fail", &[("fail-me.txt", "trigger")]);

    env.sync(&[]);

    let base_target = env.target_ref_sha(&format!("refs/git-xf/test/{sha_base}")).unwrap();
    let fail_target = env.target_ref_sha(&format!("refs/git-xf/test/{sha_fail}")).unwrap();

    assert_eq!(
        base_target, fail_target,
        "ignore-error:skip commit should map to parent's target SHA"
    );
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `rule.output` — only the specified paths appear in the target commit tree.
#[test]
fn test_output_filter() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n    output:\n      - keep.txt\n";
    let env = Env::new(config);

    env.commit("both files", &[("keep.txt", "keep"), ("skip.txt", "skip")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let files = env.target_tree_files(&tip);
    assert!(files.contains(&"keep.txt".to_string()), "keep.txt missing: {files:?}");
    assert!(!files.contains(&"skip.txt".to_string()), "skip.txt should not be in target: {files:?}");
    assert_eq!(env.target_file_content(&tip, "keep.txt"), "keep");
}

/// `git xf init` creates the local cache directory without error.
#[test]
fn test_init() {
    let env = Env::new(passthrough_config());

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .arg("init")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git xf init failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let cache = env.source.join(".git/git-xf/test.git");
    assert!(cache.exists(), "cache directory not created: {}", cache.display());

    // The cache must be a valid bare git repo.
    let out = Command::new("git")
        .arg("-C")
        .arg(&cache)
        .args(["rev-parse", "--git-dir"])
        .output()
        .unwrap();
    assert!(out.status.success(), "cache is not a valid git repo");
}

// ── git xf status tests ───────────────────────────────────────────────────────

/// Before any sync, every commit on the branch shows as Pending.
#[test]
fn test_status_all_pending() {
    let env = Env::new(passthrough_config());
    env.commit("alpha", &[("a.txt", "a")]);
    env.commit("beta", &[("b.txt", "b")]);

    let out = env.run_status(&[]);

    assert!(out.contains("Pending"), "expected Pending in status output:\n{out}");
    assert!(!out.contains("Mapped"), "unexpected Mapped in status output:\n{out}");
    assert!(out.contains("(0/2 mapped, 0 failed)"), "unexpected counts:\n{out}");
}

/// After a full sync, every commit on the branch shows as Mapped.
#[test]
fn test_status_all_mapped() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    env.commit("second", &[("b.txt", "b")]);
    env.sync(&[]);

    let out = env.run_status(&[]);

    assert!(out.contains("Mapped"), "expected Mapped in status output:\n{out}");
    assert!(!out.contains("Pending"), "unexpected Pending in status output:\n{out}");
    assert!(out.contains("(2/2 mapped, 0 failed)"), "unexpected counts:\n{out}");
}

/// After a partial sync (only the earliest commit), later commits are Pending.
#[test]
fn test_status_mixed() {
    let env = Env::new(passthrough_config());
    let sha_a = env.commit("first", &[("a.txt", "a")]);
    env.commit("second", &[("b.txt", "b")]);
    env.commit("third", &[("c.txt", "c")]);

    // Sync only the earliest commit by passing its SHA as the REF argument.
    env.sync(&[&sha_a]);

    let out = env.run_status(&[]);

    assert!(out.contains("Mapped"), "expected Mapped:\n{out}");
    assert!(out.contains("Pending"), "expected Pending:\n{out}");
    assert!(out.contains("(1/3 mapped, 0 failed)"), "unexpected counts:\n{out}");
    // Subjects should appear in the output.
    assert!(out.contains("first"), "subject 'first' missing:\n{out}");
    assert!(out.contains("third"), "subject 'third' missing:\n{out}");
}

/// A commit whose transform failed with `ignore-error: empty-commit` shows as Failed.
#[test]
fn test_status_failed() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"exit 1\"\n  ignore-error: empty-commit\n";
    let env = Env::new(config);
    env.commit("will fail", &[("a.txt", "a")]);
    env.sync(&[]);

    let out = env.run_status(&[]);

    assert!(out.contains("Failed"), "expected Failed in status output:\n{out}");
    assert!(!out.contains("Mapped"), "unexpected Mapped in status output:\n{out}");
    assert!(out.contains("(0/1 mapped, 1 failed)"), "unexpected counts:\n{out}");
}

// ── git xf hook tests ─────────────────────────────────────────────────────────

/// `hook install` writes the hook script and makes it executable.
#[test]
fn test_hook_install() {
    let env = Env::new(passthrough_config());
    let out = env.run_hook_cmd("install");
    assert!(out.status.success(), "hook install failed: {}", String::from_utf8_lossy(&out.stderr));

    let hook = env.hook_path();
    assert!(hook.exists(), "pre-push hook file not created");

    let content = fs::read_to_string(&hook).unwrap();
    assert!(content.contains("# Installed by git-xf."), "MARKER missing from hook:\n{content}");
    assert!(content.contains("git xf hook run"), "hook body missing 'git xf hook run':\n{content}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hook).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "hook is not executable (mode {:o})", mode);
    }
}

/// `hook uninstall` removes a hook installed by git-xf.
#[test]
fn test_hook_uninstall() {
    let env = Env::new(passthrough_config());
    env.run_hook_cmd("install");
    assert!(env.hook_path().exists(), "install did not create hook");

    let out = env.run_hook_cmd("uninstall");
    assert!(out.status.success(), "hook uninstall failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!env.hook_path().exists(), "hook file still present after uninstall");
}

/// `hook uninstall` does not remove a hook that was not installed by git-xf.
#[test]
fn test_hook_uninstall_preserves_foreign_hook() {
    let env = Env::new(passthrough_config());

    let hook = env.hook_path();
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\necho 'custom hook'\n").unwrap();

    let out = env.run_hook_cmd("uninstall");
    assert!(out.status.success(), "hook uninstall failed unexpectedly");
    assert!(hook.exists(), "foreign hook was removed — should have been left alone");
}

/// `hook run` reads push descriptions from stdin and triggers sync for branches
/// whose name matches the `branches` whitelist in the config.
#[test]
fn test_hook_run_matching_branch() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n  branches:\n    - main\n";
    let env = Env::new(config);
    let sha = env.commit("commit for hook", &[("a.txt", "hook")]);

    // Simulate what git passes to the pre-push hook on stdin.
    let zero = "0000000000000000000000000000000000000000";
    let stdin_line = format!("refs/heads/main {sha} refs/heads/main {zero}\n");

    let mut child = Command::new(BIN)
        .current_dir(&env.source)
        .args(["hook", "run"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin_line.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(
        out.status.success(),
        "hook run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Sync should have run and pushed the commit.
    assert!(env.target_ref_exists("refs/heads/main"), "main not pushed after hook run");
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `hook run` does nothing when the pushed branch is not in any `branches` list.
#[test]
fn test_hook_run_non_matching_branch() {
    let config = "\
test:\n  target: ../target.git\n  rule:\n    command: \"true\"\n  branches:\n    - other\n";
    let env = Env::new(config);
    let sha = env.commit("commit", &[("a.txt", "x")]);

    let zero = "0000000000000000000000000000000000000000";
    let stdin_line = format!("refs/heads/main {sha} refs/heads/main {zero}\n");

    let mut child = Command::new(BIN)
        .current_dir(&env.source)
        .args(["hook", "run"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin_line.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(out.status.success(), "hook run failed unexpectedly");
    // Branch is not in whitelist — no sync should have run.
    assert!(!env.target_ref_exists("refs/heads/main"), "main should not be pushed");
}
