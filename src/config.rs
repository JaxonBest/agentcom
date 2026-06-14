//! `agentcom.toml` — the only file agentcom keeps inside the project root.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    pub project_name: String,
    /// Default child runtime for agents: Claude Code, Codex, or DeepSeek.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<AgentProvider>,
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
    /// Hard cap on fleet size — agents may recruit teammates with
    /// `agentcom agent add`, and this is what stops a recruitment spiral.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
    /// When an agent releases file claims, auto-commit any changed files to
    /// git using the agent's name as the commit author.
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    /// Git author name to use for auto-commits (defaults to the agent's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit_author_name: Option<String>,
    /// Git author email to use for auto-commits (defaults to
    /// "<agent>@agentcom.local").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit_author_email: Option<String>,
    /// Skip pre-commit hooks when auto-committing (--no-verify). Off by
    /// default — hooks enforce project policy and should run unless you have a
    /// specific reason to bypass them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_commit_skip_hooks: bool,
    /// HTTP/HTTPS endpoint to POST hub events to (task done, agent crash, etc).
    /// Leave unset to disable webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// Optional secret for HMAC-SHA256 signing of webhook payloads.
    /// Sent as `X-Agentcom-Signature: sha256=<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    /// Glob patterns for files to skip during auto-commit (e.g. ["agentcom.toml", "*.lock"]).
    /// Defaults to ["agentcom.toml", ".agentcom/**"] to protect hub state files.
    #[serde(default = "default_commit_exclude_patterns", skip_serializing_if = "Vec::is_empty")]
    pub commit_exclude_patterns: Vec<String>,
    /// Automatically push to the remote after each auto-commit. Off by default.
    /// Requires the working tree to have a configured remote.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_push: bool,
    /// Warn (log + webhook) when an agent reaches this percentage of its
    /// max_budget_usd. Range 0–100. Default: 80.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_warn_pct: Option<f64>,
    /// Remote name to push to when `auto_push = true`. Defaults to "origin".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_push_remote: Option<String>,
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
    pub provider: Option<AgentProvider>,
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
    /// Override the global auto-commit author name for this agent.
    /// Falls back to the agent's name if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit_author_name: Option<String>,
    /// Override the global auto-commit author email for this agent.
    /// Falls back to the agent's email or "<agent>@agentcom.local" if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit_author_email: Option<String>,
    /// Per-agent override for auto_commit. When Some, takes precedence over the
    /// global HubConfig.auto_commit setting. Use false to opt this agent out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit: Option<bool>,
    /// Max API requests per minute for this agent. Hub skips feeding a new
    /// prompt if the agent has already completed this many turns in the last
    /// 60 seconds — preventing one agent from burning through quota too fast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rpm: Option<u32>,
    /// Extra environment variables injected into this agent's child process.
    /// Useful for per-agent API keys, debug flags, or tool configuration.
    /// Example: env = { ANTHROPIC_API_KEY = "sk-...", DEBUG = "1" }
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Optional kickoff message sent as the first user turn immediately after
    /// spawning. Lets you target a one-shot agent without waiting for the
    /// composer. Example: initial_prompt = "Fix the login bug in auth.rs."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    Claude,
    Codex,
    Deepseek,
}

impl std::fmt::Display for AgentProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentProvider::Claude => f.write_str("claude"),
            AgentProvider::Codex => f.write_str("codex"),
            AgentProvider::Deepseek => f.write_str("deepseek"),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            role: String::new(),
            cwd: None,
            provider: None,
            model: None,
            allowed_tools: None,
            permission_mode: default_permission_mode(),
            max_turns_per_prompt: None,
            max_budget_usd: None,
            auto_restart: true,
            auto_commit_author_name: None,
            auto_commit_author_email: None,
            auto_commit: None,
            max_rpm: None,
            env: BTreeMap::new(),
            initial_prompt: None,
        }
    }
}

pub const COMPOSER_NAME: &str = "composer";

/// Free mode: a standing goal the fleet keeps working toward until a
/// stopping condition fires. Whenever every agent goes idle, the hub nudges
/// the composer to generate the next round of work.
#[derive(Debug, Clone)]
pub struct FreeMode {
    pub goal: String,
    /// Wall-clock limit from hub start.
    pub duration: Option<std::time::Duration>,
    /// Stop when the 5-hour usage limit reaches this percentage (0-100).
    pub usage_pct: Option<f64>,
    /// If true, let running agents finish their current task before stopping
    /// (instead of killing them immediately). Idle agents are still stopped
    /// right away since they have no work in progress.
    pub finish_tasks: bool,
}

