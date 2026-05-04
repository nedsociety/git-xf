# Feature Plan: `--push-chunk`, `--depth`, `--all-branches`

## Goals

Support running `git xf sync` on large, already-existing repositories:
- **Interrupt-safe progress**: push in chunks so Ctrl+C between chunks preserves work for the next run.
- **Shallow mode**: limit transformation to the last N commits so massive histories can be synced incrementally.
- **All-branches**: transform every branch in one command.

---

## CLI changes

```
git xf sync [--push-chunk=<limit>] [--depth=<N>] [--all-branches] [--dry-run] [--jobs <n>] [--rule <head|commit>] [<REF>...]
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--push-chunk=<limit>` | count or size | `50M` | Push after every N commits (`--push-chunk=100`) or every N bytes of transformed object data (`--push-chunk=50M`). `--push-chunk=0` disables chunking (single push at the end). |
| `--depth=<N>` | usize ≥ 1 | unlimited | Transform at most N commits (BFS distance from each tip). Boundary commits become synthetic roots — **this intentionally breaks the full-graph invariant**. See note below. `--depth=0` is rejected at parse time. |
| `--all-branches` | bool | false | Use all `refs/heads/*` in the source repo as tips instead of explicit REFs. Mutually exclusive with explicit `<REF>` args — clap conflict, error if both provided. |

### `--push-chunk` syntax

```
--push-chunk=0        # no limit, single push at the end
--push-chunk=100      # at most 100 commits per push
--push-chunk=50M      # at most 50 MB of object data per push  ← default
--push-chunk=100M     # GitHub's push size limit is ~100 MB
```

Suffixes: `B`, `K`, `M`, `G` (case-insensitive, powers of 1024).

Default of `50M` gives a comfortable margin below GitHub's 100 MB push limit.

---

## New type: `ChunkLimit` (in `src/sync.rs`)

```rust
#[derive(Debug, Clone, Copy)]
pub enum ChunkLimit {
    None,           // --push-chunk=0: single push at the end
    Count(usize),   // --push-chunk=100: at most N commits per push
    Size(u64),      // --push-chunk=50M: at most N bytes per push (loose object sizes)
}

impl Default for ChunkLimit {
    fn default() -> Self { ChunkLimit::Size(50 * 1024 * 1024) }
}
```

Parser rules for `FromStr`:
- `"0"` → `None`
- Digits only → `Count`
- Digits + `B/K/M/G` suffix (case-insensitive) → `Size` in bytes

---

## Implementation plan

### 1. `--all-branches` (smallest change, no parallelism impact)

Handled entirely in `sync::run` — no threading into `sync_one`.

```rust
// In sync::run, replacing the existing effective_refs logic:
let effective_refs: Vec<String> = if all_branches {
    // for_each_ref returns HashMap<refname, sha>; pass refnames so
    // resolve_tips resolves them. BFS deduplicates shared commits via `visited`.
    git::for_each_ref(source_repo, &["refs/heads/"])?
        .into_keys()
        .collect()
} else if refs.is_empty() {
    vec!["HEAD".to_string()]
} else {
    refs.to_vec()
};
```

`--all-branches` and explicit `<REF>` args are marked as conflicting in clap (`conflicts_with`). `--dry-run` is unaffected — it returns early before dispatch regardless.

---

### 2. `--depth` (BFS change only)

Validated at parse time: `--depth=0` is rejected with an error (`value_parser` returning `Err` for `0`). `depth: Option<NonZeroUsize>` or a custom parser suffices.

Change `find_missing` signature:

```rust
fn find_missing(
    source_repo: &Path,
    cached: &HashMap<String, String>,
    tips: &[String],
    depth: Option<usize>,  // None = unlimited; Some(n) with n >= 1 guaranteed
) -> Result<(MissingMap, KnownMap)>
```

BFS queue carries `(sha, distance_from_tip)`:

