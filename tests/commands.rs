// Integration tests: git xf init, git xf status, git xf hook.

mod common;
use common::*;

use std::fs;
use std::io::Write;
use std::process::Command;

// ── git xf init ───────────────────────────────────────────────────────────────

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
    assert!(
        cache.exists(),
        "cache directory not created: {}",
        cache.display()
    );

    // The cache must be a valid bare git repo.
    let out = Command::new("git")
        .arg("-C")
        .arg(&cache)
        .args(["rev-parse", "--git-dir"])
        .output()
        .unwrap();
    assert!(out.status.success(), "cache is not a valid git repo");
}

// ── git xf status ─────────────────────────────────────────────────────────────

/// Before any sync, every commit on the branch shows as Pending.
#[test]
fn test_status_all_pending() {
    let env = Env::new(passthrough_config());
    env.commit("alpha", &[("a.txt", "a")]);
    env.commit("beta", &[("b.txt", "b")]);

    let out = env.run_status(&[]);

    assert!(
        out.contains("Pending"),
        "expected Pending in status output:\n{out}"
    );
    assert!(
        !out.contains("Mapped"),
        "unexpected Mapped in status output:\n{out}"
    );
    assert!(
        out.contains("(0/2 mapped, 0 failed)"),
        "unexpected counts:\n{out}"
    );
}

/// After a full sync, every commit on the branch shows as Mapped.
#[test]
fn test_status_all_mapped() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    env.commit("second", &[("b.txt", "b")]);
    env.sync(&[]);

    let out = env.run_status(&[]);

    assert!(
        out.contains("Mapped"),
        "expected Mapped in status output:\n{out}"
    );
    assert!(
        !out.contains("Pending"),
        "unexpected Pending in status output:\n{out}"
    );
    assert!(
        out.contains("(2/2 mapped, 0 failed)"),
        "unexpected counts:\n{out}"
    );
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
    assert!(
        out.contains("(1/3 mapped, 0 failed)"),
        "unexpected counts:\n{out}"
    );
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

    assert!(
        out.contains("Failed"),
        "expected Failed in status output:\n{out}"
    );
    assert!(
        !out.contains("Mapped"),
        "unexpected Mapped in status output:\n{out}"
    );
    assert!(
        out.contains("(0/1 mapped, 1 failed)"),
        "unexpected counts:\n{out}"
    );
}

// ── git xf hook ───────────────────────────────────────────────────────────────

/// `hook install` writes the hook script and makes it executable.
#[test]
fn test_hook_install() {
    let env = Env::new(passthrough_config());
    let out = env.run_hook_cmd("install");
    assert!(
        out.status.success(),
        "hook install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let hook = env.hook_path();
    assert!(hook.exists(), "pre-push hook file not created");

    let content = fs::read_to_string(&hook).unwrap();
    assert!(
        content.contains("# Installed by git-xf."),
        "MARKER missing from hook:\n{content}"
    );
    assert!(
        content.contains("git xf hook run"),
        "hook body missing 'git xf hook run':\n{content}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hook).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "hook is not executable (mode {:o})",
            mode
        );
    }
}

/// `hook uninstall` removes a hook installed by git-xf.
#[test]
fn test_hook_uninstall() {
    let env = Env::new(passthrough_config());
    env.run_hook_cmd("install");
    assert!(env.hook_path().exists(), "install did not create hook");

    let out = env.run_hook_cmd("uninstall");
    assert!(
        out.status.success(),
        "hook uninstall failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !env.hook_path().exists(),
        "hook file still present after uninstall"
    );
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
    assert!(
        hook.exists(),
        "foreign hook was removed — should have been left alone"
    );
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(
        out.status.success(),
        "hook run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Sync should have run and pushed the commit.
    assert!(
        env.target_ref_exists("refs/heads/main"),
        "main not pushed after hook run"
    );
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(out.status.success(), "hook run failed unexpectedly");
    // Branch is not in whitelist — no sync should have run.
    assert!(
        !env.target_ref_exists("refs/heads/main"),
        "main should not be pushed"
    );
}
