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
| `skip-commit-messages` | list of strings | `[]` | Substring-match against the source commit message; matched commits map to their first parent's target commit. Never applies to merge commits. |
| `ignore-error` | `error` \| `empty-commit` \| `skip` | `error` | How to handle a non-zero exit from `rule.command` |
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

### `git xf sync [--dry-run] [--jobs <n>] [<REF>...]`

Transforms all commits reachable from each REF (default: `HEAD`) that have not yet been mapped, then performs a single push to each target.

- `--dry-run` — print what would be transformed without writing anything.
- `--jobs <n>` — max parallel workers (default: logical CPU count).

After the sync, every `refs/heads/*` and `refs/tags/*` in the source whose tip commit has a mapping is mirrored to the target.

### `git xf init [--target <path>]`

Sets up local bare-clone caches under `.git/git-xf/`. Run once before the first `sync`. `--target` overrides the target URL from config (useful when there is only one transformation).

### `git xf status [--branch <branch>]`

Shows the mapping state (Mapped / Pending / Failed) for each commit on `<branch>` (default: current branch), per transformation.

### `git xf hook install` / `git xf hook uninstall`

Installs or removes a `pre-push` hook that calls `git xf sync` automatically before each push to a whitelisted branch.

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
    branches: ["main"]

jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      # Configure target repo credentials here (SSH key, token, etc.)
      - run: |
          git xf init
          git xf sync "${{ github.event.after }}"
```

---

## How it works

For each source commit, `git xf sync`:

1. Checks whether `refs/git-xf/<name>/<source-sha>` already exists in the local cache.
2. Topologically sorts unmapped commits and transforms them in dependency order (parallelised by `--jobs`).
3. Each transform: checks out the source commit in a temporary worktree → runs `rule.command` → stages the result into an orphaned target worktree → runs `write-tree` / `commit-tree` → records the mapping ref.
   - **Output mode** (`rule.output`): copies declared paths from the source worktree into the target worktree.
   - **BYOT mode** (`rule.targetEnv`): `command` receives a fresh empty directory and populates it directly; git-xf stages its contents.
4. Performs a single `git push` with all new mapping refs and updated branch/tag refs.

`GIT_COMMITTER_DATE` is set to the source commit date so that the same input always produces the same target SHA, making syncs idempotent.

---

## License

MIT
