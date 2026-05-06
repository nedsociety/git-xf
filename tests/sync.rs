// Integration tests: sync, init, status, hook, rule, --all-branches, --depth, --push-chunk.

mod common;
use common::*;

use std::fs;
use std::io::Write;
use std::process::Command;

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
    assert!(
        env.target_ref_exists(&mapping_ref),
        "mapping ref missing: {mapping_ref}"
    );

    // Target tip commit contains the expected trailers.
    let tip_sha = env.target_ref_sha("refs/heads/main").unwrap();
    let msg = env.target_commit_message(&tip_sha);
    assert!(
        msg.contains("git-xf-source:"),
        "missing git-xf-source trailer: {msg}"
    );
    assert!(
        msg.contains("git-xf-transform: test"),
        "missing git-xf-transform trailer: {msg}"
    );
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
    assert!(
        !env.target_ref_exists("refs/heads/main"),
        "target main should not exist after dry-run"
    );

    // Running a second dry-run after a real sync should produce no output.
    env.sync(&[]);
    let (stdout2, _) = env.sync_output(&["--dry-run"]);
    assert!(
        stdout2.is_empty(),
        "dry-run after full sync should produce no output:\n{stdout2}"
    );
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

    assert!(
        env.target_ref_exists("refs/heads/main"),
        "main missing from target"
    );
    assert!(
        env.target_ref_exists("refs/heads/feature"),
        "feature missing from target"
    );

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

    assert!(
        env.target_ref_exists("refs/tags/v1.0"),
        "tag v1.0 missing from target"
    );

    // The tag tip should differ from the main tip (it points to the first commit).
    let tag_sha = env.target_ref_sha("refs/tags/v1.0").unwrap();
    let main_sha = env.target_ref_sha("refs/heads/main").unwrap();
    assert_ne!(
        tag_sha, main_sha,
        "v1.0 tag should point to first commit, not tip"
    );
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
    let target_merge_sha = env
        .target_ref_sha(&mapping_ref)
        .unwrap_or_else(|| panic!("merge mapping ref missing: {mapping_ref}"));

    assert_eq!(
        env.target_parent_count(&target_merge_sha),
        2,
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
    let base_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_base}"))
        .unwrap();
    let skip_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_skip}"))
        .unwrap();
    assert_eq!(
        base_target, skip_target,
        "skipped commit should map to parent's target SHA"
    );

    // The commit after the skip has a non-skip mapping.
    let after_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_after}"))
        .unwrap();
    assert_ne!(
        after_target, skip_target,
        "commit after skip should have its own target SHA"
    );

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

    let target1 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha1}"))
        .unwrap();
    let target2 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha2}"))
        .unwrap();
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

    let target_sha = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha}"))
        .unwrap();
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

    let base_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_base}"))
        .unwrap();
    let fail_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_fail}"))
        .unwrap();

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
    assert!(
        files.contains(&"keep.txt".to_string()),
        "keep.txt missing: {files:?}"
    );
    assert!(
        !files.contains(&"skip.txt".to_string()),
        "skip.txt should not be in target: {files:?}"
    );
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

// ── git xf status tests ───────────────────────────────────────────────────────

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

// ── git xf hook tests ─────────────────────────────────────────────────────────

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

// ── rule: shell, output paths, BYOT ──────────────────────────────────────────

