use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

use crate::cache::Cache;
use crate::config::{Config, RuleSource, TransformConfig};
use crate::git;
use crate::transform::{transform_commit, TransformCtx};

pub async fn run(
    source_repo: &Path,
    git_dir: &Path,
    config: &Config,
    refs: &[String],
    dry_run: bool,
    jobs: usize,
    rule_source: RuleSource,
) -> Result<()> {
    let effective_refs: Vec<String> = if refs.is_empty() {
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
        )
        .await
        .with_context(|| format!("transformation '{name}' failed"))?;
    }
    Ok(())
}

// ── per-transformation sync ───────────────────────────────────────────────

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
) -> Result<()> {
    let cache = Arc::new(Cache::new(git_dir, name));
    cache.ensure_initialized(&cfg.target)?;
    cache.fetch_and_prune()?;

    let tip_shas = resolve_tips(source_repo, refs)?;

    let pre_mapped = cache.all_mappings()?;
    let (missing, init_mappings) = find_missing(source_repo, &pre_mapped, &tip_shas)?;

    if dry_run {
        if missing.is_empty() {
            return Ok(());
        }
        // Dry-run output is the primary result of the command → stdout.
        println!("[{name}] would transform {} commit(s):", missing.len());
        for sha in topo_order(&missing) {
            println!("  {sha}");
        }
        return Ok(());
    }

    if !missing.is_empty() {
        eprintln!("[{name}] transforming {} commit(s)...", missing.len());
        dispatch(
            DispatchCtx {
                source_repo: source_repo.to_path_buf(),
                git_dir: git_dir.to_path_buf(),
                cache: cache.clone(),
                config: Arc::new(cfg.clone()),
                name: name.to_string(),
                jobs,
                rule_source,
            },
            &missing,
            init_mappings,
        )
        .await?;
    }

    // Reload all mappings in one subprocess (dispatch may have added new ones).
    // This also runs when missing is empty, so a newly created branch pointing
    // to an already-mapped commit is propagated without transforming new commits.
    let post_mapped = cache.all_mappings()?;
    // Only push mapping refs for commits that got a real mapping (not dropped roots).
    let mut refspecs: Vec<String> = missing
        .keys()
        .filter(|sha| post_mapped.contains_key(*sha))
        .map(|sha| {
            let r = cache.mapping_ref(sha);
            format!("{r}:{r}")
        })
        .collect();
    for (refname, src_sha) in git::for_each_ref(source_repo, &["refs/heads/", "refs/tags/"])? {
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
/// Returns:
/// - `missing`: source SHA → source parent SHAs (commits that need transforming)
/// - `known`: source SHA → target SHA (commits already in the cache, used to
///   seed the mapping table for the parallel dispatch)
fn find_missing(
    source_repo: &Path,
    cached: &HashMap<String, String>,
    tips: &[String],
) -> Result<(MissingMap, KnownMap)> {
    let mut missing: HashMap<String, Vec<String>> = HashMap::new();
    let mut known: HashMap<String, Option<String>> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    for tip in tips {
        if visited.insert(tip.clone()) {
            queue.push_back(tip.clone());
        }
    }

    while let Some(sha) = queue.pop_front() {
        if let Some(target_sha) = cached.get(&sha) {
            known.insert(sha, Some(target_sha.clone()));
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

// ── parallel dispatch ─────────────────────────────────────────────────────

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
/// respecting topological order.
async fn dispatch(ctx: DispatchCtx, missing: &MissingMap, init_mappings: KnownMap) -> Result<()> {
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

    // Shared mapping table: pre-seeded with already-known target SHAs so
    // target_parents lookups work for commits whose parents were already in
    // the cache. `None` entries represent dropped root commits.
    let shared: Arc<Mutex<HashMap<String, Option<String>>>> = Arc::new(Mutex::new(init_mappings));
    let semaphore = Arc::new(Semaphore::new(jobs.max(1)));
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<Option<String>>)>();
    let mut in_flight: usize = 0;

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
            let ctx = TransformCtx {
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

        let Some((sha, result)) = done_rx.recv().await else {
            break;
        };
        in_flight -= 1;

        let target_sha_opt = match result {
            Ok(opt) => opt,
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
        // Insert `Some(sha)` for mapped commits and `None` for dropped roots.
        // Both states unblock dependent commits in the same way.
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
    Ok(())
}
