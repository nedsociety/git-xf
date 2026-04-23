# git-xf Implementation Plan

## Architecture

### Crate layout

```
git-xf/
  Cargo.toml
  src/
    main.rs       — tokio entrypoint, CLI dispatch
    cli.rs        — clap derive structs for all subcommands
    config.rs     — .git-transform.yaml types + validation
    error.rs      — thiserror error hierarchy
    git.rs        — thin wrappers over std::process::Command git calls
    cache.rs      — bare-clone lifecycle: init, fetch, worktree prune
    transform.rs  — single-commit pipeline (steps 1–9 from spec)
    sync.rs       — DAG walk, topo-sort, parallel dispatch, push
    status.rs     — status subcommand
    hook.rs       — pre-push hook install/uninstall
    init.rs       — git xf init
```

### Dependencies

| Crate | Purpose |
|---|---|
| `clap` (derive) | CLI |
| `serde` + `serde_yaml` | Config parsing |
| `tokio` (full) | Async runtime + Semaphore for parallelism |
| `thiserror` | Error types |
| `anyhow` | Error propagation in main |
| `regex` or `once_cell` | Name validation regex |

---

## Key design decisions

**No libgit2.** All git operations via `std::process::Command`. `git.rs` provides typed wrappers that capture stdout/stderr and return `Result<String, GitError>`. `GitError` always carries sha + transform name + stderr.

**Deterministic target SHAs.** `GIT_COMMITTER_DATE` is set equal to the source commit date, so same input → same output SHA across runs.

**Mapping ref as the source of truth.** A commit is "done" iff `refs/git-xf/<name>/<sha>` exists in the local cache. No separate database.

**Parallel DAG execution.** Build an in-degree map over the missing-commit subgraph. Maintain a `Arc<Mutex<HashMap<SourceSha, TargetSha>>>` for resolved mappings. Use a `tokio::sync::Semaphore` of size `--jobs`. When a task completes, decrement dependents' in-degrees and spawn newly-unblocked tasks.

---

## Implementation phases

### Phase 1 — Scaffold
- `cargo init --name git-xf`
- Add all dependencies to `Cargo.toml`
- Empty module files with `pub mod` declarations
- `main.rs`: `#[tokio::main]`, parse CLI, dispatch

### Phase 2 — Config (`config.rs`)
- `TransformConfig` struct matching the YAML schema
- `Config` = `HashMap<String, TransformConfig>`
- Validate name regex `[a-zA-Z0-9_-]+` at parse time
- `Config::load(path)` → reads `.git-transform.yaml` from repo root

### Phase 3 — Git primitives (`git.rs`)
One function per git operation; each runs a `Command`, checks exit code, returns stdout or a `GitError`. Key functions:
- `resolve_ref(repo, refname) -> Sha`
- `log_parents(repo, sha) -> Vec<(Sha, Vec<Sha>)>` — used for DAG walk
- `commit_info(repo, sha)` — author, committer, message, parents
- `worktree_add(repo, path, sha)`
- `worktree_add_orphan(repo, path, branch)`
- `worktree_remove_force(repo, path)`
- `write_tree(worktree) -> Sha`
- `commit_tree(repo, tree, parents, env, message) -> Sha`
- `update_ref(repo, refname, sha)`
- `push(repo, refspecs)`
- `read_ref(repo, refname) -> Option<Sha>`

### Phase 4 — Cache management (`cache.rs`)
- `Cache::ensure_initialized(name, target_url)` — bare clone with `--filter=tree:0`, adds `refs/git-xf/<name>/*` fetch refspec
- `Cache::fetch_and_prune(name)` — `git fetch --filter=tree:0 origin` + `git worktree prune`
- `Cache::path(name) -> PathBuf` — `.git/git-xf/<name>.git`
- `Cache::mapping(name, sha) -> Option<TargetSha>` — reads mapping ref

### Phase 5 — Single-commit transform (`transform.rs`)
`transform_commit(ctx: &TransformCtx) -> Result<TargetSha>` where `TransformCtx` carries source sha, config, cache path, semaphore (already acquired by caller), resolved parent target shas.

Steps mirror spec exactly (1–9). Key logic:
- `skip-commit-messages`: check before running command; if matched and not merge commit, return parent's target sha directly
- `ignore-error`: branch on exit code after `rule.command`
- `changeless`: after `write-tree`, compare resulting tree SHA to parent commit's tree SHA; if same and not merge commit, apply `changeless` policy
- Commit message appends `git-xf-source:` / `git-xf-transform:` trailers
- Pass all 6 author/committer env vars verbatim from source

### Phase 6 — Sync (`sync.rs`)
`sync(config, refs, dry_run, jobs)`:

1. For each transformation:
   a. `Cache::ensure_initialized` + `Cache::fetch_and_prune`
   b. Resolve each REF to a SHA
   c. **DAG walk**: BFS from each tip; stop at commits already in cache; collect missing set + their parent edges
   d. **Topo sort**: Kahn's algorithm over missing set
   e. **Parallel dispatch**: Kahn-style streaming — maintain a ready queue of commits whose parents are all resolved; spawn tokio tasks bounded by `Semaphore`; when a task completes, update the shared mapping and enqueue newly-unblocked commits
   f. **Branch tip update**: `update-ref refs/heads/<branch>` to tip's target sha
   g. **Single push**: collect all new mapping refs + branch refs into one `git push` refspec list

### Phase 7 — Status (`status.rs`)
- `Cache::fetch_and_prune` for each transformation
- Walk commits on `--branch` (default: current branch)
- For each commit: check mapping ref → Mapped / Pending
- Failed = mapping ref points to a commit whose message starts with `[git-xf error]`

### Phase 8 — Hook (`hook.rs`)
- `hook install`: write shell script to `.git/hooks/pre-push`; script reads stdin lines, filters by `branches` whitelist, calls `git xf sync <sha>...`; `chmod +x`
- `hook uninstall`: remove the file (or restore backup if one was made)

### Phase 9 — Init (`init.rs`)
- Read config
- For each transformation: run `Cache::ensure_initialized`
- `--target <path>` overrides the target in config (single-transformation convenience)

### Phase 10 — Polish
- `--dry-run`: thread a `dry_run: bool` flag through sync; skip all writes, print planned actions
- Structured progress output (eprintln to stderr, results to stdout)

### Phase 11 — Integration test
- Create a source repo programmatically, run `git-xf` as a subprocess, assert target repo state

---

## File creation order

The cleanest build order to avoid compile errors:

1. `Cargo.toml` + `error.rs`
2. `config.rs`
3. `git.rs`
4. `cache.rs`
5. `transform.rs`
6. `sync.rs`
7. `status.rs`, `hook.rs`, `init.rs`
8. `cli.rs` + `main.rs`