/// Explicit `shell: sh` behaves identically to the default.
#[test]
fn test_rule_explicit_sh_shell() {
    let config = "test:\n  target: ../target.git\n  rule:\n    shell: sh\n    command: \"true\"\n";
    let env = Env::new(config);
    env.commit("first", &[("a.txt", "hello")]);
    env.sync(&[]);
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `output` as a list of `src:dst` pairs copies into the specified target paths.
#[test]
fn test_rule_output_src_dst() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'true'
             output:
               - src/data.txt:dst/data.txt
        ",
    );
    let env = Env::new(&config);
    env.commit("add file", &[("src/data.txt", "payload")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let files = env.target_tree_files(&tip);
    // Target root should contain dst/, not src/.
    assert!(
        files.contains(&"dst".to_string()),
        "dst/ missing: {files:?}"
    );
    assert!(
        !files.contains(&"src".to_string()),
        "src/ should not be in target"
    );

    let content = env.target_file_content(&tip, "dst/data.txt");
    assert_eq!(content.trim(), "payload");
}

/// `output` as a `{src: dst}` map is equivalent to the list form.
#[test]
fn test_rule_output_map_format() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'true'
             output:
               src/: out/
        ",
    );
    let env = Env::new(&config);
    env.commit("add file", &[("src/file.txt", "content")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let content = env.target_file_content(&tip, "out/file.txt");
    assert_eq!(content.trim(), "content");
}

/// Build-your-own-target mode: `command` populates `$TARGET_PATH`; the result
/// becomes the target commit tree, independent of what `output` would have done.
#[test]
fn test_rule_byot() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'cp src.txt \"$XF_TARGET\"/result.txt'
             targetEnv: XF_TARGET
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("src.txt", "byot-payload")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let files = env.target_tree_files(&tip);
    assert!(
        files.contains(&"result.txt".to_string()),
        "result.txt missing from target: {files:?}"
    );
    // src.txt should NOT be in the target — only what the command placed in $XF_TARGET.
    assert!(
        !files.contains(&"src.txt".to_string()),
        "src.txt should not be in target"
    );
    let content = env.target_file_content(&tip, "result.txt");
    assert_eq!(content.trim(), "byot-payload");
}

/// Using both `output` (non-empty) and `targetEnv` in the same rule is a config error.
#[test]
fn test_rule_output_and_byot_conflict() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'true'
             output: [some/path]
             targetEnv: XF_TARGET
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("a.txt", "a")]);
    // sync should fail at config validation, not crash
    let out = Command::new(BIN)
        .current_dir(&env.source)
        .arg("sync")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "sync should have failed with a config error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("targetEnv") || stderr.contains("output"),
        "error message should mention the conflicting fields: {stderr}"
    );
}

/// An explicit empty `output: []` combined with `targetEnv` is also a config error.
#[test]
fn test_rule_output_empty_list_and_byot_conflict() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'true'
             output: []
             targetEnv: XF_TARGET
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("a.txt", "a")]);
    let out = Command::new(BIN)
        .current_dir(&env.source)
        .arg("sync")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "sync should have failed with a config error for output: [] + targetEnv"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("targetEnv") || stderr.contains("output"),
        "error message should mention the conflicting fields: {stderr}"
    );
}

/// A non-sh shell (bash) is invoked via /usr/bin/env and can use bash-specific syntax.
#[test]
fn test_rule_shell_bash() {
    // bash-specific: process substitution to write output
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             shell: bash
             command: 'printf \"%s\" \"$(echo hello)\" > out.txt'
             output: [out.txt]
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("dummy.txt", "x")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let content = env.target_file_content(&tip, "out.txt");
    assert_eq!(content.trim(), "hello");
}

/// An absolute source path in `output` is used as-is (out-of-tree build dir).
#[test]
fn test_rule_output_absolute_src_path() {
    // The command writes to /tmp; the output entry uses an absolute source path.
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'printf \"from-abs\" > /tmp/git-xf-test-abs-output.txt'
             output:
               - /tmp/git-xf-test-abs-output.txt:result.txt
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("dummy.txt", "x")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let content = env.target_file_content(&tip, "result.txt");
    assert_eq!(content.trim(), "from-abs");
}

/// A `.git` file/directory placed inside the BYOT staging dir is not copied
/// into the target worktree and does not corrupt the target cache.
#[test]
fn test_rule_byot_git_entry_not_copied() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: |
               printf \"data\" > \"$XF_TARGET/real.txt\"
               printf \"should-not-appear\" > \"$XF_TARGET/.git\"
             targetEnv: XF_TARGET
        ",
    );
    let env = Env::new(&config);
    env.commit("first", &[("src.txt", "x")]);
    env.sync(&[]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let files = env.target_tree_files(&tip);
    assert!(
        files.contains(&"real.txt".to_string()),
        "real.txt missing: {files:?}"
    );
    assert!(
        !files.contains(&".git".to_string()),
        ".git entry must not appear in the target commit: {files:?}"
    );
    let content = env.target_file_content(&tip, "real.txt");
    assert_eq!(content.trim(), "data");
}

