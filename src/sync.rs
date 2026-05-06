use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

use crate::cache::Cache;
use crate::config::{Config, RuleSource, TransformConfig};
use crate::git;
use crate::transform::{transform_commit, TransformCtx};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Runs `git push` on a threadpool thread so it doesn't block the async executor.
/// Network I/O during push can take seconds; holding a tokio thread for that
/// would starve other in-flight async work.
async fn blocking_push(repo: PathBuf, refspecs: Vec<String>) -> Result<()> {
    tokio::task::spawn_blocking(move || git::push(&repo, &refspecs))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("push worker panicked: {e}")))
}

// ── ChunkLimit ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum ChunkLimit {
    None,
    Count(usize),
    Size(u64),
}

impl Default for ChunkLimit {
    fn default() -> Self {
        ChunkLimit::Size(50 * 1024 * 1024)
    }
}

impl FromStr for ChunkLimit {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let last = s.chars().last().unwrap_or('\0');
        if last.is_ascii_alphabetic() {
            let num_str = &s[..s.len() - 1];
            let n: u64 = num_str
                .parse()
                .map_err(|_| format!("`{num_str}` is not a valid number"))?;
            let multiplier: u64 = match last.to_ascii_uppercase() {
                'B' => 1,
                'K' => 1024,
                'M' => 1024 * 1024,
                'G' => 1024 * 1024 * 1024,
                c => return Err(format!("unknown suffix '{c}'; use B, K, M, or G")),
            };
            Ok(if n == 0 {
                ChunkLimit::None
            } else {
                ChunkLimit::Size(n * multiplier)
            })
        } else {
            let n: usize = s
                .parse()
                .map_err(|_| format!("`{s}` is not a valid number"))?;
            Ok(if n == 0 {
                ChunkLimit::None
            } else {
                ChunkLimit::Count(n)
            })
        }
    }
}

// ── sync::run ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run(
    source_repo: &Path,
    git_dir: &Path,
    config: &Config,
    refs: &[String],
    dry_run: bool,
    jobs: usize,
    rule_source: RuleSource,
    chunk_limit: ChunkLimit,
    depth: Option<usize>,
    all_branches: bool,
) -> Result<()> {
    let effective_refs: Vec<String> = if all_branches {
        git::for_each_ref(source_repo, &["refs/heads/"])?
            .into_iter()
            .map(|(refname, _)| refname)
            .collect()
    } else if refs.is_empty() {
        vec!["HEAD".to_string()]
    } else {
        refs.to_vec()
    };

    let mut names: Vec<&String> = config.keys().collect();
    names.sort();
    for name in names {
        let cfg = &config[name];
        sync_one(
            source_repo,
            git_dir,
            name,
            cfg,
            &effective_refs,
            dry_run,
            jobs,
            rule_source,
            chunk_limit,
            depth,
        )
        .await
        .with_context(|| format!("transformation '{name}' failed"))?;
    }
    Ok(())
}

