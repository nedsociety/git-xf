# git-xf — Agent Specification

## What this project is

`git-xf` is a git subcommand (invoked as `git xf`) that transforms one git repository into another by applying a set of user-defined rules to each commit. It preserves the full commit graph structure, including merges, in the target repository.

Typical use cases:
- **Artifact repository**: run `make build` on each commit; push the build output as commits to a separate repo.
- **Generated source repository**: run a code generator on each commit; push the generated files as commits.

---

## Command-line interface

```
git xf sync [--dry-run] [--jobs <n>] [--rule <head|commit>]
            [--push-chunk <n|nB|nK|nM|nG>] [--depth <n>] [--all-branches]
            [<REF>...]
git xf init [--target <path>]
git xf status [--branch <branch>]
git xf hook install
git xf hook uninstall
git xf diff [-x <transform>] [<diff-options>] <revisions> [-- <path>...]
git xf pr   [-x <transform>] <branch> [<base>]
```

### `git xf sync`

**Default REF:** `HEAD`

Per named transformation in `.git-xf.yaml`:

1. Resolve each REF to a commit SHA (or resolve all `refs/heads/*` with `--all-branches`).
2. Check whether `refs/git-xf/<name>/<sha>` exists in the local cache. If missing, recurse into parent commits until the entire missing subgraph is identified. With `--depth <n>`, the BFS stops at distance ≥ n from the nearest tip; boundary commits become synthetic roots.
3. Topologically sort the missing commits.
4. Transform each missing commit (parallelizing across commits whose parents are all done — see Pipeline and Parallelism sections).
5. Push mapping refs to origin in chunks (controlled by `--push-chunk`). After all commits are transformed, push updated branch/tag refs in a final push.

`--dry-run` prints what would be transformed without touching the cache or remote.
`--jobs <n>` sets max parallel workers (default: logical CPU count).
`--rule <head|commit>` controls which `.git-xf.yaml` is used to read the `rule` block (default: `commit`):
- `head`: use the rule from HEAD's `.git-xf.yaml` for every commit (same rule across the whole sync).
- `commit`: read the `rule` block from each source commit's own `.git-xf.yaml`. If the file is missing or the block is unparseable, apply the `missing` policy defined in the transformation config.

`--push-chunk <limit>` controls how often mapping refs are pushed mid-sync (default: `50M`):
- A plain number (e.g. `100`) pushes after every N commits.
- A suffixed number (e.g. `50M`, `1G`) pushes when accumulated new loose-object bytes exceed the limit.
- `0` disables intermediate pushes — a single mapping push at the end (before the branch/tag push).

`--depth <n>` limits the BFS to commits at distance < n from any tip (must be ≥ 1). Commits at the boundary are not transformed; their children in the transformed graph become synthetic roots with no parents. Distance is determined by first-discovery BFS order, not the shortest path across all tips.

`--all-branches` uses all `refs/heads/*` as sync tips instead of explicit REFs. Conflicts with explicit REF arguments.

### `git xf status [--branch <branch>]`

Shows, per named transformation, which commits on the branch are Mapped / Pending / Failed. Reads mapping refs from the local cache (fetched on demand).

### `git xf hook install` / `git xf hook uninstall`

Installs or removes a `pre-push` hook in `.git/hooks/`.

### `git xf diff [-x <transform>] [<diff-options>] <revisions> [-- <path>...]`

Runs `git diff` in the target cache repo, with source-repo revisions translated to their mapped target SHAs.

**Argument parsing**: `-x <transform>` must be the very first argument when present — it is extracted by `main.rs` before clap sees the rest of the argv. Everything remaining is collected as a flat `Vec<String>` via clap's `trailing_var_arg`. The subcommand itself does not invoke clap for the inner arguments.

**Revision detection** (implemented in `src/diff.rs`): The argument list is split on the first bare `--` into `options_and_revs` and `paths`. Token walking skips `-`-prefixed tokens, then calls `git rev-parse --verify` on each remaining token:
- If the token contains `...` or `..` (three-dot checked first to avoid misparsing), both halves must verify; the token is treated as a range `RevToken`.
- Otherwise, if the single token verifies, it is treated as a commit `RevToken`.
- Tokens that fail `rev-parse` are silently left as-is (treated as bare path arguments before `--`).