// ── --rule: per-commit rule reading ──────────────────────────────────────────
//
// Key setup pattern for these tests:
//   1. env.commit() always stages the on-disk .git-xf.yaml via `git add .`.
//   2. To test per-commit vs HEAD rule differences:
//      - Include the desired per-commit .git-xf.yaml in the commit files list,
//        then re-write the HEAD config to disk (without committing) before sync.
//   3. To test "missing .git-xf.yaml" in a commit:
//      - After the base commit, use `git rm .git-xf.yaml` + commit to create a
//        commit whose tree has no config, then re-write the config to disk.
//   4. For a root commit without .git-xf.yaml, use commit_orphan().

/// `--rule=head` always uses HEAD's on-disk rule; per-commit .git-xf.yaml
/// differences are ignored.
#[test]
fn test_rule_source_head() {
    // HEAD (disk) rule: output: [] — copies nothing.
    // Per-commit rule: output: [a.txt] — would copy a.txt.
    // With --rule=head, a.txt should NOT appear in the target.
    let head_config =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output: []\n";
    let env = Env::new(head_config);

    let per_commit =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output:\n      - a.txt\n";
    env.commit("first", &[(".git-xf.yaml", per_commit), ("a.txt", "hello")]);

    // Restore HEAD config on disk without committing — config::load will see output: [].
    fs::write(env.source.join(".git-xf.yaml"), head_config).unwrap();
    env.sync(&["--rule=head"]);

    let tip = env.target_ref_sha("refs/heads/main").unwrap();
    let files = env.target_tree_files(&tip);
    assert!(
        !files.contains(&"a.txt".to_string()),
        "--rule=head should use HEAD's empty output, not the per-commit rule: {files:?}"
    );
}

/// `--rule=commit` uses each source commit's own rule, ignoring HEAD's.
#[test]
fn test_rule_source_commit_uses_per_commit_rule() {
    // HEAD (disk) rule: output: [] — copies nothing.
    // Per-commit rule: output: [a.txt].
    // With --rule=commit, a.txt SHOULD appear in the target.
    let head_config =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output: []\n";
    let env = Env::new(head_config);

    let per_commit =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output:\n      - a.txt\n";
    let sha1 = env.commit("first", &[(".git-xf.yaml", per_commit), ("a.txt", "hello")]);

    fs::write(env.source.join(".git-xf.yaml"), head_config).unwrap();
    env.sync(&["--rule=commit"]);

    let target1 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha1}"))
        .unwrap();
    let files = env.target_tree_files(&target1);
    assert!(
        files.contains(&"a.txt".to_string()),
        "--rule=commit should use per-commit rule (output: [a.txt]): {files:?}"
    );
}

/// No `--rule` flag defaults to `--rule=commit`.
#[test]
fn test_rule_source_commit_is_default() {
    let head_config =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output: []\n";
    let env = Env::new(head_config);

    let per_commit =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n    output:\n      - a.txt\n";
    let sha1 = env.commit("first", &[(".git-xf.yaml", per_commit), ("a.txt", "hi")]);

    fs::write(env.source.join(".git-xf.yaml"), head_config).unwrap();
    env.sync(&[]); // no --rule flag → default is commit

    let target1 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha1}"))
        .unwrap();
    let files = env.target_tree_files(&target1);
    assert!(
        files.contains(&"a.txt".to_string()),
        "default mode should be --rule=commit: {files:?}"
    );
}

/// `missing: error` (the default) — sync fails when the per-commit .git-xf.yaml
/// is absent from a commit's tree.
#[test]
fn test_rule_source_commit_missing_file_error() {
    let config = "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n"; // missing: error by default
    let env = Env::new(config);

    // Base commit: includes .git-xf.yaml (unavoidable with git add .).
    env.commit("base", &[("a.txt", "a")]);

    // Remove .git-xf.yaml from git so the next commit has no config.
    git(&env.source, &["rm", ".git-xf.yaml"]);
    git(&env.source, &["commit", "-m", "remove config"]);
    let no_config_sha = git_read(&env.source, &["rev-parse", "HEAD"]);

    // Re-write config to disk so config::load still works.
    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .args(["sync", "--rule=commit"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "sync should fail with missing: error (default)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("missing rule") || stderr.contains("missing-rule"),
        "error should mention missing rule: {stderr}"
    );
    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{no_config_sha}")),
        "failed commit should have no mapping ref"
    );
}