// ── per-transformation sync ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn sync_one(
    source_repo: &Path,
    git_dir: &Path,
    name: &str,
    cfg: &TransformConfig,
    refs: &[String],
    dry_run: bool,
    jobs: usize,
    rule_source: RuleSource,
    chunk_limit: ChunkLimit,
    depth: Option<usize>,
) -> Result<()> {
    let cache = Arc::new(Cache::new(git_dir, name));
    cache.ensure_initialized(&cfg.target)?;
    cache.fetch_and_prune()?;

    let tip_shas = resolve_tips(source_repo, refs)?;
    log::debug!("sync[{name}]: tips = {:?}", tip_shas);

    let pre_mapped = cache.all_mappings()?;
    let (missing, init_mappings) = find_missing(source_repo, &pre_mapped, &tip_shas, depth)?;
    log::debug!(
        "sync[{name}]: {} missing, {} cached, depth={depth:?}",
        missing.len(),
        init_mappings.len()
    );

    if dry_run {
        if missing.is_empty() {
            return Ok(());
        }
        println!("[{name}] would transform {} commit(s):", missing.len());
        for sha in topo_order(&missing) {
            println!("  {sha}");
        }
        return Ok(());
    }

    let total_missing = missing.len();
    if total_missing > 0 {
        log::info!("[{name}] transforming {} commit(s)...", total_missing);

        let config_arc = Arc::new(cfg.clone());
        let make_ctx = || DispatchCtx {
            source_repo: source_repo.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
            cache: cache.clone(),
            config: config_arc.clone(),
            name: name.to_string(),
            jobs,
            rule_source,
        };

        match chunk_limit {
            ChunkLimit::None | ChunkLimit::Count(_) => {
                let ordered = topo_order(&missing);
                let mut current_known: KnownMap = init_mappings;
                let mut chunk_start = 0;
                let mut chunk_idx = 0usize;

                while chunk_start < ordered.len() {
                    let chunk_end = match chunk_limit {
                        ChunkLimit::None => ordered.len(),
                        ChunkLimit::Count(n) => (chunk_start + n).min(ordered.len()),
                        ChunkLimit::Size(_) => unreachable!(),
                    };
                    let chunk_shas = &ordered[chunk_start..chunk_end];
                    log::debug!(
                        "sync[{name}]: chunk {chunk_idx}: {} commits",
                        chunk_shas.len()
                    );

                    let chunk_missing: MissingMap = chunk_shas
                        .iter()
                        .map(|s| (s.clone(), missing[s].clone()))
                        .collect();

                    let dropped =
                        dispatch(make_ctx(), &chunk_missing, current_known.clone()).await?;

                    let post = cache.all_mappings()?;
                    let refspecs: Vec<String> = chunk_shas
                        .iter()
                        .filter(|s| post.contains_key(*s))
                        .map(|s| {
                            let r = cache.mapping_ref(s);
                            format!("{r}:{r}")
                        })
                        .collect();
                    log::debug!(
                        "sync[{name}]: pushing {} mapping refs",
                        refspecs.len()
                    );
                    blocking_push(cache.path.clone(), refspecs).await?;

                    current_known = post
                        .into_iter()
                        .map(|(k, v)| (k, Some(v)))
                        .chain(dropped.into_iter().map(|k| (k, None)))
                        .collect();
                    chunk_start = chunk_end;
                    chunk_idx += 1;
                }
            }

            ChunkLimit::Size(size_limit) => {
                let mut remaining = missing;
                let mut current_known: KnownMap = init_mappings;
                let mut pushed_target_shas: Vec<String> = vec![];
                let mut spillover_shas: Vec<String> = vec![];
                let mut spillover_accumulated: u64 = 0;
                let mut chunk_idx = 0usize;

                loop {
                    if remaining.is_empty() && spillover_shas.is_empty() {
                        break;
                    }
                    log::debug!(
                        "sync[{name}]: chunk {chunk_idx}: accumulated {spillover_accumulated}B / limit {size_limit}B, {} remaining",
                        remaining.len()
                    );

                    let spillover_target_shas: Vec<String> = {
                        let post = cache.all_mappings()?;
                        spillover_shas
                            .iter()
                            .filter_map(|s| post.get(s).cloned())
                            .collect()
                    };

                    let (chunk_shas, chunk_dropped, next_spillover, next_remaining) =
                        dispatch_size_chunk(
                            make_ctx(),
                            remaining,
                            current_known.clone(),
                            size_limit,
                            &pushed_target_shas,
                            spillover_target_shas,
                            spillover_accumulated,
                        )
                        .await?;

                    let post = cache.all_mappings()?;
                    // Push mapping refs for spillover from the previous round (now confirmed
                    // in cache) plus the commits completed in this round.
                    let refspecs: Vec<String> = spillover_shas
                        .iter()
                        .chain(chunk_shas.iter())
                        .filter(|s| post.contains_key(*s))
                        .map(|s| {
                            let r = cache.mapping_ref(s);
                            format!("{r}:{r}")
                        })
                        .collect();
                    log::debug!(
                        "sync[{name}]: pushing {} mapping refs",
                        refspecs.len()
                    );
                    blocking_push(cache.path.clone(), refspecs).await?;

                    pushed_target_shas
                        .extend(spillover_shas.iter().filter_map(|s| post.get(s).cloned()));
                    pushed_target_shas
                        .extend(chunk_shas.iter().filter_map(|s| post.get(s).cloned()));

                    let next_spill_targets: Vec<String> = next_spillover
                        .iter()
                        .filter_map(|s| post.get(s))
                        .cloned()
                        .collect();
                    spillover_accumulated = loose_objects_size_delta_multi(
                        &cache.path,
                        &next_spill_targets,
                        &pushed_target_shas,
                    )?;

                    current_known = post
                        .iter()
                        .map(|(k, v)| (k.clone(), Some(v.clone())))
                        .chain(chunk_dropped.into_iter().map(|k| (k, None)))
                        .chain(
                            next_spillover
                                .iter()
                                .filter(|s| !post.contains_key(*s))
                                .map(|s| (s.clone(), None)),
                        )
                        .collect();
                    remaining = next_remaining;
                    spillover_shas = next_spillover;
                    chunk_idx += 1;
                }
            }
        }
    }

    // Push branch/tag refs — always runs so a new branch pointing to an
    // already-mapped commit is mirrored without requiring new transforms.
    let post_mapped = cache.all_mappings()?;
    let mut branch_refspecs: Vec<String> = vec![];
    for (refname, src_sha) in git::for_each_ref(source_repo, &["refs/heads/", "refs/tags/"])? {
        if let Some(target_sha) = post_mapped.get(&src_sha) {
            git::update_ref(&cache.path, &refname, target_sha)?;
            branch_refspecs.push(format!("{refname}:{refname}"));
        }
    }
    log::debug!(
        "sync[{name}]: pushing {} branch/tag refs",
        branch_refspecs.len()
    );
    blocking_push(cache.path.clone(), branch_refspecs).await?;

    if total_missing > 0 {
        log::info!("[{name}] synced {} commit(s)", total_missing);
    }
    Ok(())
}

