# git-xf

`git xf` transforms one git repository into another by applying a user-defined shell command to each commit. It preserves the full commit graph — including merges — in the target repository.

**Typical uses**

- **Artifact repo** — run `make build` on every commit; push build outputs to a separate repo.
- **Generated source repo** — run a code generator; push the result as a versioned commit history.

---

## Installation

```sh
curl -fsSL https://raw.githubusercontent.com/nedsociety/git-xf/main/install.sh | sh
```

To install a specific version or to a custom directory:

```sh
VERSION=v0.1.0 INSTALL_DIR=~/.local/bin \
  curl -fsSL https://raw.githubusercontent.com/nedsociety/git-xf/main/install.sh | sh
```

Or from source:

```sh
cargo install --git https://github.com/nedsociety/git-xf
```

---

## Quick start

```sh
# 1. Add .git-xf.yaml to your source repo (see Configuration below).

# 2. Initialise local caches.
git xf init

# 3. Transform HEAD and push to each target repo.
git xf sync
```

---

## Configuration

Create `.git-xf.yaml` in the root of the source repository. The file is a map of named transformations.

```yaml
# .git-xf.yaml

protocol-codegen:
  target: https://github.com/my-org/protocol-codegen.git  # path or URL

  rule:
    command: make generate
    # output accepts a list of "src:dst" pairs, a {src: dst} map, a single
    # string, or null/omitted to stage the entire source worktree.
    # Source paths may be absolute (useful for out-of-tree build dirs).
    output:
      - protocol/:.   # copy protocol/ from source into the target root

  changeless: skip           # empty-commit | skip
  ignore-error: error        # error | empty-commit | skip

  skip-commit-messages:      # skip source commits whose message contains any of these
    - "[skip-xf]"

  branches:                  # whitelist for automatic syncs (hook / CI)
    - main

artifacts:
  target: ../local-artifacts-repo

  rule:
    shell: bash              # sh (default) | bash | zsh | any shell on PATH
    # targetEnv enables "build-your-own-target" mode: git-xf creates an empty
    # directory and passes its path via the named env var. The command is
    # responsible for populating it; that directory becomes the target commit.
    # Mutually exclusive with output.
    command: |
      make build
      cp -r dist/ "$XF_TARGET/"
    targetEnv: XF_TARGET

  changeless: empty-commit
  ignore-error: empty-commit
  branches:
    - main
```

### Field reference

| Field | Type | Default | Description |
|---|---|---|---|
| `target` | string | required | Path or URL to the target git repository |
| `rule.command` | string | required | Shell command run in the source worktree root on each commit |
| `rule.shell` | string | `"sh"` | Shell used to run `command`. `"sh"` → `sh -c`; anything else → `/usr/bin/env $shell -c` |
| `rule.output` | string \| list \| map | entire worktree | Output-mode: files/dirs to copy from source worktree into the target commit. Each entry is `"src"` (same destination) or `"src:dst"`. Source paths may be absolute. Mutually exclusive with `targetEnv`. |
| `rule.targetEnv` | string | — | Build-your-own-target mode: name of the env var seeded with a fresh empty directory that `command` should populate. Mutually exclusive with `output`. |
| `changeless` | `empty-commit` \| `skip` | `empty-commit` | What to do when the transform produces no diff vs. the previous commit. Never applies to merge commits. |
| `skip-commit-messages` | list of strings | `[]` | Substring-match against the source commit message; matched commits map to the first mapped direct parent. If no direct parent is mapped (root commit, or all direct parents were dropped), the commit is dropped and no mapping ref is written. Never applies to merge commits. |
| `ignore-error` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a non-zero exit from `rule.command`. For `skip`: map the commit to the first mapped direct parent; if no direct parent is mapped, drop the commit and write no mapping ref. For merge commits, `skip` is suppressed to preserve topology (falls back to `empty-commit`). |
| `missing` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a source commit whose `.git-xf.yaml` is missing or whose `rule` block is unparseable (only relevant with `--rule=commit`). `error` and `empty-commit` behave like `ignore-error`. `skip` maps to the first mapped direct parent (or drops if none), but unlike `ignore-error: skip` it does not fall back to `empty-commit` for merge commits. |
| `branches` | list of strings | `[]` | Branch whitelist for automatic syncs (hook / CI). Has no effect on manual `git xf sync`. |

### `rule.output` formats

All four of these are equivalent:

```yaml
# null / omitted — stage entire source worktree
output:

# single string
output: "src:dst"

# list of strings
output:
  - generated/:.    # copy generated/ into target root
  - /tmp/build/extra.txt:extras/extra.txt  # absolute source path

# map
output:
  generated/: .
  /tmp/build/extra.txt: extras/extra.txt
```

---

## Commands

### `git xf sync [options] [<REF>...]`

Transforms all commits reachable from each REF (default: `HEAD`) that have not yet been mapped, then pushes mapping refs and updated branch/tag refs to the target.

- `--dry-run` — print what would be transformed without writing anything.
- `--jobs <n>` — max parallel workers (default: logical CPU count).
- `--rule <head|commit>` — where to read the `rule` block from (default: `commit`):
  - `head`: use the rule from the current HEAD's `.git-xf.yaml` for every commit.
  - `commit`: read the `rule` block from each source commit's own `.git-xf.yaml`; apply the `missing` policy if absent or unparseable.