/// `missing: error` — sync fails when the transformation block is absent from
/// the per-commit .git-xf.yaml.
#[test]
fn test_rule_source_commit_missing_block_error() {
    let config = "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n";
    let env = Env::new(config);

    env.commit("base", &[("a.txt", "a")]);

    // Commit a .git-xf.yaml with a DIFFERENT transformation name — no 'test' block.
    let wrong = "other:\n  target: ../target.git\n  rule:\n    command: 'true'\n";
    env.commit("wrong block", &[(".git-xf.yaml", wrong), ("b.txt", "b")]);

    // Restore correct config on disk.
    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .args(["sync", "--rule=commit"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "sync should fail when transformation block is absent from per-commit config"
    );
}

/// `missing: skip` — a commit without a per-commit rule is mapped to its
/// first mapped direct parent.
#[test]
fn test_rule_source_commit_missing_skip() {
    let config = "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n  missing: skip\n";
    let env = Env::new(config);

    let sha_base = env.commit("base", &[("a.txt", "a")]);

    git(&env.source, &["rm", ".git-xf.yaml"]);
    git(&env.source, &["commit", "-m", "no config"]);
    let sha_skip = git_read(&env.source, &["rev-parse", "HEAD"]);

    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();
    env.sync(&["--rule=commit"]);

    let base_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_base}"))
        .unwrap();
    let skip_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_skip}"))
        .unwrap();
    assert_eq!(
        base_target, skip_target,
        "missing:skip commit should map to parent's target SHA"
    );
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `missing: empty-commit` — creates a target commit with a `[git-xf missing-rule]`
/// marker in the message.
#[test]
fn test_rule_source_commit_missing_empty_commit() {
    let config =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n  missing: empty-commit\n";
    let env = Env::new(config);

    env.commit("base", &[("a.txt", "a")]);

    git(&env.source, &["rm", ".git-xf.yaml"]);
    git(&env.source, &["commit", "-m", "no config"]);
    let sha_no_config = git_read(&env.source, &["rev-parse", "HEAD"]);

    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();
    env.sync(&["--rule=commit"]);

    let target_sha = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_no_config}"))
        .expect("missing:empty-commit should produce a mapping ref");
    let msg = env.target_commit_message(&target_sha);
    assert!(
        msg.contains("[git-xf missing-rule]"),
        "commit message should contain [git-xf missing-rule] marker: {msg}"
    );
}

/// `missing: skip` on a root commit (orphan, no parents) drops the commit —
/// same root-drop semantics as `ignore-error: skip`.
#[test]
fn test_rule_source_commit_missing_skip_root_dropped() {
    let config = "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n  missing: skip\n";
    let env = Env::new(config);

    // Orphan root commit with no .git-xf.yaml in its tree.
    let root_sha = env.commit_orphan("orphan", "root no config", &[("a.txt", "a")]);

    // Restore config to disk after commit_orphan's git clean.
    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();
    env.sync(&["--rule=commit"]);

    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{root_sha}")),
        "root commit dropped due to missing rule should have no mapping ref"
    );
    assert_eq!(env.target_commit_count("refs/heads/orphan"), 0);
}

/// A malformed per-commit .git-xf.yaml triggers the `missing` policy.
#[test]
fn test_rule_source_commit_parse_error() {
    let config =
        "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n  missing: empty-commit\n";
    let env = Env::new(config);

    env.commit("base", &[("a.txt", "a")]);

    // Commit a malformed .git-xf.yaml (YAML parse error).
    let bad_yaml = "{ invalid: [unclosed";
    env.commit("bad yaml", &[(".git-xf.yaml", bad_yaml), ("b.txt", "b")]);
    let sha_bad = git_read(&env.source, &["rev-parse", "HEAD"]);

    // Restore valid config to disk.
    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();
    env.sync(&["--rule=commit"]);

    let target_sha = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha_bad}"))
        .expect("parse error + missing:empty-commit should produce a mapping ref");
    let msg = env.target_commit_message(&target_sha);
    assert!(
        msg.contains("[git-xf missing-rule]"),
        "YAML parse error should produce [git-xf missing-rule] marker: {msg}"
    );
}

