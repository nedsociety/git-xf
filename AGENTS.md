# git-xf — Agent Specification

## What this project is

`git-xf` is a git subcommand (invoked as `git xf`) that transforms one git repository into another by applying a set of user-defined rules to each commit. It preserves the full commit graph structure, including merges, in the target repository.

Typical use cases:
- **Artifact repository**: run `make build` on each commit; push the build output as commits to a separate repo.
- **Generated source repository**: run a code generator on each commit; push the generated files as commits.

---

## Command-line interface

```
git xf sync [--dry-run] [--jobs <n>] [<REF>...]
git xf init [--target <path>]
git xf status [--branch <branch>]
git xf hook install
git xf hook uninstall
```

### `git xf sync`

**Default REF:** `HEAD`

Per named transformation in `.git-xf.yaml`:

1. Resolve each REF to a commit SHA.
2. Check whether `refs/git-xf/<name>/<sha>` exists in the local cache. If missing, recurse into parent commits until the entire missing subgraph is identified.
3. Topologically sort the missing commits.
4. Transform each missing commit (parallelizing across commits whose parents are all done — see Pipeline and Parallelism sections).
5. After all commits are created locally, perform a single `git push` to origin.

`--dry-run` prints what would be transformed without touching the cache or remote.
`--jobs <n>` sets max parallel workers (default: logical CPU count).

### `git xf status [--branch <branch>]`

Shows, per named transformation, which commits on the branch are Mapped / Pending / Failed. Reads mapping refs from the local cache (fetched on demand).

### `git xf hook install` / `git xf hook uninstall`

Installs or removes a `pre-push` hook in `.git/hooks/`.

---

## Configuration file: `.git-xf.yaml`

Lives in the root of the **source** repository. The root is a map of named transformations.

Transformation names must match `[a-zA-Z0-9_-]+` — the name is used as both a ref path component (`refs/git-xf/<name>/...`) and a filesystem directory name (`.git/git-xf/<name>.git`). Validated at parse time.

```yaml
# .git-xf.yaml

protocol-codegen:
  target: https://github.com/my-org/protocol-codegen.git  # path or URL

  rule:
    command: make generate
    # output accepts: null/omitted (entire worktree), a single "src:dst" string,
    # a list of "src[:dst]" strings, or a {src: dst} map.
    # Source paths may be absolute (for out-of-tree build dirs).
    output:
      - protocol/:.  # copy protocol/ from source into the target root

  changeless: empty-commit   # empty-commit | skip

  skip-commit-messages:      # skip source commits whose message contains any of these (substring match)
    - "[skip-xf]"

  ignore-error: error        # error | empty-commit | skip

  branches:                  # whitelist for automatic syncs; no effect on manual `git xf sync`
    - main

artifacts:
  target: ../local-artifacts-repo

  rule:
    shell: bash              # sh (default) | any shell reachable via /usr/bin/env
    # targetEnv enables build-your-own-target mode: git-xf creates a fresh empty
    # directory and passes its path via the named env var. command populates it;
    # that directory becomes the target commit tree. Mutually exclusive with output.
    command: |
      make build
      cp -r dist/ "$XF_TARGET/"
    targetEnv: XF_TARGET

  changeless: empty-commit
  ignore-error: error
  branches:
    - main
```

### Field reference

| Field | Type | Default | Description |
|---|---|---|---|
| `target` | string | required | Path or URL to the target git repository |
| `rule.command` | string | required | Shell command to run on each source commit (runs in the source worktree root) |
| `rule.shell` | string | `"sh"` | Shell used to run `command`. `"sh"` → `sh -c`; anything else → `/usr/bin/env $shell -c` |
| `rule.output` | string \| list \| map | entire worktree | Output mode: files/dirs to copy into the target commit. Each entry is `"src"` or `"src:dst"`; source paths may be absolute. Mutually exclusive with `targetEnv`. |
| `rule.targetEnv` | string | — | Build-your-own-target mode: name of the env var seeded with a fresh empty directory that `command` should populate. Mutually exclusive with `output`. |
| `changeless` | `empty-commit` \| `skip` | `empty-commit` | What to do when the transform produces no diff vs. the previous target commit. Never applies to merge commits. |
| `skip-commit-messages` | list of strings | `[]` | Substring match against source commit message; matched commits are mapped to their first parent's target commit. Never applies to merge commits. |
| `ignore-error` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a non-zero exit from `rule.command` |
| `branches` | list of strings | `[]` | Branch whitelist for automatic syncs (pre-push hook, CI). Has no effect on manual `git xf sync`. |