- `--push-chunk <n|nB|nK|nM|nG>` — push mapping refs incrementally after every N commits or N bytes of new loose objects (default: `50M`). Use `0` to push everything at the end.
- `--depth <n>` — limit sync to commits within BFS distance `n` from the tips (≥ 1). Boundary commits become synthetic roots in the target.
- `--all-branches` — use all `refs/heads/*` as tips instead of explicit REFs. Cannot be combined with explicit REF arguments.

After the sync, every `refs/heads/*` and `refs/tags/*` in the source whose tip commit has a mapping is mirrored to the target.

### `git xf init [--target <path>]`

Sets up local bare-clone caches under `.git/git-xf/`. Run once before the first `sync`. `--target` overrides the target URL from config (useful when there is only one transformation).

### `git xf status [--branch <branch>]`

Shows the mapping state (Mapped / Pending / Failed) for each commit on `<branch>` (default: current branch), per transformation.

### `git xf hook install` / `git xf hook uninstall`

Installs or removes a `pre-push` hook that calls `git xf sync` automatically before each push to a whitelisted branch.

### `git xf diff [-x <transform>] [<diff-options>] <revisions> [-- <path>...]`

Runs `git diff` against the **transformed** (target) repository using the local cache. Revision arguments are resolved in the source repo and mapped to their corresponding target SHAs; the diff itself is then executed in the cache.

Supported revision forms:
- `<sha1> <sha2>` — two separate commits
- `<sha1>..<sha2>` — two-dot range
- `<sha1>...<sha2>` — three-dot merge-base range
- `<sha1>` alone — diff the transformed commit against its parent (requires a clean working tree)

The `-x <transform>` flag, when needed, **must be the first argument** (before any diff options).

A best-effort cache fetch is attempted before resolving SHAs. If a commit has not been synced yet, the command exits with an error and a hint to run `git xf sync`.

### `git xf pr [-x <transform>] <branch> [<base>]`

Prints (or opens) a GitHub compare URL for the given branch in the transformed target repository.

- If `<base>` is supplied, the URL is `…/compare/<base>…<branch>`; otherwise `…/compare/<branch>`.
- When stdout is an interactive TTY and `open`/`xdg-open` is available, the URL is opened in the browser instead of printed.
- Both HTTPS (`https://[user[:token]@]github.com/…`) and SSH (`git@github.com:…`) remote URLs are supported.
- Both `<branch>` and `<base>` must exist in the target cache; run `git xf sync` first if they are missing.

---

## Automatic sync

### Pre-push hook

```sh
git xf hook install
```

The hook reads the refs being pushed and calls `git xf sync <sha>` for each transformation whose `branches` list includes the pushed branch. The push is aborted if sync fails (unless `ignore-error: skip` or `empty-commit` is set).

The hook can be bypassed with `git push --no-verify`; use CI as the authoritative backstop.

### GitHub Actions

```yaml
# .github/workflows/git-xf-sync.yml
name: git-xf sync

on:
  push:

concurrency:
  group: git-xf-sync
  cancel-in-progress: false

jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Restore target repo cache
        uses: actions/cache@v4
        with:
          path: .git/git-xf
          key: git-xf-cache

      - name: Install git-xf
        run: curl -fsSL https://raw.githubusercontent.com/nedsociety/git-xf/main/install.sh | sh

      # Configure credentials for the target repo(s). Only one is needed
      # unless you have multiple transformations targeting different hosts.
      - name: Configure target credentials
        env:
          TARGET_TOKEN: ${{ secrets.TARGET_GITHUB_TOKEN }}
          TARGET_SSH_KEY: ${{ secrets.TARGET_SSH_KEY }}
        run: |
          if [ -n "$TARGET_TOKEN" ]; then
            git config --global \
              url."https://x-access-token:${TARGET_TOKEN}@github.com/".insteadOf \
              "https://github.com/"
          fi
          if [ -n "$TARGET_SSH_KEY" ]; then
            echo "$TARGET_SSH_KEY" > /tmp/deploy_key
            chmod 600 /tmp/deploy_key
            echo "GIT_SSH_COMMAND=ssh -i /tmp/deploy_key -o StrictHostKeyChecking=no" \
              >> "$GITHUB_ENV"
          fi

      - name: Init target repos (if cache miss)
        run: git xf init

      - name: Sync all branches
        run: git xf sync --all-branches
```

---

## How it works

For each source commit, `git xf sync`:

1. Checks whether `refs/git-xf/<name>/<source-sha>` already exists in the local cache.
2. Topologically sorts unmapped commits and transforms them in dependency order (parallelised by `--jobs`).
3. Each transform: checks out the source commit in a temporary worktree → runs `rule.command` → stages the result into an orphaned target worktree → runs `write-tree` / `commit-tree` → records the mapping ref.
   - **Output mode** (`rule.output`): copies declared paths from the source worktree into the target worktree.
   - **BYOT mode** (`rule.targetEnv`): `command` receives a fresh empty directory and populates it directly; git-xf stages its contents.
4. Pushes mapping refs to the target in chunks (see `--push-chunk`), then pushes updated branch/tag refs in a final push.

`GIT_COMMITTER_DATE` is set to the source commit date so that the same input always produces the same target SHA, making syncs idempotent.

---

## Releasing

Releases are managed with [`cargo-release`](https://github.com/crate-ci/cargo-release).

```sh
cargo release patch          # dry-run preview (or: minor, major)
cargo release patch --execute
```

This bumps the version in `Cargo.toml`, commits, tags, and pushes. The CI release workflow picks up the tag and builds binaries automatically.

---

## License

MIT