/// `--rule=head` ignores the `missing` policy entirely — the per-commit config
/// is never read, so a missing file does not trigger any policy.
#[test]
fn test_rule_source_head_ignores_missing_policy() {
    let config = "test:\n  target: ../target.git\n  rule:\n    command: 'true'\n  missing: error\n";
    let env = Env::new(config);

    env.commit("base", &[("a.txt", "a")]);

    git(&env.source, &["rm", ".git-xf.yaml"]);
    git(&env.source, &["commit", "-m", "remove config"]);

    // Re-write config to disk — needed for config::load.
    fs::write(env.source.join(".git-xf.yaml"), config).unwrap();

    // --rule=head should succeed even though the commit has no .git-xf.yaml,
    // because the per-commit config is never consulted.
    env.sync(&["--rule=head"]);
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);
}

// ── skip on root commit ───────────────────────────────────────────────────────

/// `ignore-error: skip` on the very first (root) commit drops it entirely —
/// no mapping ref is created and the sync still succeeds.
#[test]
fn test_ignore_error_skip_root_dropped() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'exit 1'
           ignore-error: skip
        ",
    );
    let env = Env::new(&config);
    let root_sha = env.commit("root fails", &[("a.txt", "a")]);
    env.sync(&[]);

    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{root_sha}")),
        "dropped root should have no mapping ref"
    );
    assert_eq!(env.target_commit_count("refs/heads/main"), 0);
}

/// The child of a dropped root has no target parents — it becomes the target root.
#[test]
fn test_ignore_error_skip_root_child_becomes_target_root() {
    // Command succeeds only when ok.txt exists. ROOT has no ok.txt (fails → dropped);
    // CHILD adds ok.txt so the tree contains it (succeeds).
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: '[ -f ok.txt ]'
           ignore-error: skip
        ",
    );
    let env = Env::new(&config);
    let root_sha = env.commit("root will fail", &[("other.txt", "x")]);
    let child_sha = env.commit("child succeeds", &[("ok.txt", "good")]);
    env.sync(&[]);

    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{root_sha}")),
        "dropped root should have no mapping ref"
    );

    let child_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{child_sha}"))
        .expect("child should have a mapping ref");
    assert_eq!(
        env.target_parent_count(&child_target),
        0,
        "child of dropped root should be a target root (0 parents)"
    );
    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
}

/// `skip-commit-messages` applied to the root commit drops it — the next commit
/// becomes the target root.
#[test]
fn test_skip_commit_messages_root_dropped() {
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: 'true'
           skip-commit-messages:
             - '[drop-me]'
        ",
    );
    let env = Env::new(&config);
    let root_sha = env.commit("init [drop-me]", &[("a.txt", "a")]);
    let child_sha = env.commit("real first commit", &[("b.txt", "b")]);
    env.sync(&[]);

    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{root_sha}")),
        "skipped root should have no mapping ref"
    );

    let child_target = env
        .target_ref_sha(&format!("refs/git-xf/test/{child_sha}"))
        .expect("child should have a mapping ref");
    assert_eq!(
        env.target_parent_count(&child_target),
        0,
        "child of skipped root should be a target root (0 parents)"
    );
}

/// A merge commit whose one parent was a dropped root gets one fewer parent in
/// the target — the dropped parent is silently omitted from the parent list.
#[test]
fn test_ignore_error_skip_merge_drops_failed_root_parent() {
    // Command succeeds if ok.txt exists; fails otherwise.
    let config = indoc(
        "test:
           target: ../target.git
           rule:
             command: '[ -f ok.txt ]'
           ignore-error: skip
        ",
    );
    let env = Env::new(&config);

    // Root on main (ok.txt present → succeeds).
    let root_main = env.commit("main root", &[("ok.txt", "yes")]);

    // Orphan root on "side" (no ok.txt → fails → dropped).
    let root_side = env.commit_orphan("side", "side root", &[("other.txt", "no")]);

    // Merge the unrelated side branch into main.
    env.checkout("main");
    let merge_sha = env.merge_unrelated("side", "merge unrelated histories");

    env.sync(&["--rule=head"]);

    // side root has no mapping (it was dropped).
    assert!(
        !env.target_ref_exists(&format!("refs/git-xf/test/{root_side}")),
        "dropped side root should have no mapping ref"
    );

    // main root is mapped.
    let _target_root_main = env
        .target_ref_sha(&format!("refs/git-xf/test/{root_main}"))
        .expect("main root should be mapped");

    // Merge commit is mapped and has only 1 parent (the dropped root was excluded).
    let target_merge = env
        .target_ref_sha(&format!("refs/git-xf/test/{merge_sha}"))
        .expect("merge commit should be mapped");
    assert_eq!(
        env.target_parent_count(&target_merge),
        1,
        "merge with one dropped parent should have 1 parent in target"
    );
}