fn resolve_tips(source_repo: &Path, refs: &[String]) -> Result<Vec<String>> {
    refs.iter()
        .map(|r| git::resolve_ref(source_repo, r))
        .collect()
}

type MissingMap = HashMap<String, Vec<String>>;
/// `source_sha → Some(target_sha)` for mapped commits, `None` for dropped roots.
type KnownMap = HashMap<String, Option<String>>;

/// BFS from `tips`, collecting commits not yet present in `cached`.
///
/// When `depth` is `Some(d)`, commits at BFS distance ≥ d are skipped entirely;
/// their children get empty `target_parents` and become synthetic roots in the target.
///
/// Distance is assigned on **first discovery** in BFS order (not the minimum
/// graph distance across all tips). With multiple tips, a shared ancestor's
/// distance is determined by whichever tip's BFS wave reaches it first.
///
/// Returns:
/// - `missing`: source SHA → source parent SHAs (commits that need transforming)
/// - `known`: source SHA → target SHA (commits already in the cache)
fn find_missing(
    source_repo: &Path,
    cached: &HashMap<String, String>,
    tips: &[String],
    depth: Option<usize>,
) -> Result<(MissingMap, KnownMap)> {
    // Fetch all uncached reachable commits and their parent SHAs in one
    // subprocess, replacing the previous O(n) `git show` per-commit loop.
    let cached_list: Vec<&str> = cached.keys().map(|s| s.as_str()).collect();
    let all_parents = git::log_parents(source_repo, tips, &cached_list)?;

    let mut missing: HashMap<String, Vec<String>> = HashMap::new();
    let mut known: HashMap<String, Option<String>> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    for tip in tips {
        if visited.insert(tip.clone()) {
            queue.push_back((tip.clone(), 0));
        }
    }

    while let Some((sha, dist)) = queue.pop_front() {
        if let Some(target_sha) = cached.get(&sha) {
            known.insert(sha, Some(target_sha.clone()));
            continue;
        }
        // Depth cutoff: don't transform and don't recurse. Children will have no
        // entry in the mapping table → their target_parents will be empty → they
        // become synthetic roots.
        if depth.is_some_and(|d| dist >= d) {
            continue;
        }
        // Parents come from the batch result; absent means a true git root commit.
        let parents: Vec<String> = all_parents.get(&sha).cloned().unwrap_or_default();
        for parent in &parents {
            if visited.insert(parent.clone()) {
                queue.push_back((parent.clone(), dist + 1));
            }
        }
        missing.insert(sha, parents);
    }

    // Seed known with direct parents of missing commits that aren't themselves
    // missing — either they're already cached (Some) or depth-cut / git roots (None).
    for parents in missing.values() {
        for parent in parents {
            if !missing.contains_key(parent) && !known.contains_key(parent) {
                known.insert(parent.clone(), cached.get(parent).cloned());
            }
        }
    }

    log::debug!(
        "find_missing: visited {} commits ({} missing, {} known)",
        visited.len(),
        missing.len(),
        known.len()
    );
    Ok((missing, known))
}