```rust
let mut queue: VecDeque<(String, usize)> = tips.iter()
    .filter(|t| visited.insert((*t).clone()))
    .map(|t| (t.clone(), 0))
    .collect();

while let Some((sha, dist)) = queue.pop_front() {
    if let Some(target_sha) = cached.get(&sha) {
        known.insert(sha, Some(target_sha.clone()));
        continue;
    }
    // Stop at depth cutoff: don't transform, don't recurse.
    // Parents of boundary commits will be absent from the mapping table →
    // boundary commits get empty target_parents → synthetic roots.
    if depth.map_or(false, |d| dist >= d) {
        continue;
    }
    let info = git::commit_info(source_repo, &sha)?;
    for parent in &info.parents {
        if visited.insert(parent.clone()) {
            queue.push_back((parent.clone(), dist + 1));
        }
    }
    missing.insert(sha, info.parents);
}
```

No changes to `dispatch` or `transform_commit` — the existing `filter_map` in target-parents resolution already handles absent parents correctly.

**Known limitation (must be prominent in docs and CLI help)**: `--depth` intentionally breaks the project's full-graph preservation invariant. Boundary commits become synthetic roots in the target, and merge commits whose second parent falls outside the shallow range produce disconnected ancestry. Filling in pre-boundary history later does not retroactively stitch the graph.

This trade-off is acceptable as an explicit opt-in for large repos. Surface it in two places:
1. **clap help string**: `--depth=<N>  Transform at most N commits from each tip (BFS distance). Boundary commits become synthetic roots; the target graph will not be complete. See docs for details.`
2. **AGENTS.md / README.md**: caveat directly under the `--depth` row in the flag table, not only in a buried limitations section.

---

### 3. `--push-chunk` (structural change to `sync_one`)

#### Current structure

```
find_missing → dispatch (all commits at once) → push (mapping refs + branch/tag refs)
```

#### New structure

```
find_missing → topo_order → chunk loop (dispatch_chunk → push mapping refs)
                          → final push (branch/tag refs only)
```

Two dispatch variants depending on mode:

- **Count / None mode**: slice `topo_order` into fixed-N batches, call existing `dispatch` on each sub-map.
- **Size mode**: new `dispatch_size_chunk` that runs the full parallel loop and signals a chunk boundary when the accumulated size crosses the limit.

#### Count / None mode

```rust
let ordered = topo_order(&missing);
let mut current_known: KnownMap = init_mappings;  // clone is acceptable
let mut chunk_start = 0;

while chunk_start < ordered.len() {
    let chunk_end = match chunk_limit {
        ChunkLimit::None     => ordered.len(),
        ChunkLimit::Count(n) => (chunk_start + n).min(ordered.len()),
        ChunkLimit::Size(_)  => unreachable!(),
    };
    let chunk_shas = &ordered[chunk_start..chunk_end];

    let chunk_missing: MissingMap = chunk_shas.iter()
        .map(|s| (s.clone(), missing[s].clone()))
        .collect();

    // dispatch returns (in addition to writing to cache) the source SHAs that
    // were dropped (Ok(None)), so we can reconstruct None entries in current_known.
    let chunk_dropped_shas = dispatch(DispatchCtx { ... }, &chunk_missing, current_known.clone()).await?;

    let post = cache.all_mappings()?;  // HashMap<String, String>
    let refspecs: Vec<String> = chunk_shas.iter()
        .filter(|s| post.contains_key(*s))
        .map(|s| { let r = cache.mapping_ref(s); format!("{r}:{r}") })
        .collect();
    if !refspecs.is_empty() {
        git::push(&cache.path, &refspecs)?;
    }

    // Rebuild KnownMap. cache.all_mappings() only returns mapped commits (Some);
    // dropped roots (Ok(None) from skip_to_parent) are absent from the cache.
    // In the current dispatch paths, absent and None are functionally equivalent:
    //   - in_degree counts only same-chunk parents, so cross-chunk dropped roots
    //     never influence scheduling.
    //   - target_parents uses filter_map(m.get(p).and_then(|v| v.clone())), which
    //     yields the same result for absent and Some(None).
    //   - children is built from same-chunk parents only.
    // To preserve the None semantic explicitly (and guard against future code paths
    // that distinguish absent from None), dispatch returns the dropped source SHAs
    // and we insert them as None into current_known.
    current_known = post.into_iter().map(|(k, v)| (k, Some(v)))
        .chain(chunk_dropped_shas.into_iter().map(|k| (k, None)))
        .collect();
    chunk_start = chunk_end;
}
```

