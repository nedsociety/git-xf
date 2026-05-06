// Integration tests: git xf pr.

mod common;
use common::*;

// ── git xf pr tests ───────────────────────────────────────────────────────────

/// SSH remote, branch only: URL uses single-branch compare form.
#[test]
fn test_pr_ssh_branch_only() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "git@github.com:org/repo.git");

    let out = env.run_pr(&["main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com/org/repo/compare/main"),
        "unexpected URL: {stdout}"
    );
}

/// SSH remote, branch + base: URL uses three-dot compare form.
#[test]
fn test_pr_ssh_branch_and_base() {
    let env = Env::new(passthrough_config());
    env.commit("base-commit", &[("a.txt", "aaa")]);
    env.create_branch("feat");
    env.commit("feat-commit", &[("b.txt", "bbb")]);
    env.sync(&["--all-branches"]);
    set_cache_remote_url(&env, "test", "git@github.com:org/repo.git");

    let out = env.run_pr(&["feat", "main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com/org/repo/compare/main...feat"),
        "unexpected URL: {stdout}"
    );
}

/// HTTPS remote with .git suffix.
#[test]
fn test_pr_https_with_git_suffix() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "https://github.com/org/repo.git");

    let out = env.run_pr(&["main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com/org/repo/compare/main"),
        "unexpected URL: {stdout}"
    );
}

/// HTTPS remote without .git suffix.
#[test]
fn test_pr_https_no_git_suffix() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "https://github.com/org/repo");

    let out = env.run_pr(&["main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com/org/repo/compare/main"),
        "unexpected URL: {stdout}"
    );
    // Repo name must not contain double ".git.git".
    assert!(
        !stdout.contains(".git"),
        "URL must not contain .git: {stdout}"
    );
}

/// HTTPS remote with embedded credentials (e.g. GitHub token via git config).
#[test]
fn test_pr_https_with_credentials() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(
        &env,
        "test",
        "https://x-access-token:ghp_abc123@github.com/org/repo.git",
    );

    let out = env.run_pr(&["main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com/org/repo/compare/main"),
        "unexpected URL: {stdout}"
    );
    assert!(
        !stdout.contains("ghp_abc123"),
        "credentials must not appear in URL: {stdout}"
    );
}

/// `-x` selects the correct transformation.
#[test]
fn test_pr_x_selects_transform() {
    let env = Env::new(two_transform_config());
    let target2 = env.source.parent().unwrap().join("target2.git");
    bare_init(&target2);

    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&["--all-branches"]);

    set_cache_remote_url(&env, "xform1", "git@github.com:org/repo1.git");
    set_cache_remote_url(&env, "xform2", "git@github.com:org/repo2.git");

    let out = env.run_pr(&["-x", "xform2", "main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("org/repo2/compare/main"),
        "expected repo2 in URL: {stdout}"
    );
    assert!(
        !stdout.contains("repo1"),
        "should not reference repo1: {stdout}"
    );
}

/// `--transform` long form works the same as `-x`.
#[test]
fn test_pr_long_transform_flag() {
    let env = Env::new(two_transform_config());
    let target2 = env.source.parent().unwrap().join("target2.git");
    bare_init(&target2);

    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&["--all-branches"]);

    set_cache_remote_url(&env, "xform2", "git@github.com:org/repo2.git");

    let out = env.run_pr(&["--transform", "xform2", "main"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("org/repo2/compare/main"),
        "unexpected URL: {stdout}"
    );
}

/// Branch not in target: error mentions "not found".
#[test]
fn test_pr_err_branch_not_found() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "git@github.com:org/repo.git");

    let out = env.run_pr(&["nonexistent"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found"),
        "expected 'not found': {stderr}"
    );
}

/// Base not in target: error mentions "not found".
#[test]
fn test_pr_err_base_not_found() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "git@github.com:org/repo.git");

    let out = env.run_pr(&["main", "nonexistent-base"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found"),
        "expected 'not found': {stderr}"
    );
}

/// Non-GitHub remote: error mentions "not a GitHub".
#[test]
fn test_pr_err_non_github_remote() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&[]);
    set_cache_remote_url(&env, "test", "git@gitlab.com:org/repo.git");

    let out = env.run_pr(&["main"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a GitHub"),
        "expected 'not a GitHub': {stderr}"
    );
}

/// Multiple transformations with no `-x`: error mentions "use -x".
#[test]
fn test_pr_err_multiple_transforms_no_x() {
    let env = Env::new(two_transform_config());
    let target2 = env.source.parent().unwrap().join("target2.git");
    bare_init(&target2);

    env.commit("first", &[("a.txt", "aaa")]);
    env.sync(&["--all-branches"]);

    let out = env.run_pr(&["main"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("use -x"), "expected 'use -x': {stderr}");
}
