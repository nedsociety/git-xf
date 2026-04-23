use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

use crate::cache::Cache;
use crate::config::{Config, TransformConfig};
use crate::git;
use crate::transform::{transform_commit, TransformCtx};

pub async fn run(
    source_repo: &Path,
    git_dir: &Path,
    config: &Config,
    refs: &[String],
    dry_run: bool,
    jobs: usize,
) -> Result<()> {
    let effective_refs: Vec<String> = if refs.is_empty() {
        vec!["HEAD".to_string()]
    } else {
        refs.to_vec()
    };

    for (name, cfg) in config {
        sync_one(source_repo, git_dir, name, cfg, &effective_refs, dry_run, jobs)
            .await
            .with_context(|| format!("transformation '{name}' failed"))?;
    }
    Ok(())
}

// ── per-transformation sync ───────────────────────────────────────────────

async fn sync_one(
    source_repo: &Path,
    git_dir: &Path,
    name: &str,
    cfg: &TransformConfig,
    refs: &[String],
    dry_run: bool,
    jobs: usize,
) -> Result<()> {
    let cache = Arc::new(Cache::new(git_dir, name));
    cache.ensure_initialized(&cfg.target)?;
    cache.fetch_and_prune()?;

    let tip_shas = resolve_tips(source_repo, refs)?;

    // ── walk the DAG to find commits not yet in the cache ─────────────────
    let pre_mapped = cache.all_mappings()?;
    let (missing, init_mappings) = find_missing(source_repo, &pre_mapped, &tip_shas)?;

    if dry_run {
        if missing.is_empty() {
            return Ok(());
        }
        eprintln!("[{name}] would transform {} commit(s):", missing.len());
        // Print in topological order (oldest first) for readability.
        for sha in topo_order(&missing) {
            eprintln!("  {sha}");
        }
        return Ok(());
    }

    // ── parallel dispatch ─────────────────────────────────────────────────
    if !missing.is_empty() {
        dispatch(
            source_repo.to_path_buf(),
            git_dir.to_path_buf(),
            cache.clone(),
            Arc::new(cfg.clone()),
            name.to_string(),
            &missing,
            init_mappings,
            jobs,
        )
        .await?;
    }

    // ── mirror branches/tags from source whose tips are mapped in cache ───
    // Reload all mappings in one subprocess (dispatch may have added new ones).
    // This also runs when missing is empty, so a newly created branch pointing
    // to an already-mapped commit is propagated without transforming new commits.
    let post_mapped = cache.all_mappings()?;
    let mut refspecs: Vec<String> = missing
        .keys()
        .map(|sha| {
            let r = cache.mapping_ref(sha);
            format!("{r}:{r}")
        })
        .collect();
    for (refname, src_sha) in
        git::for_each_ref(source_repo, &["refs/heads/", "refs/tags/"])?
    {
        if let Some(target_sha) = post_mapped.get(&src_sha) {
            git::update_ref(&cache.path, &refname, target_sha)?;
            refspecs.push(format!("{refname}:{refname}"));
        }
    }
    git::push(&cache.path, &refspecs)?;

    if !missing.is_empty() {
        eprintln!("[{name}] synced {} commit(s)", missing.len());
    }
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────

fn resolve_tips(source_repo: &Path, refs: &[String]) -> Result<Vec<String>> {
    refs.iter()
        .map(|r| git::resolve_ref(source_repo, r))
        .collect()
}

/// BFS from `tips`, collecting commits not yet present in `cached`.
///
/// Returns:
/// - `missing`: source SHA → source parent SHAs (commits that need transforming)
/// - `known`:   source SHA → target SHA (commits already in the cache, used to
///              seed the mapping table for the parallel dispatch)
fn find_missing(
    source_repo: &Path,
    cached: &HashMap<String, String>,
    tips: &[String],
) -> Result<(HashMap<String, Vec<String>>, HashMap<String, String>)> {
    let mut missing: HashMap<String, Vec<String>> = HashMap::new();
    let mut known: HashMap<String, String> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    for tip in tips {
        if visited.insert(tip.clone()) {
            queue.push_back(tip.clone());
        }
    }

    while let Some(sha) = queue.pop_front() {
        if let Some(target_sha) = cached.get(&sha) {
            known.insert(sha, target_sha.clone());
            continue; // already done; don't recurse further
        }
        let info = git::commit_info(source_repo, &sha)?;
        for parent in &info.parents {
            if visited.insert(parent.clone()) {
                queue.push_back(parent.clone());
            }
        }
        missing.insert(sha, info.parents);
    }

    Ok((missing, known))
}

/// Returns `missing` keys in topological order (parents before children).
fn topo_order(missing: &HashMap<String, Vec<String>>) -> Vec<String> {
    // in_degree[sha] = number of sha's parents that are also in missing
    let mut in_degree: HashMap<&str, usize> =
        missing.keys().map(|k| (k.as_str(), 0)).collect();
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
    debug_assert_eq!(order.len(), missing.len(), "topo_order: incomplete — cycle in graph?");
    order
}

// ── parallel dispatch ─────────────────────────────────────────────────────

/// Transforms all commits in `missing` in parallel (bounded by `jobs`),
/// respecting topological order.
async fn dispatch(
    source_repo: PathBuf,
    git_dir: PathBuf,
    cache: Arc<Cache>,
    config: Arc<TransformConfig>,
    name: String,
    missing: &HashMap<String, Vec<String>>,
    init_mappings: HashMap<String, String>,
    jobs: usize,
) -> Result<()> {
    // Build Kahn's in-degree and parent→children maps (within the missing set).
    let mut in_degree: HashMap<String, usize> =
        missing.keys().map(|k| (k.clone(), 0usize)).collect();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();

    for (sha, parents) in missing {
        for parent in parents {
            if missing.contains_key(parent) {
                *in_degree.get_mut(sha).unwrap() += 1;
                children.entry(parent.clone()).or_default().push(sha.clone());
            }
        }
    }

    let mut ready: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(s, _)| s.clone())
        .collect();

    // Shared mapping table: pre-seeded with already-known target SHAs so
    // target_parents lookups work for commits whose parents were already in
    // the cache.
    let shared: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(init_mappings));
    let semaphore = Arc::new(Semaphore::new(jobs.max(1)));
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<String>)>();
    let mut in_flight: usize = 0;

    loop {
        // Greedily spawn every unblocked commit up to the semaphore limit.
        while let Some(sha) = ready.front().cloned() {
            let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                break; // all slots occupied; fall through to await a completion
            };
            ready.pop_front();

            let target_parents: Vec<String> = {
                let m = shared.lock().unwrap();
                missing[&sha]
                    .iter()
                    .filter_map(|p| m.get(p).cloned())
                    .collect()
            };
            let ctx = TransformCtx {
                source_repo: source_repo.clone(),
                git_dir: git_dir.clone(),
                source_sha: sha.clone(),
                cache: cache.clone(),
                config: config.clone(),
                name: name.clone(),
                target_parents,
            };
            let tx = done_tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit; // released when the blocking work finishes
                    transform_commit(&ctx)
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

        // Await the next completed commit.
        let Some((sha, result)) = done_rx.recv().await else {
            break;
        };
        in_flight -= 1;

        let target_sha = match result {
            Ok(sha) => sha,
            Err(e) => {
                // Drain remaining in-flight workers before returning the error
                // so their tokio tasks don't outlive this function.
                while in_flight > 0 {
                    done_rx.recv().await;
                    in_flight -= 1;
                }
                return Err(e);
            }
        };
        shared.lock().unwrap().insert(sha.clone(), target_sha);

        // Unblock children whose all missing parents are now done.
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
    Ok(())
}