#### Size mode: parallel accumulator with spillover

**The overshoot problem**: naively draining all in-flight tasks after the limit is crossed means up to `--jobs` extra commits land in the current chunk, potentially pushing it far past the limit. With large commits and high parallelism this can exceed GitHub's 100 MB hard limit.

**Fix: spillover**. When the accumulated size first crosses the limit, stop spawning and drain the remaining in-flight tasks, but route their completions into a `spillover` list rather than the current chunk. The current chunk closes cleanly at the moment the limit was hit. Spillover commits are already in the local cache; they start the next chunk. The next chunk's initial accumulated size is pre-seeded with the spillover's object sizes so it immediately knows how much budget it has left.

**Guarantee**: each chunk's pushed size is strictly less than the limit, with one exception — a single commit whose objects alone exceed the limit is pushed as a best-effort single-commit chunk (an empty push would be pointless).

```rust
async fn dispatch_size_chunk(
    ctx: DispatchCtx,
    missing: MissingMap,        // all remaining commits for this and future chunks
    init_mappings: KnownMap,
    size_limit: u64,
    // Target SHAs already pushed to remote — used as rev-list boundary for size deltas.
    pushed_target_shas: &[String],
    // Objects from spillover of the previous chunk already committed to THIS push.
    // Initial accumulated size is the sum of their sizes.
    spillover_counted: Vec<String>,   // target SHAs
    spillover_accumulated: u64,       // pre-computed size of spillover objects
) -> Result<(
    Vec<String>,   // source SHAs completed in this chunk (to be pushed)
    Vec<String>,   // source SHAs dropped in this chunk (Ok(None), for None entries in KnownMap)
    Vec<String>,   // source SHAs in spillover (in local cache, start next chunk)
    MissingMap,    // commits not yet started (for future chunks)
)>
```

Inner loop — `stop_spawning` + spillover routing:

```rust
let mut accumulated: u64 = spillover_accumulated;
let mut stop_spawning = false;
let mut chunk_shas: Vec<String> = vec![];           // mapped commits in this push
let mut chunk_dropped_shas: Vec<String> = vec![];   // dropped (Ok(None)) in this push
let mut spillover_shas: Vec<String> = vec![];       // goes into next chunk
// counted_in_chunk tracks objects already measured to avoid double-counting.
// Starts with spillover target SHAs so their objects don't inflate new commits' sizes.
let mut counted_in_chunk: Vec<String> = {
    let mut v = pushed_target_shas.to_vec();
    v.extend(spillover_counted);
    v
};

loop {
    while !stop_spawning {
        let Some(sha) = ready.front().cloned() else { break };
        let Ok(permit) = semaphore.clone().try_acquire_owned() else { break };
        ready.pop_front();
        // ... spawn task (identical to existing dispatch) ...
        in_flight += 1;
    }

    if in_flight == 0 { break; }

    let (sha, result) = done_rx.recv().await ...;
    in_flight -= 1;
    let target_sha_opt = /* handle error / extract Ok(opt) */;
    shared.lock().unwrap().insert(sha.clone(), target_sha_opt.clone());
    // Unblock children.

    if stop_spawning {
        // We're draining: all further completions go to spillover.
        spillover_shas.push(sha);
    } else if let Some(ref target_sha) = target_sha_opt {
        let size = loose_objects_size_delta(&ctx.cache.path, target_sha, &counted_in_chunk)?;
        counted_in_chunk.push(target_sha.clone());

        if accumulated + size >= size_limit && !chunk_shas.is_empty() {
            // Adding this commit would exceed the limit AND we already have
            // at least one commit in the chunk → spill this one.
            spillover_shas.push(sha);
            stop_spawning = true;
            // Don't count this commit's size — it belongs to the next chunk.
        } else {
            // Either under the limit, or chunk is empty (best-effort: must include it).
            accumulated += size;
            chunk_shas.push(sha);
            if accumulated >= size_limit {
                stop_spawning = true; // best-effort case: single oversized commit included
            }
        }
    } else {
        // Dropped commit (root skip): no size, track separately for KnownMap None entries.
        chunk_dropped_shas.push(sha);
    }
}

let processed: HashSet<&str> = chunk_shas.iter()
    .chain(chunk_dropped_shas.iter())
    .chain(spillover_shas.iter())
    .map(|s| s.as_str())
    .collect();
let remaining: MissingMap = missing.into_iter()
    .filter(|(s, _)| !processed.contains(s.as_str()))
    .collect();

Ok((chunk_shas, chunk_dropped_shas, spillover_shas, remaining))
```