/// Returns `missing` keys in topological order (parents before children).
fn topo_order(missing: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut in_degree: HashMap<&str, usize> = missing.keys().map(|k| (k.as_str(), 0)).collect();
    for (sha, parents) in missing {
        for p in parents {
            if missing.contains_key(p) {
                *in_degree.get_mut(sha.as_str()).unwrap() += 1;
            }
        }
    }
    let mut ready: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&s, _)| s)
        .collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (sha, parents) in missing {
        for p in parents {
            if missing.contains_key(p) {
                children.entry(p.as_str()).or_default().push(sha.as_str());
            }
        }
    }
    let mut order = Vec::with_capacity(missing.len());
    while let Some(sha) = ready.pop_front() {
        order.push(sha.to_string());
        if let Some(kids) = children.get(sha) {
            for child in kids {
                let d = in_degree.get_mut(child).unwrap();
                *d -= 1;
                if *d == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    debug_assert_eq!(
        order.len(),
        missing.len(),
        "topo_order: incomplete — cycle in graph?"
    );
    order
}

// ── parallel dispatch ─────────────────────────────────────────────────────────

struct DispatchCtx {
    source_repo: PathBuf,
    git_dir: PathBuf,
    cache: Arc<Cache>,
    config: Arc<TransformConfig>,
    name: String,
    jobs: usize,
    rule_source: RuleSource,
}

/// Transforms all commits in `missing` in parallel (bounded by `jobs`),
/// respecting topological order. Returns the source SHAs of dropped commits
/// (those that returned `Ok(None)`) so callers can insert them as `None` in
/// `KnownMap` for the next chunk.
async fn dispatch(
    ctx: DispatchCtx,
    missing: &MissingMap,
    init_mappings: KnownMap,
) -> Result<Vec<String>> {
    let DispatchCtx {
        source_repo,
        git_dir,
        cache,
        config,
        name,
        jobs,
        rule_source,
    } = ctx;
    let mut in_degree: HashMap<String, usize> =
        missing.keys().map(|k| (k.clone(), 0usize)).collect();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();

    for (sha, parents) in missing {
        for parent in parents {
            if missing.contains_key(parent) {
                *in_degree.get_mut(sha).unwrap() += 1;
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(sha.clone());
            }
        }
    }

    let mut ready: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(s, _)| s.clone())
        .collect();

    let shared: Arc<Mutex<KnownMap>> = Arc::new(Mutex::new(init_mappings));
    let semaphore = Arc::new(Semaphore::new(jobs.max(1)));
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<Option<String>>)>();
    let mut in_flight: usize = 0;
    let mut dropped_shas: Vec<String> = Vec::new();

    loop {
        while let Some(sha) = ready.front().cloned() {
            let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                break;
            };
            ready.pop_front();

            let target_parents: Vec<String> = {
                let m = shared.lock().unwrap();
                missing[&sha]
                    .iter()
                    .filter_map(|p| m.get(p).and_then(|v| v.clone()))
                    .collect()
            };
            let transform_ctx = TransformCtx {
                source_repo: source_repo.clone(),
                git_dir: git_dir.clone(),
                source_sha: sha.clone(),
                cache: cache.clone(),
                config: config.clone(),
                name: name.clone(),
                target_parents,
                rule_source,
            };
            let tx = done_tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    transform_commit(&transform_ctx)
                })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("worker panicked: {e}")));
                let _ = tx.send((sha, result));
            });
            in_flight += 1;
        }

        if in_flight == 0 {
            break;
        }

        let Some((sha, result)) = done_rx.recv().await else {
            break;
        };
        in_flight -= 1;

        let target_sha_opt = match result {
            Ok(opt) => opt,
            Err(e) => {
                while in_flight > 0 {
                    done_rx.recv().await;
                    in_flight -= 1;
                }
                return Err(e);
            }
        };

        if target_sha_opt.is_none() {
            dropped_shas.push(sha.clone());
        }
        shared.lock().unwrap().insert(sha.clone(), target_sha_opt);

        if let Some(child_shas) = children.get(&sha) {
            for child in child_shas {
                let d = in_degree.get_mut(child).unwrap();
                *d -= 1;
                if *d == 0 {
                    ready.push_back(child.clone());
                }
            }
        }
    }

    drop(done_tx);
    Ok(dropped_shas)
}