**Revision forms**:
- Two separate commit tokens → two-commit form.
- Single token with `..` or `...` → range form.
- Single commit token with no separator → single-commit form: requires a clean working tree, appends the mapped `HEAD` SHA to the reconstructed argv so the diff is relative to parent.

**SHA mapping**: All source SHAs from the rev tokens (and `HEAD` in single-commit form) are looked up via `refs/git-xf/<name>/<sha>` in the local cache. Missing mappings exit with an error; the message mentions a failed cache fetch if applicable.

**Execution**: The reconstructed argv (with target SHAs substituted) is passed to `git diff` run inside the cache. The process exit code is forwarded via `std::process::exit`.

### `git xf pr [-x <transform>] <branch> [<base>]`

Opens or prints a GitHub compare URL for the transformed branch.

**Implementation** (`src/pr.rs`):
1. Selects the transformation (same single-or-named logic as other subcommands).
2. Best-effort `fetch_and_prune` of the local cache.
3. Verifies `<branch>` (and `<base>` if given) exist in the cache via `git::resolve_ref`.
4. Reads `remote.origin.url` from the cache with `git config`.
5. Parses the URL as a GitHub remote — supports HTTPS (`https://[user[:token]@]github.com/<org>/<repo>[.git]`) and SSH (`git@github.com:<org>/<repo>[.git]`). Non-GitHub targets exit with an error.
6. Builds `https://github.com/<org>/<repo>/compare/<base>...<branch>` (or without `<base>` prefix when omitted).
7. If stdout is a TTY (`std::io::IsTerminal`) and `open`/`xdg-open` spawns successfully, opens the URL in the browser (non-blocking). Otherwise prints it.

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
| `rule.output` | string \| list \| map | entire worktree | Output mode: files/dirs to copy into the target commit. Each entry is `"src"` or `"src:dst"`; source paths may be absolute. An explicit empty list (`[]`) means copy nothing. Mutually exclusive with `targetEnv`. |
| `rule.targetEnv` | string | — | Build-your-own-target mode: name of the env var seeded with a fresh empty directory that `command` should populate. Mutually exclusive with `output`. |
| `rule.copyParent` | bool | `false` | Requires `targetEnv` to be set. If true, the `targetEnv` directory is pre-populated with the first target parent commit's tree before `command` runs. If there are multiple source parents, the first is used. If there is no parent (root commit), the directory starts empty as usual. |
| `changeless` | `empty-commit` \| `skip` | `empty-commit` | What to do when the transform produces no diff vs. the previous target commit. Never applies to merge commits. |
| `skip-commit-messages` | list of strings | `[]` | Substring match against source commit message; matched commits map to the first mapped direct parent. If no direct parent is mapped (root commit, or all direct parents were dropped), the commit is dropped and no mapping ref is written. Never applies to merge commits. |
| `ignore-error` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a non-zero exit from `rule.command` |
| `missing` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a source commit whose `.git-xf.yaml` is missing or whose `rule` block is unparseable (only relevant when `--rule=commit`). `error` and `empty-commit` behave identically to `ignore-error`. `skip` maps to the first mapped direct parent (or drops if none), but unlike `ignore-error: skip` it does **not** fall back to `empty-commit` for merge commits — the merge commit is still collapsed to one parent, potentially breaking target graph topology. |
| `branches` | list of strings | `[]` | Branch whitelist for automatic syncs (pre-push hook, CI). Has no effect on manual `git xf sync`. |

### Merge commit exception for `changeless` and `skip-commit-messages`

Both skip policies are suppressed for merge commits — a merge commit always produces a real target commit. Silently dropping a merge commit would make one of its parent lines unreachable from the target graph.

### `ignore-error` semantics

- `error`: abort and report (exit non-zero).
- `empty-commit`: create a target commit with the error in the message; content carries over from parent.
- `skip`: on command failure, map the commit to the first mapped direct parent. If no direct parent is mapped (root commit, or all direct parents were dropped), drop the commit and write no mapping ref. For merge commits, `skip` is suppressed to preserve topology (falls back to `empty-commit`).

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

