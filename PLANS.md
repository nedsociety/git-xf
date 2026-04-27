# Feature Plan: Per-commit rule reading (`--rule`)

## Overview

Currently `git xf sync` always uses the `rule` block from HEAD's `.git-xf.yaml` when
transforming every source commit. This is wrong for repos where the build command,
output paths, or shell script evolves over time — old commits should be transformed
with the rule that was current at that point, not the rule from today.

This feature adds a `--rule` flag to `git xf sync` that controls how the `rule` block
is sourced for each commit.

---

## Design

### CLI

```
git xf sync [--rule=head|commit] [--dry-run] [--jobs <n>] [<REF>...]
```

Default: `--rule=commit`.

### Modes

| Mode | Behavior |
|---|---|
| `head` | Always use the `rule` block from HEAD's `.git-xf.yaml`. Current behavior; opt-in escape hatch. |
| `commit` *(default)* | Read `.git-xf.yaml` from each source commit. Extract the `rule` block for this transformation. If the rule is missing or the YAML is malformed, apply the transformation's `missing` policy (see below). |

### Authoritative HEAD config

Regardless of `--rule`, HEAD's `.git-xf.yaml` is authoritative for everything except
the `rule` block:

- Which transformations exist and run (HEAD-only; transformations removed from HEAD are
  never run, even against old commits that contained them).
- `target`, `changeless`, `ignore-error`, `skip-commit-messages`, `branches`, `missing`.

Only the `rule` block (`command`, `shell`, `output`, `targetEnv`) is optionally
overridden per-commit.

### "Missing" definition

In `--rule=commit` mode, a commit's rule is **missing** when:
- `.git-xf.yaml` does not exist in that commit's tree, OR
- The file exists but the transformation block is absent.

Both cases are treated identically. A YAML parse error is also treated as missing.

### New config field: `missing`

```yaml
my-transform:
  target: ...
  rule:
    command: make build
  missing: error          # error | empty-commit | skip   (default: error)
```

Controls what happens in `--rule=commit` mode when the per-commit rule cannot be read.
Has no effect in `--rule=head` mode.

| Value | Behavior |
|---|---|
| `error` *(default)* | Abort sync and report the commit/transformation. |
| `empty-commit` | Create a target commit whose tree carries over from the parent, with a `[git-xf missing-rule]` marker in the message. |
| `skip` | Map to the first mapped direct parent (or drop if none — same semantics as `ignore-error: skip` on root commits). |

The `missing` and `ignore-error` policies are independent. A commit can succeed at
running the command but produce an empty diff (`changeless`), or fail the command
(`ignore-error`), or have no rule at all (`missing`). Each case has its own policy.

### Error / empty-commit message format

```
[git-xf missing-rule] <name> on <source-sha>

<reason: "no .git-xf.yaml" | "transformation block absent" | "YAML parse error: ...">

git-xf-source: <source-sha>
git-xf-transform: <name>
```

### Field-level fallback: none

If a commit's rule block exists but omits optional fields (e.g. no `shell`), normal
field defaults apply. HEAD's values for those fields are **not** consulted.

### Already-cached commits

Unaffected. Once a commit is in the cache, its mapping is stable and it is never
re-transformed regardless of rule changes.

---

## Implementation plan

### 1. Config / types (`src/config.rs`)

- Add `MissingPolicy` enum (mirrors `IgnoreErrorPolicy` shape):
  ```rust
  #[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
  #[serde(rename_all = "kebab-case")]
  pub enum MissingPolicy { #[default] Error, EmptyCommit, Skip }
  ```
- Add `missing: MissingPolicy` field to `TransformConfig`.
- Add `RuleSource` enum:
  ```rust
  pub enum RuleSource { Head, Commit }
  ```
- Add a free function `parse_rule(yaml: &str, name: &str) -> Result<Option<RuleConfig>>`
  that deserializes a `.git-xf.yaml` string and returns the `rule` block for the named
  transformation, or `None` if the transformation block is absent.

### 2. CLI (`src/main.rs`)

- Add `--rule <mode>` argument to the `Sync` subcommand (clap, `value_enum`, default
  `Commit`).
- Thread `RuleSource` through `sync::run`.

### 3. Sync orchestration (`src/sync.rs`)

- Add `rule_source: RuleSource` to `DispatchCtx`.
- Thread it through to `TransformCtx`.

### 4. Transform pipeline (`src/transform.rs`)

- Add `rule_source: RuleSource` to `TransformCtx`.
- After the source worktree is checked out, resolve the effective rule before calling
  `run_rule`:

  ```
  if rule_source == Head:
      effective_rule = &ctx.config.rule
  else:  // Commit
      match read_and_parse(<src_wt>/.git-xf.yaml, ctx.name):
          Ok(Some(rule)) => effective_rule = rule
          Ok(None)       => reason = "transformation block absent"  → apply missing policy
          Err(io_err)    => reason = "no .git-xf.yaml"              → apply missing policy
          Err(yaml_err)  => reason = "YAML parse error: {yaml_err}" → apply missing policy
  ```

- "Apply missing policy" mirrors the `ignore-error` early-return paths:
  - `Error` → return `Err(Error::MissingRule { ... })`
  - `EmptyCommit` → `parent_tree_sha` + `create_and_record` with missing-rule message
  - `Skip` → `skip_to_parent` (inherits root-commit drop semantics)

- A new `error::Error::MissingRule { name, sha, reason }` variant for the `error` case.
- Pass `effective_rule` to `run_rule` and `copy_output`/BYOT instead of
  `&ctx.config.rule`.

### 5. Integration tests (`tests/integration.rs`)

- `test_rule_source_head` — `--rule=head` uses HEAD rule; per-commit `.git-xf.yaml`
  differences are ignored.
- `test_rule_source_commit_uses_per_commit_rule` — `--rule=commit` picks up rule changes
  between commits.
- `test_rule_source_commit_missing_file_error` — no `.git-xf.yaml` in commit +
  `missing: error` → sync fails.
- `test_rule_source_commit_missing_block_error` — file present, block absent +
  `missing: error` → sync fails.
- `test_rule_source_commit_missing_skip` — `missing: skip` → commit mapped to parent
  (or dropped if root).
- `test_rule_source_commit_missing_empty_commit` — `missing: empty-commit` →
  `[git-xf missing-rule]` marker in target commit message.
- `test_rule_source_commit_parse_error` — malformed YAML → applies `missing` policy.
- `test_rule_source_commit_default` — no `--rule` flag → behaves as `--rule=commit`.
- `test_rule_source_head_ignores_missing_policy` — `--rule=head` + `missing: skip` →
  `missing` policy never fires.

### 6. Documentation

- `AGENTS.md`: add `--rule` to the `git xf sync` CLI section; add `missing` to the
  field reference table; describe the `missing` policy and its independence from
  `ignore-error`.
- `README.md`: same.

---

## Out of scope (future)

- Auto-detection: automatically use per-commit rule when the block is present and HEAD
  rule when absent (eliminates the flag).
- Per-transformation `rule-source` config field (finer-grained control per
  transformation).
