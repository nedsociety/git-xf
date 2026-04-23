use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RuleConfig {
    pub command: String,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum ChangelessPolicy {
    #[default]
    EmptyCommit,
    Skip,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum IgnoreErrorPolicy {
    #[default]
    Error,
    EmptyCommit,
    Skip,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TransformConfig {
    pub target: String,
    pub rule: RuleConfig,
    #[serde(default)]
    pub changeless: ChangelessPolicy,
    #[serde(default)]
    pub skip_commit_messages: Vec<String>,
    #[serde(default)]
    pub ignore_error: IgnoreErrorPolicy,
    #[serde(default)]
    pub branches: Vec<String>,
}

pub type Config = HashMap<String, TransformConfig>;

fn validate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn load(repo_root: &Path) -> Result<Config> {
    let path = repo_root.join(".git-xf.yaml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let config: Config =
        serde_yaml::from_str(&text).with_context(|| "failed to parse .git-xf.yaml")?;
    for name in config.keys() {
        if !validate_name(name) {
            bail!(
                "invalid transformation name {:?}: must match [a-zA-Z0-9_-]+",
                name
            );
        }
    }
    Ok(config)
}
