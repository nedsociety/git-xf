use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

// ── OutputSpec ────────────────────────────────────────────────────────────────

/// Normalized `(source_path, target_path)` pairs from the `output` field.
///
/// An empty vec means "copy nothing". Absent/null is represented as `None` at
/// the `Option<OutputSpec>` level in `RuleConfig` and means "whole worktree".
#[derive(Debug, Clone, Default)]
pub struct OutputSpec(Vec<(String, String)>);

impl OutputSpec {
    pub fn paths(&self) -> &[(String, String)] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for OutputSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> serde::de::Visitor<'de> for V {
            type Value = OutputSpec;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a string, a list of strings, or a map of strings")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<OutputSpec, E> {
                Ok(OutputSpec(vec![parse_path_pair(v)]))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<OutputSpec, A::Error> {
                let mut paths = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    paths.push(parse_path_pair(&s));
                }
                Ok(OutputSpec(paths))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<OutputSpec, A::Error> {
                let mut paths = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, String>()? {
                    paths.push((k, v));
                }
                Ok(OutputSpec(paths))
            }
        }

        d.deserialize_any(V)
    }
}

/// Parses `"src:dst"` → `("src", "dst")` and `"src"` → `("src", "src")`.
fn parse_path_pair(s: &str) -> (String, String) {
    match s.split_once(':') {
        Some((src, dst)) => (src.to_string(), dst.to_string()),
        None => (s.to_string(), s.to_string()),
    }
}

fn default_shell() -> String {
    "sh".to_string()
}

// ── Rule config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RuleConfig {
    pub command: String,
    /// Shell used to run `command`. `"sh"` → `sh -c $command`;
    /// anything else → `/usr/bin/env $shell -c $command`.
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Output-mode: paths to copy from source worktree into the target commit.
    /// Absent or null means copy the entire source worktree.
    /// Each entry: `"src[:dst]"`, a list of such strings, or a `{src: dst}` map.
    /// Mutually exclusive with `target_env` — even `output: []` is rejected.
    #[serde(default)]
    pub output: Option<OutputSpec>,
    /// Build-your-own-target mode: name of the env var seeded with a fresh
    /// empty directory that `command` should populate. Mutually exclusive with `output`.
    #[serde(rename = "targetEnv")]
    pub target_env: Option<String>,
}

// ── Policies ──────────────────────────────────────────────────────────────────

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

// ── TransformConfig ───────────────────────────────────────────────────────────

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

// ── Loading ───────────────────────────────────────────────────────────────────

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
    for (name, cfg) in &config {
        if !validate_name(name) {
            bail!(
                "invalid transformation name {:?}: must match [a-zA-Z0-9_-]+",
                name
            );
        }
        if cfg.rule.output.is_some() && cfg.rule.target_env.is_some() {
            bail!(
                "transformation {:?}: 'output' and 'targetEnv' cannot both be set",
                name
            );
        }
    }
    Ok(config)
}
