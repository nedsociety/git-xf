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

// ── RuleSource ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum RuleSource {
    /// Always use the rule from HEAD's .git-xf.yaml.
    Head,
    /// Read the rule from each source commit's .git-xf.yaml.
    /// If missing or unparseable, apply the `missing` policy.
    #[default]
    Commit,
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
    /// If true, populate the targetEnv directory with the first target parent
    /// commit's tree before running the command. Requires `targetEnv` to be set.
    /// Ignored (starts empty) when there is no parent (root commit).
    #[serde(default, rename = "copyParent")]
    pub copy_parent: bool,
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

/// What to do in `--rule=commit` mode when the per-commit rule is missing
/// (`.git-xf.yaml` absent, transformation block absent, or YAML parse error).
/// Has no effect in `--rule=head` mode.
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum MissingPolicy {
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
    pub missing: MissingPolicy,
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

/// Parses a `.git-xf.yaml` string and extracts the `rule` block for `name`.
///
/// Returns `Err` if the YAML is malformed, if the transformation block is absent,
/// or if the `rule` block itself cannot be deserialized.
pub fn parse_rule(yaml: &str, name: &str) -> Result<RuleConfig> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(yaml).context("failed to parse .git-xf.yaml")?;

    let entry = doc
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("transformation '{name}' not found in .git-xf.yaml"))?;

    let rule_value = entry
        .get("rule")
        .cloned()
        .unwrap_or(serde_yaml::Value::Null);

    let rule: RuleConfig = serde_yaml::from_value(rule_value)
        .with_context(|| format!("failed to parse rule block for transformation '{name}'"))?;

    if rule.output.is_some() && rule.target_env.is_some() {
        bail!("transformation '{name}': 'output' and 'targetEnv' cannot both be set");
    }
    if rule.copy_parent && rule.target_env.is_none() {
        bail!("transformation '{name}': 'copyParent' requires 'targetEnv' to be set");
    }

    Ok(rule)
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
        if cfg.rule.copy_parent && cfg.rule.target_env.is_none() {
            bail!(
                "transformation {:?}: 'copyParent' requires 'targetEnv' to be set",
                name
            );
        }
    }
    Ok(config)
}
