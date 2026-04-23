use std::path::{Path, PathBuf};

use anyhow::Result;

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
            return Ok(());
        }
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        git::clone_bare(target_url, &self.path)?;
        git::config_add(
            &self.path,
            "remote.origin.fetch",
            &format!("+refs/git-xf/{name}/*:refs/git-xf/{name}/*", name = self.name),
        )?;
        Ok(())
    }

    pub fn fetch_and_prune(&self) -> Result<()> {
        git::fetch(&self.path)?;
        git::worktree_prune(&self.path)?;
        Ok(())
    }

    pub fn mapping(&self, source_sha: &str) -> Result<Option<String>> {
        git::read_ref(&self.path, &self.mapping_ref(source_sha))
    }

    pub fn set_mapping(&self, source_sha: &str, target_sha: &str) -> Result<()> {
        git::update_ref(&self.path, &self.mapping_ref(source_sha), target_sha)
    }

    pub fn mapping_ref(&self, source_sha: &str) -> String {
        format!("refs/git-xf/{}/{}", self.name, source_sha)
    }
}
