use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};

use crate::git;

pub struct Cache {
    pub path: PathBuf,
    pub name: String,
}

impl Cache {
    pub fn new(git_dir: &Path, name: &str) -> Self {
        Self {
            path: git_dir.join("git-xf").join(format!("{name}.git")),
            name: name.to_string(),
        }
    }

    pub fn ensure_initialized(&self, target_url: &str) -> Result<()> {
        if self.path.exists() {
            // Verify the directory is actually a valid bare git repo.
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(["rev-parse", "--git-dir"])
                .output()?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
                bail!(
                    "cache at {} exists but is not a valid git repository \
                     (delete it and re-run `git xf init`): {stderr}",
                    self.path.display()
                );
            }
            self.ensure_fetch_refspec()?;
            return Ok(());
        }
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        git::clone_bare(target_url, &self.path)?;
        self.ensure_fetch_refspec()?;
        Ok(())
    }

    /// Adds the mapping fetch refspec if it is not already configured.
    fn ensure_fetch_refspec(&self) -> Result<()> {
        let expected = format!(
            "+refs/git-xf/{name}/*:refs/git-xf/{name}/*",
            name = self.name
        );
        let existing = git::config_get_all(&self.path, "remote.origin.fetch")?;
        if !existing.iter().any(|v| v == &expected) {
            git::config_add(&self.path, "remote.origin.fetch", &expected)?;
        }
        Ok(())
    }

    pub fn fetch_and_prune(&self) -> Result<()> {
        git::fetch(&self.path)?;
        git::worktree_prune(&self.path)?;
        Ok(())
    }

    pub fn drop_blobs(&self) -> Result<()> {
        git::repack_drop_blobs(&self.path)
    }

    pub fn set_mapping(&self, source_sha: &str, target_sha: &str) -> Result<()> {
        git::update_ref(&self.path, &self.mapping_ref(source_sha), target_sha)
    }

    pub fn mapping_ref(&self, source_sha: &str) -> String {
        format!("refs/git-xf/{}/{}", self.name, source_sha)
    }

    /// Loads every cached mapping for this transformation in one subprocess.
    /// Returns `source_sha → target_sha`.
    pub fn all_mappings(&self) -> Result<HashMap<String, String>> {
        let prefix = format!("refs/git-xf/{}/", self.name);
        let refs = git::for_each_ref(&self.path, &[prefix.as_str()])?;
        Ok(refs
            .into_iter()
            .filter_map(|(refname, target_sha)| {
                refname
                    .strip_prefix(&prefix)
                    .map(|src_sha| (src_sha.to_string(), target_sha))
            })
            .collect())
    }
}