/// Transforms commits in parallel until the accumulated size of new objects
/// reaches `size_limit`, then drains in-flight work into spillover.
///
/// Returns:
/// - `chunk_shas`: mapped source SHAs to push in this round
/// - `chunk_dropped`: dropped source SHAs (Ok(None)) from this round
/// - `spillover_shas`: source SHAs already in the local cache, to seed the next round
/// - `remaining`: commits not yet started (for future rounds)
#[allow(clippy::too_many_arguments)]
async fn dispatch_size_chunk(
    ctx: DispatchCtx,
    missing: MissingMap,
    init_mappings: KnownMap,
    size_limit: u64,
    pushed_target_shas: &[String],
    spillover_counted: Vec<String>,
    spillover_accumulated: u64,
) -> Result<(Vec<String>, Vec<String>, Vec<String>, MissingMap)> {
    let DispatchCtx {
        source_repo,
        git_dir,
        cache,
        config,
        name,
        jobs,
        rule_source,
    } = ctx;

    let mut in_degree: HashMap<String, usize> =
        missing.keys().map(|k| (k.clone(), 0usize)).collect();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (sha, parents) in &missing {
        for parent in parents {
            if missing.contains_key(parent) {
                *in_degree.get_mut(sha).unwrap() += 1;
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(sha.clone());
            }
        }
    }

    let mut ready: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(s, _)| s.clone())
        .collect();

    let shared: Arc<Mutex<KnownMap>> = Arc::new(Mutex::new(init_mappings));
    let semaphore = Arc::new(Semaphore::new(jobs.max(1)));
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<Option<String>>)>();
    let mut in_flight: usize = 0;

    let mut accumulated: u64 = spillover_accumulated;
    let mut stop_spawning = false;
    let mut chunk_shas: Vec<String> = vec![];
    let mut chunk_dropped_shas: Vec<String> = vec![];
    let mut spillover_shas: Vec<String> = vec![];
    // Objects already accounted for — starts with previously pushed SHAs plus spillover.
    let mut counted_in_chunk: Vec<String> = {
        let mut v = pushed_target_shas.to_vec();
        v.extend(spillover_counted);
        v
    };

    loop {
        if !stop_spawning {
            while let Some(sha) = ready.front().cloned() {
                let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                    break;
                };
                ready.pop_front();

                let target_parents: Vec<String> = {
                    let m = shared.lock().unwrap();
                    missing[&sha]
                        .iter()
                        .filter_map(|p| m.get(p).and_then(|v| v.clone()))
                        .collect()
                };
                let transform_ctx = TransformCtx {
                    source_repo: source_repo.clone(),
                    git_dir: git_dir.clone(),
                    source_sha: sha.clone(),
                    cache: cache.clone(),
                    config: config.clone(),
                    name: name.clone(),
                    target_parents,
                    rule_source,
                };
                let tx = done_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        transform_commit(&transform_ctx)
                    })
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("worker panicked: {e}")));
                    let _ = tx.send((sha, result));
                });
                in_flight += 1;
            }
        }

        if in_flight == 0 {
            break;
        }

        let Some((sha, result)) = done_rx.recv().await else {
            break;
        };
        in_flight -= 1;

        let target_sha_opt = match result {
            Ok(opt) => opt,
            Err(e) => {
                while in_flight > 0 {
                    done_rx.recv().await;
                    in_flight -= 1;
                }
                return Err(e);
            }
        };

        shared
            .lock()
            .unwrap()
            .insert(sha.clone(), target_sha_opt.clone());

        if let Some(child_shas) = children.get(&sha) {
            for child in child_shas {
                let d = in_degree.get_mut(child).unwrap();
                *d -= 1;
                if *d == 0 {
                    ready.push_back(child.clone());
                }
            }
        }

        if stop_spawning {
            if target_sha_opt.is_some() {
                // Draining: mapped commit spills to the next round.
                spillover_shas.push(sha);
            } else {
                // Dropped during drain: same treatment as a normal drop.
                chunk_dropped_shas.push(sha);
            }
        } else if let Some(ref target_sha) = target_sha_opt {
            let size = loose_objects_size_delta(&cache.path, target_sha, &counted_in_chunk)?;
            counted_in_chunk.push(target_sha.clone());

            if accumulated + size >= size_limit && !chunk_shas.is_empty() {
                // Including this commit would exceed the limit and the chunk already
                // has at least one commit → spill this one to the next round.
                spillover_shas.push(sha);
                stop_spawning = true;
            } else {
                // Either under the limit, or the chunk is empty (best-effort: a single
                // oversized commit must be included regardless).
                accumulated += size;
                chunk_shas.push(sha);
                if accumulated >= size_limit {
                    stop_spawning = true;
                }
            }
        } else {
            // Dropped commit (Ok(None)): no objects to measure; track separately
            // so the caller can insert None into KnownMap.
            chunk_dropped_shas.push(sha);
        }
    }

    drop(done_tx);

    let processed: HashSet<&str> = chunk_shas
        .iter()
        .chain(chunk_dropped_shas.iter())
        .chain(spillover_shas.iter())
        .map(|s| s.as_str())
        .collect();
    let remaining: MissingMap = missing
        .into_iter()
        .filter(|(s, _)| !processed.contains(s.as_str()))
        .collect();

    Ok((chunk_shas, chunk_dropped_shas, spillover_shas, remaining))
}

// ── size measurement ──────────────────────────────────────────────────────────

fn loose_object_size(cache_path: &Path, sha: &str) -> u64 {
    let path = cache_path.join("objects").join(&sha[..2]).join(&sha[2..]);
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn loose_objects_size_delta(
    cache_path: &Path,
    target_sha: &str,
    already_counted: &[String],
) -> Result<u64> {
    let exclude: Vec<&str> = already_counted.iter().map(|s| s.as_str()).collect();
    let objects = git::rev_list_objects(cache_path, &[target_sha], &exclude)?;
    Ok(objects
        .iter()
        .map(|sha| loose_object_size(cache_path, sha))
        .sum())
}

fn loose_objects_size_delta_multi(
    cache_path: &Path,
    target_shas: &[String],
    already_counted: &[String],
) -> Result<u64> {
    if target_shas.is_empty() {
        return Ok(0);
    }
    let include: Vec<&str> = target_shas.iter().map(|s| s.as_str()).collect();
    let exclude: Vec<&str> = already_counted.iter().map(|s| s.as_str()).collect();
    let objects = git::rev_list_objects(cache_path, &include, &exclude)?;
    Ok(objects
        .iter()
        .map(|sha| loose_object_size(cache_path, sha))
        .sum())
}