// ── --all-branches ────────────────────────────────────────────────────────────

/// `--all-branches` syncs every branch; shared commits are transformed once.
#[test]
fn test_all_branches() {
    let env = Env::new(passthrough_config());
    // "base" is shared between main and feat.
    env.commit("base", &[("a.txt", "a")]);
    env.create_branch("feat");
    env.commit("feat commit", &[("b.txt", "b")]);
    env.checkout("main");
    env.commit("main commit", &[("c.txt", "c")]);

    env.sync(&["--all-branches"]);

    assert!(
        env.target_ref_exists("refs/heads/main"),
        "main missing from target"
    );
    assert!(
        env.target_ref_exists("refs/heads/feat"),
        "feat missing from target"
    );
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);
    assert_eq!(env.target_commit_count("refs/heads/feat"), 2);
}

/// Passing both `--all-branches` and an explicit REF is a clap conflict error.
#[test]
fn test_all_branches_conflicts_with_refs() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .args(["sync", "--all-branches", "HEAD"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--all-branches + explicit REF should error"
    );
}

// ── --depth ───────────────────────────────────────────────────────────────────

/// `--depth=2` on a 5-commit chain transforms only the 2 closest commits;
/// the boundary commit becomes a synthetic root in the target.
#[test]
fn test_depth_limits_commits() {
    let env = Env::new(passthrough_config());
    env.commit("c1", &[("a.txt", "1")]);
    env.commit("c2", &[("b.txt", "2")]);
    env.commit("c3", &[("c.txt", "3")]);
    let sha4 = env.commit("c4", &[("d.txt", "4")]);
    let sha5 = env.commit("c5", &[("e.txt", "5")]);

    env.sync(&["--depth=2"]);

    // Only the 2 closest commits (c4 dist=1, c5 dist=0) are transformed.
    assert_eq!(env.target_commit_count("refs/heads/main"), 2);

    let target5 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha5}"))
        .expect("c5 should be mapped");
    let target4 = env
        .target_ref_sha(&format!("refs/git-xf/test/{sha4}"))
        .expect("c4 should be mapped");

    // c5 has c4 as its single parent in the target.
    assert_eq!(env.target_parent_count(&target5), 1);
    // c4 is a synthetic root — its parent (c3) was not transformed.
    assert_eq!(env.target_parent_count(&target4), 0);
}

/// BFS stops at already-mapped commits before reaching the depth limit.
#[test]
fn test_depth_with_already_mapped() {
    let env = Env::new(passthrough_config());
    env.commit("c1", &[("a.txt", "1")]);
    env.commit("c2", &[("b.txt", "2")]);
    env.commit("c3", &[("c.txt", "3")]);

    // Full sync first.
    env.sync(&[]);
    assert_eq!(env.target_commit_count("refs/heads/main"), 3);

    // Add a new commit and sync with a depth large enough that c1-c3 would be
    // in range, but the BFS should stop at c3 (already mapped).
    let sha4 = env.commit("c4", &[("d.txt", "4")]);
    env.sync(&["--depth=10"]);

    // Only c4 is new; c1-c3 were not re-transformed.
    assert_eq!(env.target_commit_count("refs/heads/main"), 4);
    assert!(
        env.target_ref_exists(&format!("refs/git-xf/test/{sha4}")),
        "c4 should be mapped"
    );
}