`--filter=tree:0` downloads commit objects eagerly, tree/blob objects lazily. Sync never reads old target content in normal operation, so blobs are never needed — unless `rule.copyParent` is true, in which case git will lazily fetch the parent tree's blobs on demand when the parent worktree is checked out. The extra fetch refspec is required because the default bare clone only fetches `refs/heads/*`; without it `git fetch` silently ignores all mapping refs and the cache always appears empty.

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

2. **Resolve rule**: when `--rule=commit`, read `<src-wt-path>/.git-xf.yaml` and extract the `rule` block for this transformation. If the file is absent or the block is unparseable, apply the `missing` policy (same semantics as `ignore-error`). When `--rule=head`, use the rule from the HEAD config unconditionally.

3. **Run rule**: execute `rule.command` in `<src-wt-path>`. On non-zero exit apply `ignore-error` policy.

4. **Orphaned target worktree** (starts with empty index — blank staging area in the cache's object store):
   ```
   git -C .git/git-xf/<name>.git worktree add --orphan -b xf-work-<source-sha> <tgt-wt-path>
   ```

5. **Populate** the target worktree — two modes:
   - **Output mode** (`targetEnv` absent): copy `rule.output` entries from `<src-wt-path>` to `<tgt-wt-path>`. Each entry is a `(src, dst)` pair; source paths may be absolute. If `output` is omitted/null, copy the entire source worktree excluding `.git`.
   - **Build-your-own-target mode** (`targetEnv` set): create a fresh empty temp directory, expose its path as `$(<targetEnv>)` in `rule.command`'s environment, then copy the temp directory's contents into `<tgt-wt-path>` after the command exits.

6. **Snapshot**:
   ```
   git -C <tgt-wt-path> add <output-paths>
   TREE=$(git -C <tgt-wt-path> write-tree)
   ```

7. **Create commit**: resolve each source parent SHA via `refs/git-xf/<name>/<parent-sha>` to get target parent SHAs. Root commits have no `-p`.
   ```
   COMMIT=$(git -C .git/git-xf/<name>.git commit-tree "$TREE" \
     -p <target-parent-1> [-p <target-parent-2> ...] \
     -m "<original-message>

   git-xf-source: <source-sha>
   git-xf-transform: <name>")
   ```
   Pass `GIT_AUTHOR_NAME/EMAIL/DATE` and `GIT_COMMITTER_NAME/EMAIL/DATE` verbatim from source. `GIT_COMMITTER_DATE` must be set (not left as wall-clock time) — same source commit + same rule output must produce the same target SHA across runs.

8. **Record mapping**: `git -C .git/git-xf/<name>.git update-ref "refs/git-xf/<name>/<source-sha>" "$COMMIT"`

9. **Update branch tip** (once per branch, after all commits in the batch):
   `git -C .git/git-xf/<name>.git update-ref refs/heads/<branch> <tip-sha>`

10. **Clean up**:
   ```
   git -C .git/git-xf/<name>.git worktree remove --force <tgt-wt-path>
   git worktree remove <src-wt-path>
   ```
   `--force` is required on the target worktree because its index has staged content but no commit was ever made on the orphan branch.

---

## Pushing to origin

Git cannot push raw SHAs — refspecs require named source refs. Steps 7–8 provide them.

**Mapping-ref pushes** happen once per chunk (controlled by `--push-chunk`). Each chunk push includes only the mapping refs for commits completed in that chunk:

```
git -C .git/git-xf/<name>.git push origin \
  refs/git-xf/<name>/<sha1>:refs/git-xf/<name>/<sha1> \
  refs/git-xf/<name>/<sha2>:refs/git-xf/<name>/<sha2> \
  ...
```

**Branch/tag push** happens once at the end, after all chunks are complete:

```
git -C .git/git-xf/<name>.git push origin \
  refs/heads/<branch>:refs/heads/<branch> \
  refs/tags/<tag>:refs/tags/<tag> \
  ...
```

With `--push-chunk=0` (`ChunkLimit::None`), there is a single mapping-ref push followed by the branch/tag push — two pushes total per transformation.

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

---

## Releasing

Releases are managed with [`cargo-release`](https://github.com/crate-ci/cargo-release).

```sh
cargo release patch          # dry-run preview (or: minor, major)
cargo release patch --execute
```

This bumps the version in `Cargo.toml`, commits, tags, and pushes. The CI release workflow picks up the tag and builds binaries automatically.
