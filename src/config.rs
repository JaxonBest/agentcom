//! `agentcom.toml` — the only file agentcom keeps inside the project root.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Hub-wide cumulative spend cap in USD across all agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_budget_usd: Option<f64>,
    /// Emit `stream_event` partials from children (live TUI streaming).
    #[serde(default = "default_true")]
    pub partial_messages: bool,
    /// Seconds an agent may stay in Interrupting before the hub escalates
    /// to a tree-kill + resume.
    #[serde(default = "default_interrupt_timeout")]
    pub interrupt_timeout_secs: u64,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Unique handle other agents address messages to. `[a-z0-9_-]+`.
    pub name: String,
    /// Appended to the system prompt as this agent's role description.
    pub role: String,
    /// Working directory for the child process (relative paths resolve
    /// against the project root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// `--max-turns` per fed prompt; caps a single autonomous stretch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns_per_prompt: Option<u32>,
    /// Cumulative USD cap for this agent across the hub's lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
}

/// Shared by config validation and live `agent add`.
pub fn validate_agent_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!(
            "agent name {:?} is invalid (use lowercase letters, digits, '-', '_')",
            name
        );
    }
    if name == "all" || name == "human" || name == "hub" {
        bail!("agent name {:?} is reserved", name);
    }
    Ok(())
}

fn default_true() -> bool {
    true
}
fn default_permission_mode() -> String {
    "acceptEdits".into()
}
fn default_interrupt_timeout() -> u64 {
    15
}

impl HubConfig {
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(crate::paths::CONFIG_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: HubConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.agents.is_empty() {
            bail!("agentcom.toml defines no [[agent]] entries");
        }
        let mut seen = std::collections::HashSet::new();
        for a in &self.agents {
            validate_agent_name(&a.name)?;
            if !seen.insert(&a.name) {
                bail!("duplicate agent name {:?}", a.name);
            }
        }
        Ok(())
    }

    pub fn agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Resolve an agent's cwd against the project root.
    pub fn agent_cwd(&self, agent: &AgentConfig, project_root: &Path) -> PathBuf {
        match &agent.cwd {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => project_root.join(p),
            None => project_root.to_path_buf(),
        }
    }
}

pub const EXAMPLE_CONFIG: &str = r#"# agentcom configuration
# Define your agent fleet here. Run `agentcom up` to start it.

project_name = "my-project"

# Default model for agents that don't set one. Omit to use your `claude` default.
# default_model = "sonnet"

# Stop everything once total spend crosses this (USD).
# max_total_budget_usd = 20.0

# Seconds to wait for an interrupted agent to abort before force-killing it.
# interrupt_timeout_secs = 15

[[agent]]
name = "builder"
role = "Implements features and fixes. Owns src/. Coordinates with reviewer before large refactors."
# IMPORTANT: agents run headless, so nobody can answer permission prompts —
# any tool NOT listed here is auto-denied. (`agentcom` coordination commands
# are always allowed regardless.) Narrow Bash rules like "Bash(npm test:*)"
# work too.
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# cwd = "."                      # working dir, relative to this file
# model = "sonnet"
# permission_mode = "acceptEdits"  # or "plan", "default", "bypassPermissions"
# max_turns_per_prompt = 50
# max_budget_usd = 10.0
# auto_restart = true

[[agent]]
name = "reviewer"
role = "Reviews changes made by other agents, runs tests, and files follow-up tasks for problems found."
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
"#;

/// Append a new `[[agent]]` block to an existing agentcom.toml, preserving
/// the rest of the file (comments included). The combined file is re-parsed
/// and validated before anything is written.
pub fn append_agent(project_root: &Path, agent: &AgentConfig) -> Result<PathBuf> {
    #[derive(Serialize)]
    struct Wrap<'a> {
        agent: [&'a AgentConfig; 1],
    }
    let path = project_root.join(crate::paths::CONFIG_FILE);
    let mut text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&toml::to_string(&Wrap { agent: [agent] })?);

    let combined: HubConfig =
        toml::from_str(&text).context("config invalid after adding agent")?;
    combined.validate()?;
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn write_example(project_root: &Path, force: bool) -> Result<PathBuf> {
    let path = project_root.join(crate::paths::CONFIG_FILE);
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, EXAMPLE_CONFIG)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: HubConfig = toml::from_str(EXAMPLE_CONFIG).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].name, "builder");
        assert_eq!(cfg.agents[0].permission_mode, "acceptEdits");
        assert!(cfg.agents[0].auto_restart);
    }

    #[test]
    fn reserved_and_duplicate_names_rejected() {
        let bad = r#"
project_name = "x"
[[agent]]
name = "all"
role = "r"
"#;
        let cfg: HubConfig = toml::from_str(bad).unwrap();
        assert!(cfg.validate().is_err());

        let dup = r#"
project_name = "x"
[[agent]]
name = "a"
role = "r"
[[agent]]
name = "a"
role = "r"
"#;
        let cfg: HubConfig = toml::from_str(dup).unwrap();
        assert!(cfg.validate().is_err());
    }
}