Outer size-mode loop in `sync_one`:

```rust
let mut remaining = missing;
let mut current_known: KnownMap = init_mappings;
let mut pushed_target_shas: Vec<String> = vec![];
let mut spillover_shas: Vec<String> = vec![];
let mut spillover_accumulated: u64 = 0;

loop {
    let is_done = remaining.is_empty() && spillover_shas.is_empty();
    if is_done { break; }

    // Spillover from the previous chunk seeds this chunk's init_mappings
    // (already in local cache) and its initial accumulated size.
    let spillover_target_shas: Vec<String> = {
        let post = cache.all_mappings()?;
        spillover_shas.iter().filter_map(|s| post.get(s).cloned()).collect()
    };

    let (chunk_shas, chunk_dropped_shas, next_spillover, next_remaining) = dispatch_size_chunk(
        DispatchCtx { ... },
        remaining,
        current_known.clone(),
        size_limit,
        &pushed_target_shas,
        spillover_target_shas,
        spillover_accumulated,
    ).await?;

    // Push mapping refs for this chunk (excludes spillover).
    let post = cache.all_mappings()?;
    let refspecs: Vec<String> = chunk_shas.iter()
        .filter(|s| post.contains_key(*s))
        .map(|s| { let r = cache.mapping_ref(s); format!("{r}:{r}") })
        .collect();
    if !refspecs.is_empty() {
        git::push(&cache.path, &refspecs)?;
    }

    let new_pushed: Vec<String> = chunk_shas.iter()
        .filter_map(|s| post.get(s).cloned())
        .collect();
    pushed_target_shas.extend(new_pushed);

    // Pre-compute next spillover's size for the next iteration.
    spillover_accumulated = loose_objects_size_delta_multi(
        &cache.path,
        next_spillover.iter()
            .filter_map(|s| post.get(s))
            .cloned()
            .collect::<Vec<_>>()
            .as_slice(),
        &pushed_target_shas,
    )?;

    // Rebuild KnownMap. post has only mapped commits (Some); insert dropped
    // commits from the chunk and any dropped commits that ended up in spillover
    // (detected as spillover SHAs absent from post) as None.
    current_known = post.iter().map(|(k, v)| (k.clone(), Some(v.clone())))
        .chain(chunk_dropped_shas.into_iter().map(|k| (k, None)))
        .chain(
            next_spillover.iter()
                .filter(|s| !post.contains_key(*s))
                .map(|s| (s.clone(), None))
        )
        .collect();
    remaining = next_remaining;
    spillover_shas = next_spillover;
}
```

#### Size measurement: `loose_objects_size_delta`

```rust
fn loose_objects_size_delta(
    cache_path: &Path,
    target_sha: &str,
    already_counted: &[String],
) -> Result<u64> {
    // git rev-list --objects <sha> --not <counted...>
    let objects = git::rev_list_objects(cache_path, &[target_sha], already_counted)?;
    Ok(objects.iter().map(|sha| loose_object_size(cache_path, sha)).sum())
}

fn loose_object_size(cache_path: &Path, sha: &str) -> u64 {
    let path = cache_path.join("objects").join(&sha[..2]).join(&sha[2..]);
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// Multi-target variant used by the size-mode outer loop to pre-compute the
// accumulated size of spillover objects before the next chunk starts.
fn loose_objects_size_delta_multi(
    cache_path: &Path,
    target_shas: &[String],
    already_counted: &[String],
) -> Result<u64> {
    if target_shas.is_empty() {
        return Ok(0);
    }
    let objects = git::rev_list_objects(cache_path, target_shas, already_counted)?;
    Ok(objects.iter().map(|sha| loose_object_size(cache_path, sha)).sum())
}
```

`git rev-list --objects` is called once per completed commit during dispatch, O(commits) total. Loose file sizes (compressed on disk) are a practical proxy for push payload.