/// `--depth=1 --all-branches` applies the depth limit per branch tip.
#[test]
fn test_depth_all_branches() {
    let env = Env::new(passthrough_config());
    env.commit("shared", &[("a.txt", "a")]);
    env.create_branch("feat");
    env.commit("feat-only", &[("b.txt", "b")]);
    env.checkout("main");
    env.commit("main-only", &[("c.txt", "c")]);

    // depth=1: only the tip of each branch (dist=0) is transformed.
    // "shared" is at dist=1 from each tip → depth cutoff, not transformed.
    env.sync(&["--depth=1", "--all-branches"]);

    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
    assert_eq!(env.target_commit_count("refs/heads/feat"), 1);
}

/// `--depth=0` is rejected at argument parse time.
#[test]
fn test_depth_zero_rejected() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .args(["sync", "--depth=0"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "--depth=0 should be rejected");
}

// ── --push-chunk ──────────────────────────────────────────────────────────────

/// `--push-chunk=2` on 5 linear commits produces 3 mapping-ref pushes + 1 branch-ref push.
#[test]
fn test_push_chunk_count() {
    let env = Env::new(passthrough_config());
    env.install_post_receive_counter();

    env.commit("c1", &[("a.txt", "1")]);
    env.commit("c2", &[("b.txt", "2")]);
    env.commit("c3", &[("c.txt", "3")]);
    env.commit("c4", &[("d.txt", "4")]);
    env.commit("c5", &[("e.txt", "5")]);

    env.sync(&["--push-chunk=2"]);

    assert_eq!(env.target_commit_count("refs/heads/main"), 5);
    // chunks [c1,c2], [c3,c4], [c5] → 3 mapping pushes + 1 branch push = 4
    assert_eq!(env.push_log_count(), 4);
}

/// `--push-chunk=<1B>` forces each commit into its own push round.
#[test]
fn test_push_chunk_size() {
    let env = Env::new(passthrough_config());
    env.install_post_receive_counter();

    env.commit("c1", &[("a.txt", &"a".repeat(500))]);
    env.commit("c2", &[("b.txt", &"b".repeat(500))]);
    env.commit("c3", &[("c.txt", &"c".repeat(500))]);

    // 1-byte limit forces each commit into its own chunk (best-effort single commit).
    env.sync(&["--push-chunk=1B"]);

    assert_eq!(env.target_commit_count("refs/heads/main"), 3);
    // 3 mapping pushes + 1 branch push = 4
    assert_eq!(env.push_log_count(), 4);
}

/// A single commit whose objects exceed the size limit is pushed as its own chunk.
#[test]
fn test_push_chunk_single_oversized() {
    let env = Env::new(passthrough_config());
    env.install_post_receive_counter();

    env.commit("large", &[("big.txt", &"x".repeat(5_000))]);

    env.sync(&["--push-chunk=1B"]);

    assert_eq!(env.target_commit_count("refs/heads/main"), 1);
    // 1 mapping push + 1 branch push = 2
    assert_eq!(env.push_log_count(), 2);
}

/// `--push-chunk=0` disables chunking: exactly 1 mapping-ref push + 1 branch-ref push.
#[test]
fn test_push_chunk_zero() {
    let env = Env::new(passthrough_config());
    env.install_post_receive_counter();

    env.commit("c1", &[("a.txt", "1")]);
    env.commit("c2", &[("b.txt", "2")]);
    env.commit("c3", &[("c.txt", "3")]);

    env.sync(&["--push-chunk=0"]);

    assert_eq!(env.target_commit_count("refs/heads/main"), 3);
    // single mapping push + 1 branch push = 2
    assert_eq!(env.push_log_count(), 2);
}

/// After a chunked sync, a second sync finds everything already mapped (no-op).
#[test]
fn test_push_chunk_resume() {
    let env = Env::new(passthrough_config());

    env.commit("c1", &[("a.txt", "1")]);
    env.commit("c2", &[("b.txt", "2")]);
    env.commit("c3", &[("c.txt", "3")]);

    env.sync(&["--push-chunk=1"]);
    assert_eq!(env.target_commit_count("refs/heads/main"), 3);

    // Second sync: all commits already mapped → no new transforms.
    env.sync(&["--push-chunk=1"]);
    assert_eq!(env.target_commit_count("refs/heads/main"), 3);
}
