use std::path::Path;

use anyhow::Result;

use crate::cache::Cache;
use crate::config::Config;
use crate::git;

pub fn run(
    source_repo: &Path,
    git_dir: &Path,
    config: &Config,
    branch: Option<&str>,
) -> Result<()> {
    let branch_ref: String = match branch {
        Some(b) => b.to_string(),
        None => match git::current_branch(source_repo)? {
            Some(b) => b,
            None => "HEAD".to_string(),
        },
    };

    let commits = git::log_commits(source_repo, &branch_ref)?;

    if commits.is_empty() {
        println!("No commits on {branch_ref}.");
        return Ok(());
    }

    // Iterate transformations in deterministic alphabetical order.
    let mut names: Vec<&String> = config.keys().collect();
    names.sort();

    for name in names {
        let cfg = &config[name];
        let cache = Cache::new(git_dir, name);
        cache.ensure_initialized(&cfg.target)?;
        cache.fetch_and_prune()?;

        let mappings = cache.all_mappings()?;

        // Batch-read target commit subjects in one subprocess to detect error
        // commits without O(n) subprocess calls.
        let target_shas: Vec<&str> = commits
            .iter()
            .filter_map(|(sha, _)| mappings.get(sha).map(String::as_str))
            .collect();
        let target_subjects = git::commit_subjects(&cache.path, &target_shas)?;

        let mapped = mappings.len();
        let failed = target_subjects
            .values()
            .filter(|s| s.starts_with("[git-xf error]"))
            .count();
        let total = commits.len();

        println!(
            "{name}  →  {}  ({mapped}/{total} mapped, {failed} failed)",
            cfg.target
        );
        for (sha, subject) in &commits {
            let status = match mappings.get(sha) {
                None => "Pending",
                Some(target_sha) => {
                    if target_subjects
                        .get(target_sha)
                        .is_some_and(|s| s.starts_with("[git-xf error]"))
                    {
                        "Failed"
                    } else {
                        "Mapped"
                    }
                }
            };
            println!("  {}  {:<7}  {}", &sha[..8], status, subject);
        }
        println!();
    }

    Ok(())
}