**Size guarantee summary**:
- Normal case: chunk size ≤ `size_limit` (the commit that would exceed the limit spills to the next chunk; a commit that lands exactly on the limit is included in the current chunk).
- Best-effort case: a single commit whose objects alone exceed `size_limit` is included alone in its own chunk regardless of limit.
- In-flight overshoot: zero — in-flight completions after limit detection go to spillover, not the current push.

#### Branch/tag refs: final push only

After all chunks complete:

```rust
let post_mapped = cache.all_mappings()?;  // HashMap<String, String>
let mut branch_refspecs = vec![];
for (refname, src_sha) in git::for_each_ref(source_repo, &["refs/heads/", "refs/tags/"])? {
    if let Some(target_sha) = post_mapped.get(&src_sha) {
        git::update_ref(&cache.path, &refname, target_sha)?;
        branch_refspecs.push(format!("{refname}:{refname}"));
    }
}
if !branch_refspecs.is_empty() {
    git::push(&cache.path, &branch_refspecs)?;
}
```

---

## Documentation updates

### AGENTS.md

- Update CLI synopsis to include `--push-chunk`, `--depth`, `--all-branches`.
- Add field descriptions for each flag.
- Add `--depth` known-limitation note.
- Note that `--push-chunk=0` restores the old single-push behavior.
- Note that `50M` default targets GitHub's 100 MB push limit.

### README.md

- Same CLI additions.

---

## Integration tests

| Test | What it checks |
|---|---|
| `test_all_branches` | `--all-branches` syncs commits from every branch; commits shared across branches are transformed exactly once. |
| `test_all_branches_conflicts_with_refs` | Passing both `--all-branches` and an explicit REF errors out. |
| `test_depth_limits_commits` | `--depth=2` on a 5-commit chain transforms only the last 2; the boundary commit is a synthetic root in the target. |
| `test_depth_with_already_mapped` | BFS stops at already-mapped commits before reaching the depth limit; no re-transformation occurs. |
| `test_depth_all_branches` | `--depth=2 --all-branches` applies the depth limit per branch tip. |
| `test_depth_zero_rejected` | `--depth=0` fails at argument parse time with a clear error message, before any git operations occur. |
| `test_push_chunk_count` | `--push-chunk=2` on 5 commits produces exactly 3 mapping-ref pushes + 1 branch-ref push. Verified via `post-receive` hook counter (see below). |
| `test_push_chunk_size` | `--push-chunk=<small>` triggers multiple push rounds on a repo with measurable object data. Verified via hook counter. |
| `test_push_chunk_single_oversized` | A single commit whose objects exceed the size limit is pushed as its own chunk without error. |
| `test_push_chunk_zero` | `--push-chunk=0` produces exactly 1 mapping-ref push + 1 branch-ref push. Verified via hook counter. |
| `test_push_chunk_resume` | Manually pre-push a subset of mapping refs to the target (using raw `git push`), then run sync; only the remaining commits are transformed and pushed. |

#### Observing push-round count: `post-receive` hook counter

The pre-push partial state pattern validates resumability but not chunk-boundary behaviour within a single sync invocation. A regression that accidentally reverts to a single final push would pass those tests.

To directly observe push batching, install a `post-receive` hook in the target bare repo that appends a line to a log file on each invocation:

```sh
#!/usr/bin/env sh
echo "push" >> "$GIT_DIR/push-log.txt"
```

After sync, count lines in `push-log.txt` and assert the expected number of rounds. For `--push-chunk=2` on 5 commits the expected count is 3 mapping-ref rounds + 1 final branch-ref round = 4.

This works with the real binary, requires no mocking, and will catch any regression to a single-push path.

---

## Out of scope

- Parallelism optimisation for `already_counted` growth in `loose_objects_size_delta` (as more commits are counted the `--not` arg list grows; can be replaced with a single pack-reachability check if it becomes a bottleneck).
- Adaptive chunk sizing based on observed push latency.
- Resuming mid-chunk after Ctrl+C — only whole-chunk boundaries are interrupt-safe.
- `--depth` + `--rule=commit` interaction: boundary commits whose parents are absent become synthetic roots via the existing `filter_map` path — no special handling needed.