### Merge commit exception for `changeless` and `skip-commit-messages`

Both skip policies are suppressed for merge commits — a merge commit always produces a real target commit. Silently dropping a merge commit would make one of its parent lines unreachable from the target graph.

### `ignore-error` semantics

- `error`: abort and report (exit non-zero).
- `empty-commit`: create a target commit with the error in the message; content carries over from parent.
- `skip`: map the failing commit to the same target commit as its parent, eliding it from target history.

---

## Ref mapping convention

```
refs/git-xf/<transformation-name>/<source-commit-sha>  →  <target-commit-sha>
```

Stored in the **target** repository (and mirrored in the local cache). The target repo is self-contained — it carries all information needed for incremental sync without touching the source repo's ref namespace.

---

## Branch and tag mirroring

After a sync run, `git xf sync` scans **all** `refs/heads/*` and `refs/tags/*` in the source repository and mirrors any whose tip commit appears in the mapping table (newly transformed or previously cached) to the target:

- **Branches**: every `refs/heads/<branch>` in the source whose tip commit has a mapping causes `refs/heads/<branch>` in the target to be updated.
- **Tags**: every `refs/tags/<tag>` in the source whose resolved commit has a mapping causes `refs/tags/<tag>` in the target to be updated as a **lightweight tag**. Annotated tags are dereferenced to their tagged commit for the mapping lookup; the tag object itself is not re-created.

This scan is driven by the mapping table, not the explicit REF arguments. REF arguments only control which commit subgraph is traversed; once commits are mapped (now or from a prior run), any source branch or tag pointing to them is automatically mirrored.

All updated branch and tag refspecs are included in the single `git push` at the end of each sync run.

---

## Target repository local cache

A bare clone per named transformation at `.git/git-xf/<name>.git`.

**Initial setup:**
```
git clone --bare --filter=tree:0 <target-url> .git/git-xf/<name>.git
git -C .git/git-xf/<name>.git config --add remote.origin.fetch \
  '+refs/git-xf/<name>/*:refs/git-xf/<name>/*'
```

`--filter=tree:0` downloads commit objects eagerly, tree/blob objects lazily — sufficient since sync never checks out old target content. The extra fetch refspec is required because the default bare clone only fetches `refs/heads/*`; without it `git fetch` silently ignores all mapping refs and the cache always appears empty.

**On each sync run**, before anything else:
```
git -C .git/git-xf/<name>.git fetch --filter=tree:0 origin
git -C .git/git-xf/<name>.git worktree prune
```

`worktree prune` removes stale entries left by interrupted previous runs; without it the next `git worktree add` to the same path fails.

---

## Commit creation pipeline

No `git commit` is called. The orphan branch is a throwaway index; commits are created via `write-tree` → `commit-tree` → `update-ref`.

Per commit:

1. **Source worktree**: `git worktree add <src-wt-path> <source-sha>`

2. **Run rule**: execute `rule.command` in `<src-wt-path>`. On non-zero exit apply `ignore-error` policy.

