// Integration tests: git xf diff.

mod common;
use common::*;

use std::fs;

// ── git xf diff tests ─────────────────────────────────────────────────────────

/// Two-commit form: `diff A B` shows changes between two mapped commits.
#[test]
fn test_diff_two_commit_form() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    let sha3 = env.commit("third", &[("c.txt", "ccc")]);
    env.sync(&[]);

    let out = env.run_diff(&[&sha1, &sha3]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("b.txt") && stdout.contains("c.txt"),
        "expected diff output: {stdout}"
    );
}

/// Two-dot form: `diff A..B` produces the same output as `diff A B`.
#[test]
fn test_diff_two_dot_form() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha3 = env.commit("third", &[("c.txt", "ccc")]);
    env.sync(&[]);

    let range = format!("{sha1}..{sha3}");
    let out = env.run_diff(&[&range]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("c.txt"), "expected diff output: {stdout}");
}

/// Three-dot form: `diff main...feat` shows only commits unique to feat.
#[test]
fn test_diff_three_dot_form() {
    let env = Env::new(passthrough_config());
    env.commit("base", &[("base.txt", "base")]);
    env.create_branch("feat");
    env.commit("feat-only", &[("feat.txt", "feat")]);
    env.checkout("main");
    env.commit("main-only", &[("main.txt", "main")]);
    env.sync(&["--all-branches"]);

    let out = env.run_diff(&["main...feat"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feat.txt"),
        "expected feat.txt in diff: {stdout}"
    );
    assert!(
        !stdout.contains("main.txt"),
        "main.txt should not appear: {stdout}"
    );
}

/// Single-commit form: `diff <sha>` diffs that commit against HEAD.
#[test]
fn test_diff_single_commit_form() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&[&sha1]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("b.txt"), "expected b.txt in diff: {stdout}");
}

/// `--name-only` is forwarded and suppresses hunk output.
#[test]
fn test_diff_option_forwarded() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&["--name-only", &sha1, &sha2]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("b.txt"), "expected b.txt: {stdout}");
    assert!(
        !stdout.contains("@@"),
        "should not have hunk headers: {stdout}"
    );
}

/// Path filter `-- a.txt` limits output to a.txt only.
#[test]
fn test_diff_path_filter_forwarded() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("a.txt", "aaa2"), ("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&[&sha1, &sha2, "--", "a.txt"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("a.txt"), "expected a.txt: {stdout}");
    assert!(
        !stdout.contains("b.txt"),
        "b.txt should be filtered out: {stdout}"
    );
}

/// `-x <name>` selects the transformation when multiple are configured.
#[test]
fn test_diff_x_selects_transform() {
    let env = Env::new(two_transform_config());
    // Create the second bare target.
    let target2 = env.source.parent().unwrap().join("target2.git");
    bare_init(&target2);

    let sha1 = env.commit("first", &[("a.txt", "v1")]);
    let sha2 = env.commit("second", &[("a.txt", "v2")]);
    env.sync(&["--all-branches"]);

    // diff in xform2's target
    let out = env.run_diff(&["-x", "xform2", &sha1, &sha2]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("a.txt"),
        "expected a.txt in xform2 diff: {stdout}"
    );
}

/// Single transformation with no `-x` works without error.
#[test]
fn test_diff_single_transform_no_x() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&[&sha1, &sha2]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── error-case tests ──────────────────────────────────────────────────────────

/// One commit unmapped: error mentions "no mapping" and "git xf sync".
#[test]
fn test_diff_err_one_unmapped() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    // sha2 is not synced

    let out = env.run_diff(&[&sha1, &sha2]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no mapping"),
        "expected 'no mapping': {stderr}"
    );
    assert!(
        stderr.contains("git xf sync"),
        "expected 'git xf sync': {stderr}"
    );
}

/// Both commits unmapped: error mentions the first unmapped SHA.
#[test]
fn test_diff_err_both_unmapped() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    // no sync

    let out = env.run_diff(&[&sha1, &sha2]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no mapping"),
        "expected 'no mapping': {stderr}"
    );
}

/// All non-flag tokens fail rev-parse: error mentions "no revision".
#[test]
fn test_diff_err_no_revision_args() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);

    let out = env.run_diff(&["--name-only"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no revision"),
        "expected 'no revision': {stderr}"
    );
}

/// `-x` with no name: error mentions "missing argument for -x".
#[test]
fn test_diff_err_x_no_name() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&["-x"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("missing argument for -x"),
        "expected error: {stderr}"
    );
}

/// Multiple transformations with no `-x`: error mentions "use -x".
#[test]
fn test_diff_err_multiple_transforms_no_x() {
    let env = Env::new(two_transform_config());
    let target2 = env.source.parent().unwrap().join("target2.git");
    bare_init(&target2);

    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&["--all-branches"]);

    let out = env.run_diff(&[&sha1, &sha2]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("use -x"), "expected 'use -x': {stderr}");
}

/// Unknown revision: the bad ref is forwarded to git diff which then errors.
///
/// Note: `nonexistent-ref` fails rev-parse and is silently skipped as a
/// non-revision token (the token-walking design). `HEAD` is the only RevToken,
/// so we enter single-commit form. The reconstructed argv fed to `git diff` in
/// the target is `["nonexistent-ref", <mapped-sha>, <mapped-sha>]`, which git
/// rejects — producing an error that happens to mention `"nonexistent-ref"`.
#[test]
fn test_diff_err_unknown_revision() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);

    let out = env.run_diff(&["nonexistent-ref", "HEAD"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nonexistent-ref") || stderr.contains("no revision"),
        "expected unknown ref error: {stderr}"
    );
}

/// Single-commit form with staged changes: error mentions "not clean".
#[test]
fn test_diff_err_single_commit_staged() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    // Stage a change.
    fs::write(env.source.join("new.txt"), "new").unwrap();
    git(&env.source, &["add", "new.txt"]);

    let out = env.run_diff(&[&sha1]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not clean"),
        "expected 'not clean': {stderr}"
    );
}

/// Single-commit form with unstaged changes: error mentions "not clean".
#[test]
fn test_diff_err_single_commit_unstaged() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    // Modify a tracked file without staging.
    fs::write(env.source.join("a.txt"), "modified").unwrap();

    let out = env.run_diff(&[&sha1]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not clean"),
        "expected 'not clean': {stderr}"
    );
}

/// Single-commit form, HEAD unmapped: error mentions "no mapping".
#[test]
fn test_diff_err_single_commit_head_unmapped() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    // Add a commit but do not sync it; HEAD is now unmapped.
    env.commit("second", &[("b.txt", "bbb")]);

    let out = env.run_diff(&[&sha1]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no mapping"),
        "expected 'no mapping': {stderr}"
    );
}
