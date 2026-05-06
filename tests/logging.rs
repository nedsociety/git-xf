// Integration tests: --loglevel / -v / -vv / LOGLEVEL.

mod common;
use common::*;

use std::process::Command;

fn streaming_config() -> &'static str {
    // Multi-line stdout helps verify line-by-line streaming at TRACE.
    "test:\n  target: ../target.git\n  rule:\n    command: \"printf 'line1\\nline2\\nline3\\n'\"\n"
}

fn run_sync(env: &Env, leading: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(&env.source);
    cmd.env_remove("LOGLEVEL");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    for arg in leading {
        cmd.arg(arg);
    }
    cmd.arg("sync");
    cmd.output().unwrap()
}

// ── 1. -v shows DEBUG ─────────────────────────────────────────────────────────

#[test]
fn test_v_flag_enables_debug() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &["-v"], &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cache: fetching"),
        "expected 'cache: fetching' in stderr: {stderr}"
    );
    assert!(
        stderr.contains("transform["),
        "expected 'transform[' in stderr: {stderr}"
    );
}

// ── 2. -vv enables TRACE and streams rule output ──────────────────────────────

#[test]
fn test_vv_flag_enables_trace_streaming() {
    let env = Env::new(streaming_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &["-vv"], &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rule[") && stderr.contains(": command:"),
        "expected 'rule[...]: command:' line: {stderr}"
    );
    // Streaming should emit a stdout line per line of rule output.
    assert!(
        stderr.contains("stdout: line1") && stderr.contains("stdout: line3"),
        "expected per-line streamed stdout: {stderr}"
    );
}

// ── 3. LOGLEVEL=debug envvar enables DEBUG ────────────────────────────────────

#[test]
fn test_loglevel_env_enables_debug() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &[], &[("LOGLEVEL", "debug")]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cache: fetching") && stderr.contains("transform["),
        "expected DEBUG output via LOGLEVEL: {stderr}"
    );
}

// ── 4. --loglevel beats LOGLEVEL ──────────────────────────────────────────────

#[test]
fn test_loglevel_flag_overrides_env() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &["--loglevel", "warn"], &[("LOGLEVEL", "trace")]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("transform[") && !stderr.contains("cache: fetching"),
        "DEBUG/TRACE leaked despite --loglevel warn: {stderr}"
    );
    assert!(
        !stderr.contains("rule["),
        "TRACE leaked despite --loglevel warn: {stderr}"
    );
}

// ── 5. Default level is INFO — no DEBUG/TRACE chatter ─────────────────────────

#[test]
fn test_default_no_debug_chatter() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &[], &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("transform["),
        "unexpected DEBUG line at default level: {stderr}"
    );
    assert!(
        !stderr.contains("rule["),
        "unexpected TRACE line at default level: {stderr}"
    );
    // INFO-level lines should still be present (back-compat for grep).
    assert!(
        stderr.contains("transforming") && stderr.contains("commit(s)"),
        "expected INFO 'transforming N commit(s)' line: {stderr}"
    );
}

// ── 6. Invalid LOGLEVEL is a clear error ──────────────────────────────────────

#[test]
fn test_invalid_loglevel_env_fails() {
    let env = Env::new(passthrough_config());
    env.commit("first", &[("a.txt", "a")]);
    let out = run_sync(&env, &[], &[("LOGLEVEL", "banana")]);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid log level") && stderr.contains("banana"),
        "expected clear error mentioning 'invalid log level' and 'banana': {stderr}"
    );
}

// ── 7. `git xf diff -v` forwards -v to git diff ───────────────────────────────

#[test]
fn test_diff_v_flag_forwarded() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = env.run_diff(&["-v", &sha1, &sha2]);
    // `git diff -v` is accepted (it's a synonym for --patch + verbose).
    // What matters is that we don't spuriously emit DEBUG/TRACE lines —
    // -v after `diff` must not be intercepted as our verbose flag.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("transform[") && !stderr.contains("rule["),
        "our verbose flag was intercepted instead of forwarded: {stderr}"
    );
    assert!(
        out.status.success(),
        "diff failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
}

// ── 8. LOGLEVEL works for diff ────────────────────────────────────────────────

#[test]
fn test_diff_with_loglevel_env() {
    let env = Env::new(passthrough_config());
    let sha1 = env.commit("first", &[("a.txt", "aaa")]);
    let sha2 = env.commit("second", &[("b.txt", "bbb")]);
    env.sync(&[]);

    let out = Command::new(BIN)
        .current_dir(&env.source)
        .env("LOGLEVEL", "debug")
        .arg("diff")
        .arg(&sha1)
        .arg(&sha2)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cache: fetching"),
        "expected DEBUG cache line via LOGLEVEL on diff: {stderr}"
    );
}
