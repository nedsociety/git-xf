use std::path::Path;

use anyhow::{bail, Result};

use crate::cache::Cache;
use crate::config::Config;

pub fn run(git_dir: &Path, config: &Config, target_override: Option<&str>) -> Result<()> {
    if let Some(target) = target_override {
        if config.len() != 1 {
            bail!(
                "--target requires exactly one transformation in .git-transform.yaml, \
                 found {}",
                config.len()
            );
        }
        let (name, cfg) = config.iter().next().unwrap();
        let mut cfg = cfg.clone();
        cfg.target = target.to_string();
        let cache = Cache::new(git_dir, name);
        cache.ensure_initialized(&cfg.target)?;
        eprintln!("Initialized '{name}' → {}", cfg.target);
    } else {
        let mut names: Vec<&String> = config.keys().collect();
        names.sort();
        for name in names {
            let cfg = &config[name];
            let cache = Cache::new(git_dir, name);
            cache.ensure_initialized(&cfg.target)?;
            eprintln!("Initialized '{name}' → {}", cfg.target);
        }
    }
    Ok(())
}