/// Parse "2h", "90m", "1h30m", "45s", or plain seconds ("3600").
pub fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim().to_lowercase();
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(std::time::Duration::from_secs(secs));
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut matched = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid duration {s:?}"))?;
            num.clear();
            total += match c {
                'h' => n * 3600,
                'm' => n * 60,
                's' => n,
                _ => bail!("invalid duration {s:?} (use e.g. 2h, 90m, 1h30m, 45s)"),
            };
            matched = true;
        }
    }
    if !num.is_empty() || !matched {
        bail!("invalid duration {s:?} (use e.g. 2h, 90m, 1h30m, 45s)");
    }
    Ok(std::time::Duration::from_secs(total))
}

/// The built-in coordinator. Injected by `agentcom up` when the config
/// doesn't define its own `[[agent]] name = "composer"`. It coordinates and
/// converses with the human; it does not edit code itself.
pub fn composer_default(default_model: Option<&str>) -> AgentConfig {
    AgentConfig {
        name: COMPOSER_NAME.to_string(),
        role: "Coordinator. You converse with the human, turn their goals into board tasks, \
               recruit and direct worker agents, prevent conflicting edits, and report progress. \
               You never edit code yourself."
            .to_string(),
        model: default_model.map(str::to_string),
        allowed_tools: Some(["Bash", "Read", "Glob", "Grep"].map(String::from).to_vec()),
        permission_mode: "acceptEdits".to_string(),
        max_turns_per_prompt: Some(30),
        ..Default::default()
    }
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
fn default_max_agents() -> usize {
    8
}
fn default_commit_exclude_patterns() -> Vec<String> {
    vec!["agentcom.toml".into(), ".agentcom/**".into()]
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
        if self.agents.len() > self.max_agents {
            bail!(
                "{} agents configured but max_agents = {}",
                self.agents.len(),
                self.max_agents
            );
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

    pub fn agent_provider(&self, agent: &AgentConfig) -> AgentProvider {
        agent
            .provider
            .or(self.default_provider)
            .unwrap_or(AgentProvider::Claude)
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

/// Fleet archetype for `agentcom init --template`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ConfigTemplate {
    /// Composer (auto-injected) + builder only — great for solo hacking.
    Solo,
    /// Composer + builder + reviewer — the recommended starting point.
    #[default]
    Team,
    /// Composer + builder + reviewer + DeepSeek junior — cost-efficient mixed fleet.
    Mixed,
}



/// Generate an `agentcom.toml` for the given project name and fleet archetype.
pub fn render_example_config(project_name: &str, template: ConfigTemplate) -> String {
    let header = format!(
        "# ============================================================\n\
         # agentcom.toml — {project_name}\n\
         # ============================================================\n\
         #\n\
         # GETTING STARTED\n\
         # ─────────────────────────────────────────────────────────────\n\
         # 1. Edit the [[agent]] entries below to describe your fleet.\n\
         # 2. Run `agentcom up` to launch the hub and agent fleet.\n\
         # 3. Chat with the auto-injected \"composer\" coordinator in the\n\
         #    TUI pane — it turns your goals into board tasks and\n\
         #    directs workers without ever editing code itself.\n\
         # 4. Seed the board early: `agentcom task add \"Fix login bug\"`\n\
         # 5. Monitor: `agentcom status`  |  `agentcom tail <agent>`\n\
         # ─────────────────────────────────────────────────────────────\n\
         \n\
         # ── GLOBAL SETTINGS ─────────────────────────────────────────\n\
         \n\
         # (required) Human-readable label shown in the TUI and status.\n\
         project_name = \"{project_name}\"\n\
         \n\
         # Default runtime for agents that don't set provider themselves.\n\
         # Values: \"claude\" | \"codex\" | \"deepseek\"    Default: \"claude\"\n\
         # default_provider = \"claude\"\n\
         \n\
         # Default model passed to agents that don't set model themselves.\n\
         # Omit to use each provider's own default (recommended).\n\
         # Values: any model string                   Default: (provider default)\n\
         # default_model = \"claude-sonnet-4-5\"\n\
         \n\
         # Hub-wide cumulative spend cap in USD. Hub shuts down once reached.\n\
         # Values: any positive float                 Default: (no cap)\n\
         # max_total_budget_usd = 20.0\n\
         \n\
         # Seconds to wait for an interrupted agent to abort gracefully\n\
         # before the hub escalates to a force-kill.\n\
         # Values: any positive integer               Default: 15\n\
         # interrupt_timeout_secs = 15\n\
         \n\
         # Hard cap on fleet size. Agents can recruit teammates with\n\
         # `agentcom agent add`; this cap (plus budgets) bounds that.\n\
         # Values: any positive integer               Default: 8\n\
         # max_agents = 8\n\
         \n\
         # Automatically push to the remote after each auto-commit.\n\
         # Values: true | false                       Default: false\n\
         # auto_push = false\n\
         # auto_push_remote = \"origin\"\n\
         \n\
         # ── AGENT FLEET ─────────────────────────────────────────────\n\
         #\n\
         # One [[agent]] block per worker. A \"composer\" coordinator is\n\
         # injected automatically (unless you define your own below).\n\
         # The composer talks to the human, files board tasks, and\n\
         # directs workers — it never edits code itself.\n\
         #\n\
         # Each agent gets agentcom CLI commands (task/send/files/inbox)\n\
         # automatically. Tools not in allowed_tools are auto-denied\n\
         # when agents run headless — keep the list explicit.\n\
         #\n"
    );

    let builder = r#"
[[agent]]
name = "builder"
role = "Implements features and fixes. Owns src/. Coordinates with reviewer before large refactors."
# Tools the agent may call — everything else is auto-denied.
# Values: list of tool names               Default: (all tools)
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# Working directory (relative to this file's location).
# Values: any path                         Default: (project root)
# cwd = "."
# Agent runtime (overrides default_provider).
# Values: "claude" | "codex" | "deepseek"  Default: (default_provider)
# provider = "claude"
# Model to use (overrides default_model).
# Values: any model string                 Default: (default_model)
# model = "claude-sonnet-4-5"
# Tool permission policy.
# Values: "acceptEdits" | "plan" | "default" | "bypassPermissions"
#                                          Default: "acceptEdits"
# permission_mode = "acceptEdits"
# Max turns per fed prompt (caps one autonomous stretch).
# Values: any positive integer             Default: (no cap)
# max_turns_per_prompt = 50
# Per-agent cumulative USD spend cap.
# Values: any positive float               Default: (no cap)
# max_budget_usd = 10.0
# Restart the agent automatically if it exits.
# Values: true | false                     Default: true
# auto_restart = true
# Extra environment variables injected into this agent's process.
# Useful for per-agent API keys, debug flags, etc.
# env = { ANTHROPIC_API_KEY = "sk-...", DEBUG = "1" }
"#;

    let reviewer = r#"
[[agent]]
name = "reviewer"
role = "Reviews changes made by other agents, runs tests, and files follow-up tasks for problems found."
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
"#;

    let deepseek_active = r#"
[[agent]]
name = "junior-developer"
role = "Given clear instructions to do large amounts of code that require little reasoning."
provider = "deepseek"
model = "deepseek-coder"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 5.0
"#;

    let deepseek_commented = r#"
# ── ADD A DEEPSEEK WORKER (uncomment to activate) ───────────
#
# [[agent]]
# name = "junior-developer"
# role = "Given clear instructions to do large amounts of code that require little reasoning."
# provider = "deepseek"
# model = "deepseek-coder"
# allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# max_budget_usd = 5.0
"#;

    let mut out = header;
    out.push_str(builder);
    match template {
        ConfigTemplate::Solo => {
            out.push_str(deepseek_commented);
        }
        ConfigTemplate::Team => {
            out.push_str(reviewer);
            out.push_str(deepseek_commented);
        }
        ConfigTemplate::Mixed => {
            out.push_str(reviewer);
            out.push_str(deepseek_active);
        }
    }
    out
}

/// Render agentcom.toml with a new `[[agent]]` block appended, preserving
/// the rest of the file (comments included). Validates the combined config
/// (duplicates, max_agents, names) without writing — callers persist the
/// returned text once any hub-side checks have also passed.
pub fn render_with_agent(project_root: &Path, agent: &AgentConfig) -> Result<(PathBuf, String)> {
    #[derive(Serialize)]
    struct Wrap<'a> {
        agent: [&'a AgentConfig; 1],
    }
    let path = project_root.join(crate::paths::CONFIG_FILE);
    let mut text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&toml::to_string(&Wrap { agent: [agent] })?);

    let combined: HubConfig = toml::from_str(&text).context("config invalid after adding agent")?;
    combined.validate()?;
    Ok((path, text))
}

/// Remove a named agent from agentcom.toml and re-write it. Comments in the
/// original file are lost (toml round-trip); all data is preserved.
pub fn remove_agent(project_root: &Path, name: &str) -> Result<()> {
    let path = project_root.join(crate::paths::CONFIG_FILE);
    let mut cfg = HubConfig::load(project_root)?;
    let before = cfg.agents.len();
    cfg.agents.retain(|a| a.name != name);
    if cfg.agents.len() == before {
        bail!("agent {name:?} not found in agentcom.toml");
    }
    cfg.validate()?;
    std::fs::write(&path, toml::to_string_pretty(&cfg)?)?;
    Ok(())
}

pub fn write_example_template(
    project_root: &Path,
    force: bool,
    template: ConfigTemplate,
) -> Result<PathBuf> {
    let path = project_root.join(crate::paths::CONFIG_FILE);
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project");
    std::fs::write(&path, render_example_config(project_name, template))?;
    Ok(path)
}


/// Walk a project tree (up to 3 levels deep, skipping `.git`/`target`/`node_modules`)
/// and return a concise one-line summary suitable for an AI prompt.
///
/// The summary includes the detected language/framework, project name (if found),
/// file count by top extensions, and a README preview (first 5 lines).
pub fn scan_project(root: &Path) -> String {
    let skip_dirs: &[&str] = &[".git", "target", "node_modules"];

    let mut ext_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut frameworks: Vec<String> = Vec::new();
    let mut project_name: Option<String> = None;
    let mut total_files: usize = 0;
    let mut readme_lines: Vec<String> = Vec::new();

    // Recursive walk helper (fn, not closure — no capture needed).
    #[allow(clippy::too_many_arguments)]
    fn walk(
        dir: &Path,
        depth: usize,
        max_depth: usize,
        skip_dirs: &[&str],
        ext_counts: &mut BTreeMap<String, usize>,
        frameworks: &mut Vec<String>,
        project_name: &mut Option<String>,
        total_files: &mut usize,
        readme_lines: &mut Vec<String>,
    ) {
        if depth > max_depth {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(t) => t,
                _ => continue,
            };
            let name = entry.file_name();
            let name_lower = name.to_string_lossy().to_lowercase();
            let path = entry.path();

            if ft.is_dir() {
                if skip_dirs.contains(&name_lower.as_str())
                    || skip_dirs.contains(&name.to_string_lossy().as_ref())
                {
                    continue;
                }
                walk(
                    &path,
                    depth + 1,
                    max_depth,
                    skip_dirs,
                    ext_counts,
                    frameworks,
                    project_name,
                    total_files,
                    readme_lines,
                );
            } else if ft.is_file() {
                *total_files += 1;

                // --- Detect language / framework from marker files ---
                match name_lower.as_str() {
                    "cargo.toml" => {
                        push_unique(frameworks, "Rust");
                        if project_name.is_none() {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                // Minimal TOML parse for [package].name
                                #[derive(Deserialize)]
                                struct Pkg {
                                    name: Option<String>,
                                }
                                #[derive(Deserialize)]
                                struct Ct {
                                    package: Option<Pkg>,
                                }
                                if let Ok(ct) = toml::from_str::<Ct>(&text) {
                                    if let Some(n) = ct.package.and_then(|p| p.name) {
                                        *project_name = Some(n);
                                    }
                                }
                            }
                        }
                    }
                    "package.json" => {
                        push_unique(frameworks, "JS/TS");
                        if project_name.is_none() {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                #[derive(Deserialize)]
                                struct Pj {
                                    name: Option<String>,
                                }
                                if let Ok(pj) = serde_json::from_str::<Pj>(&text) {
                                    if let Some(n) = pj.name {
                                        *project_name = Some(n);
                                    }
                                }
                            }
                        }
                    }
                    "pyproject.toml" | "setup.py" => {
                        push_unique(frameworks, "Python");
                    }
                    "go.mod" => {
                        push_unique(frameworks, "Go");
                    }
                    "gemfile" => {
                        push_unique(frameworks, "Ruby");
                    }
                    "pom.xml" => {
                        push_unique(frameworks, "Java");
                    }
                    "build.gradle" | "build.gradle.kts" => {
                        push_unique(frameworks, "Kotlin/Groovy");
                    }
                    "cmakelists.txt" => {
                        push_unique(frameworks, "C/C++ (CMake)");
                    }
                    "makefile" | "makefile.am" | "gnumakefile" => {
                        push_unique(frameworks, "C/C++");
                    }
                    _ => {}
                }

                // --- Check for README.md (any casing) ---
                if matches!(
                    name_lower.as_str(),
                    "readme.md" | "readme.markdown" | "readme.txt"
                ) && readme_lines.is_empty()
                {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        readme_lines.extend(
                            text.lines().take(5).map(|l| l.to_string()),
                        );
                    }
                }

                // --- Count extensions ---
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if !ext_str.is_empty() {
                        *ext_counts.entry(ext_str).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    walk(
        root,
        0,
        3,
        skip_dirs,
        &mut ext_counts,
        &mut frameworks,
        &mut project_name,
        &mut total_files,
        &mut readme_lines,
    );

    // --- Assemble summary ---
    let lang = if frameworks.is_empty() {
        "Unknown".to_string()
    } else {
        frameworks.join("/")
    };

    let name_part = project_name
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();

    // Sort extensions by count descending, take top 7
    let mut ext_vec: Vec<(String, usize)> = ext_counts.into_iter().collect();
    ext_vec.sort_by_key(|b| std::cmp::Reverse(b.1));
    ext_vec.truncate(7);

    let ext_summary: String = ext_vec
        .iter()
        .map(|(ext, count)| format!("~{} .{ext} files", count))
        .collect::<Vec<_>>()
        .join(", ");

    let readme_part = if !readme_lines.is_empty() {
        let preview = readme_lines.join(" | ");
        if preview.len() > 120 {
            format!("README.md: {}…", &preview[..117])
        } else {
            format!("README.md: {preview}")
        }
    } else {
        "No README.md found".to_string()
    };

    if total_files == 0 {
        return format!("Empty project{name_part}. {readme_part}");
    }

    let summary = format!(
        "{lang} project{name_part}. ~{total_files} files: {ext_summary}. {readme_part}"
    );

    // Enforce 500-char limit
    if summary.len() > 500 {
        let trimmed: String = summary.chars().take(497).collect();
        format!("{}…", trimmed)
    } else {
        summary
    }
}

/// Push `val` into `vec` only if not already present.
fn push_unique(vec: &mut Vec<String>, val: &str) {
    if !vec.iter().any(|v| v == val) {
        vec.push(val.to_string());
    }
}

/// Set a config value in agentcom.toml using `toml_edit` for non-destructive
/// edits (preserving comments and formatting).
///
/// Supports top-level keys (`project_name`, `auto_commit`, etc.) and nested
/// agent fields via dot notation (`agent.<name>.model`).
///
/// The value string is auto-parsed as boolean, integer, float, or string.
pub fn config_set(project_root: &Path, key: &str, value: &str) -> Result<()> {
    let path = project_root.join(crate::paths::CONFIG_FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() || parts[0].is_empty() {
        bail!("key cannot be empty");
    }

    let toml_value = infer_toml_value(value);

    if parts.len() == 1 {
        doc[parts[0]] = toml_edit::Item::Value(toml_value);
    } else if parts[0] == "agent" && parts.len() >= 3 {
        let agent_name = parts[1];
        let field_parts = &parts[2..];

        let agents = doc
            .get_mut("agent")
            .and_then(|e| e.as_array_of_tables_mut())
            .ok_or_else(|| anyhow::anyhow!("no [[agent]] tables found in config"))?;

        let agent_table = agents
            .iter_mut()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(agent_name))
            .ok_or_else(|| anyhow::anyhow!("agent {agent_name:?} not found in agentcom.toml"))?;

        set_nested_table(agent_table, field_parts, &toml_value)?;
    } else {
        bail!("unsupported key path {key:?} — use top-level keys or 'agent.<name>.<field>'");
    }

    let new_text = doc.to_string();
    // Parse-validate the new TOML to catch schema errors before writing.
    let _: HubConfig = toml::from_str(&new_text)
        .context("config invalid after set — rejecting write")?;

    std::fs::write(&path, &new_text)
        .with_context(|| format!("writing {}", path.display()))?;
    println!("  set {key} = {value}");
    Ok(())
}

/// Recursively set a value at `parts` path within a `toml_edit::Table`.
fn set_nested_table(table: &mut toml_edit::Table, parts: &[&str], value: &toml_edit::Value) -> Result<()> {
    match parts {
        [] => bail!("empty field path"),
        [key] => {
            table[*key] = toml_edit::Item::Value(value.clone());
            Ok(())
        }
        [key, rest @ ..] => {
            if !table.contains_key(*key) {
                table.insert(*key, toml_edit::table());
            }
            let sub = table[*key]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("expected a table at key {key:?}"))?;
            set_nested_table(sub, rest, value)
        }
    }
}

/// Infer a `toml_edit::Value` from a string, auto-detecting booleans,
/// integers, floats, or falling back to a string.
fn infer_toml_value(s: &str) -> toml_edit::Value {
    if let Ok(b) = s.parse::<bool>() {
        return toml_edit::Value::from(b);
    }
    if let Ok(i) = s.parse::<i64>() {
        return toml_edit::Value::from(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return toml_edit::Value::from(f);
    }
    toml_edit::Value::from(s)
}

#[cfg(test)]
mod config_set_tests {
    use super::*;
    use std::fs;

    #[test]
    fn infer_value_types() {
        // Booleans
        assert!(infer_toml_value("true").as_bool() == Some(true));
        assert!(infer_toml_value("false").as_bool() == Some(false));

        // Integers
        assert!(infer_toml_value("42").as_integer() == Some(42));
        assert!(infer_toml_value("-1").as_integer() == Some(-1));

        // Floats — use a value that isn't an approximation of a named constant
        let f = infer_toml_value("3.14");
        #[allow(clippy::approx_constant)]
        let expected = 3.14_f64;
        assert!((f.as_float().unwrap() - expected).abs() < 1e-10);

        // Strings
        assert!(infer_toml_value("hello").as_str() == Some("hello"));
        assert!(infer_toml_value("true_story").as_str() == Some("true_story"));
    }

    #[test]
    fn set_top_level_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let toml_path = root.join("agentcom.toml");

        let base = r#"
project_name = "my-app"
auto_commit = true
max_agents = 8
"#;
        fs::write(&toml_path, base).unwrap();

        config_set(root, "project_name", "renamed-app").unwrap();
        config_set(root, "auto_commit", "false").unwrap();
        config_set(root, "max_agents", "16").unwrap();

        let result = fs::read_to_string(&toml_path).unwrap();
        let cfg: HubConfig = toml::from_str(&result).unwrap();
        assert_eq!(cfg.project_name, "renamed-app");
        assert!(!cfg.auto_commit);
        assert_eq!(cfg.max_agents, 16);
    }

    #[test]
    fn set_agent_field() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let toml_path = root.join("agentcom.toml");

        let base = r#"
project_name = "my-app"
[[agent]]
name = "builder"
role = "Implements features"
model = "claude-sonnet-4-5"
"#;
        fs::write(&toml_path, base).unwrap();

        config_set(root, "agent.builder.model", "claude-sonnet-4-6").unwrap();

        let result = fs::read_to_string(&toml_path).unwrap();
        let cfg: HubConfig = toml::from_str(&result).unwrap();
        assert_eq!(cfg.agents[0].model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn set_throws_on_nonexistent_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let toml_path = root.join("agentcom.toml");

        let base = r#"
project_name = "my-app"
[[agent]]
name = "builder"
role = "Implements features"
"#;
        fs::write(&toml_path, base).unwrap();

        let err = config_set(root, "agent.nobody.model", "something").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not found"), "expected 'not found' error, got: {msg}");
    }

    #[test]
    fn set_env_var_for_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let toml_path = root.join("agentcom.toml");

        let base = r#"
project_name = "my-app"
[[agent]]
name = "builder"
role = "Implements features"
[agent.env]
EXISTING = "old"
"#;
        fs::write(&toml_path, base).unwrap();

        config_set(root, "agent.builder.env.NEWKEY", "newvalue").unwrap();

        let result = fs::read_to_string(&toml_path).unwrap();
        let cfg: HubConfig = toml::from_str(&result).unwrap();
        assert_eq!(cfg.agents[0].env.get("NEWKEY").map(String::as_str), Some("newvalue"));
        assert_eq!(cfg.agents[0].env.get("EXISTING").map(String::as_str), Some("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse() {
        use std::time::Duration;
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("90m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("3600").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("2x").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn example_config_parses_and_validates() {
        let text = render_example_config("test-project", ConfigTemplate::Team);
        let cfg: HubConfig = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].name, "builder");
        assert_eq!(cfg.agents[0].permission_mode, "acceptEdits");
        assert!(cfg.agents[0].auto_restart);
    }

    #[test]
    fn render_example_config_solo() {
        let text = render_example_config("cool-proj", ConfigTemplate::Solo);
        assert!(text.contains("project_name = \"cool-proj\""));
        let cfg: HubConfig = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].name, "builder");
    }

    #[test]
    fn render_example_config_team() {
        let text = render_example_config("cool-proj", ConfigTemplate::Team);
        assert!(text.contains("project_name = \"cool-proj\""));
        let cfg: HubConfig = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[1].name, "reviewer");
    }

    #[test]
    fn render_example_config_mixed() {
        let text = render_example_config("cool-proj", ConfigTemplate::Mixed);
        assert!(text.contains("project_name = \"cool-proj\""));
        let cfg: HubConfig = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 3);
        assert_eq!(cfg.agents[2].name, "junior-developer");
        assert_eq!(
            cfg.agents[2].provider,
            Some(AgentProvider::Deepseek)
        );
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

    #[test]
    fn scan_project_detects_rust() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create Cargo.toml with a project name
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();

        // Create src/ with a couple of .rs files
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}").unwrap();

        // Create a .rs build file to test extension counting
        std::fs::write(root.join("build.rs"), "fn main() {}").unwrap();

        let summary = scan_project(root);
        assert!(
            summary.contains("Rust"),
            "Expected 'Rust' in summary, got: {summary}"
        );
        assert!(
            summary.contains("test-crate"),
            "Expected 'test-crate' in summary, got: {summary}"
        );
        assert!(
            summary.contains("~3 .rs files"),
            "Expected ~3 .rs files in summary, got: {summary}"
        );
        assert!(
            summary.contains("No README.md found"),
            "Expected 'No README.md found' in summary, got: {summary}"
        );
    }

    #[test]
    fn auto_push_defaults_false() {
        let toml = r#"
project_name = "x"
[[agent]]
name = "worker"
role = "does things"
"#;
        let cfg: HubConfig = toml::from_str(toml).unwrap();
        assert!(!cfg.auto_push);
        assert!(cfg.auto_push_remote.is_none());
    }

    #[test]
    fn auto_push_remote_configurable() {
        let toml = r#"
project_name = "x"
auto_push = true
auto_push_remote = "upstream"
[[agent]]
name = "worker"
role = "does things"
"#;
        let cfg: HubConfig = toml::from_str(toml).unwrap();
        assert!(cfg.auto_push);
        assert_eq!(cfg.auto_push_remote.as_deref(), Some("upstream"));
    }

    #[test]
    fn agent_env_field_roundtrips() {
        let toml = r#"
project_name = "x"
[[agent]]
name = "worker"
role = "does things"
[agent.env]
MY_KEY = "hello"
ANOTHER = "world"
"#;
        let cfg: HubConfig = toml::from_str(toml).unwrap();
        cfg.validate().unwrap();
        let env = &cfg.agents[0].env;
        assert_eq!(env.get("MY_KEY").map(String::as_str), Some("hello"));
        assert_eq!(env.get("ANOTHER").map(String::as_str), Some("world"));

        // Re-serialize and re-parse to confirm roundtrip.
        let text = toml::to_string(&cfg).unwrap();
        let cfg2: HubConfig = toml::from_str(&text).unwrap();
        assert_eq!(cfg2.agents[0].env, cfg.agents[0].env);
    }
}