3. **Orphaned target worktree** (starts with empty index — blank staging area in the cache's object store):
   ```
   git -C .git/git-xf/<name>.git worktree add --orphan -b xf-work-<source-sha> <tgt-wt-path>
   ```

4. **Populate** the target worktree — two modes:
   - **Output mode** (`targetEnv` absent): copy `rule.output` entries from `<src-wt-path>` to `<tgt-wt-path>`. Each entry is a `(src, dst)` pair; source paths may be absolute. If `output` is omitted/null, copy the entire source worktree excluding `.git`.
   - **Build-your-own-target mode** (`targetEnv` set): create a fresh empty temp directory, expose its path as `$(<targetEnv>)` in `rule.command`'s environment, then copy the temp directory's contents into `<tgt-wt-path>` after the command exits.

5. **Snapshot**:
   ```
   git -C <tgt-wt-path> add <output-paths>
   TREE=$(git -C <tgt-wt-path> write-tree)
   ```

6. **Create commit**: resolve each source parent SHA via `refs/git-xf/<name>/<parent-sha>` to get target parent SHAs. Root commits have no `-p`.
   ```
   COMMIT=$(git -C .git/git-xf/<name>.git commit-tree "$TREE" \
     -p <target-parent-1> [-p <target-parent-2> ...] \
     -m "<original-message>

   git-xf-source: <source-sha>
   git-xf-transform: <name>")
   ```
   Pass `GIT_AUTHOR_NAME/EMAIL/DATE` and `GIT_COMMITTER_NAME/EMAIL/DATE` verbatim from source. `GIT_COMMITTER_DATE` must be set (not left as wall-clock time) — same source commit + same rule output must produce the same target SHA across runs.

7. **Record mapping**: `git -C .git/git-xf/<name>.git update-ref "refs/git-xf/<name>/<source-sha>" "$COMMIT"`

8. **Update branch tip** (once per branch, after all commits in the batch):
   `git -C .git/git-xf/<name>.git update-ref refs/heads/<branch> <tip-sha>`

9. **Clean up**:
   ```
   git -C .git/git-xf/<name>.git worktree remove --force <tgt-wt-path>
   git worktree remove <src-wt-path>
   ```
   `--force` is required on the target worktree because its index has staged content but no commit was ever made on the orphan branch.

---

## Single push at the end

Git cannot push raw SHAs — refspecs require named source refs. Steps 7–8 provide them. After the full batch:

```
git -C .git/git-xf/<name>.git push origin \
  refs/heads/<branch>:refs/heads/<branch> \
  refs/git-xf/<name>/<sha1>:refs/git-xf/<name>/<sha1> \
  refs/git-xf/<name>/<sha2>:refs/git-xf/<name>/<sha2> \
  ...
```

---

## Commit graph preservation

For a source graph:

```
C0 ← C1 ← C2
      ↑
      C3 (merge, parents: C1, C4)
```

The target graph must be:

```
T(C0) ← T(C1) ← T(C2)
          ↑
          T(C3)  (parents: T(C1), T(C4))
```

---

## Parallelism

Commits whose parents are all transformed can run concurrently. Each parallel job gets its own source worktree and target orphaned worktree. Concurrency is bounded by a semaphore of size `--jobs`.

---

## Commit message format

Normal:
```
<original message>

git-xf-source: <source-sha>
git-xf-transform: <name>
```

Error marker (`ignore-error: empty-commit`):
```
[git-xf error] <name> failed on <source-sha>

<truncated stderr>

git-xf-source: <source-sha>
git-xf-transform: <name>
```

---

## Automatic synchronization

### Pre-push hook

Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines from stdin. Filters to refs whose branch name appears in the transformation's `branches` whitelist, then calls `git xf sync <local-sha>...`. Aborts the push (exit non-zero) if sync fails and `ignore-error: error`.

Bypassed by `--no-verify` — not authoritative.

### GitHub Actions CI

Workflow at `.github/workflows/git-xf-sync.yml`, trigger `on: push`.

1. Checkout source with `fetch-depth: 0`.
2. Configure target credentials (SSH deploy key or `GITHUB_TOKEN`).
3. Filter `github.event.ref` against each transformation's `branches` whitelist.
4. Call `git xf sync <github.event.after>` for matching transformations.

Authoritative sync path — runs for all contributors, cannot be bypassed. Pre-push hook and CI coexist; CI is the backstop.

---

## Implementation notes (Rust)

- Invoke raw `git` commands via `std::process::Command`. No libgit2.
- Config: `serde` + `serde_yaml`.
- CLI: `clap` with derive macros.
- Parallelism: `tokio` with a `Semaphore` bounding concurrent worktrees.
- Binary name: `git-xf` (git resolves `git xf` to it when on `$PATH`).
- Error messages must include: source commit SHA, transformation name, raw command stderr.
