//! Child `claude` process construction.

use crate::config::{AgentConfig, AgentProvider, HubConfig};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Resolve the claude executable once at hub startup.
/// `AGENTCOM_CLAUDE_EXE` overrides (used by tests to point at mock-claude).
pub fn resolve_claude_exe() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AGENTCOM_CLAUDE_EXE") {
        return Ok(PathBuf::from(p));
    }
    which::which("claude")
        .context("could not find `claude` on PATH — install Claude Code or set AGENTCOM_CLAUDE_EXE")
}

/// Resolve the Codex executable used by the adapter.
/// `AGENTCOM_CODEX_EXE` overrides the PATH lookup.
/// On macOS the desktop app bundle is preferred over the PATH codex because
/// it carries its own OAuth credentials and tends to be more up-to-date.
pub fn resolve_codex_exe() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AGENTCOM_CODEX_EXE") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = windows_codex_app_exe() {
        return Ok(p);
    }
    if let Some(p) = macos_codex_app_exe() {
        return Ok(p);
    }
    which::which("codex")
        .context("could not find `codex` on PATH — install Codex or set AGENTCOM_CODEX_EXE")
}

#[cfg(windows)]
fn windows_codex_app_exe() -> Option<PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)?
        .join("OpenAI")
        .join("Codex")
        .join("bin");
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(root).ok()? {
        let candidate = entry.ok()?.path().join("codex.exe");
        if candidate.exists() {
            matches.push(candidate);
        }
    }
    matches.sort();
    matches.pop()
}

#[cfg(not(windows))]
fn windows_codex_app_exe() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn macos_codex_app_exe() -> Option<PathBuf> {
    let p = PathBuf::from("/Applications/Codex.app/Contents/Resources/codex");
    p.exists().then_some(p)
}

#[cfg(not(target_os = "macos"))]
fn macos_codex_app_exe() -> Option<PathBuf> {
    None
}

/// Resolve the bundled adapter binary that makes Codex look like a persistent
/// agentcom child process.
pub fn resolve_codex_adapter_exe(agentcom_dir: Option<&Path>) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AGENTCOM_CODEX_ADAPTER_EXE") {
        return Ok(PathBuf::from(p));
    }
    let exe_name = if cfg!(windows) {
        "agentcom-codex-adapter.exe"
    } else {
        "agentcom-codex-adapter"
    };
    if let Some(dir) = agentcom_dir {
        let p = dir.join(exe_name);
        if p.exists() {
            return Ok(p);
        }
    }
    which::which(exe_name)
        .context("could not find `agentcom-codex-adapter` beside agentcom or on PATH")
}

/// Resolve the bundled adapter binary that talks to the DeepSeek API.
pub fn resolve_deepseek_adapter_exe(agentcom_dir: Option<&Path>) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AGENTCOM_DEEPSEEK_ADAPTER_EXE") {
        return Ok(PathBuf::from(p));
    }
    let exe_name = if cfg!(windows) {
        "agentcom-deepseek-adapter.exe"
    } else {
        "agentcom-deepseek-adapter"
    };
    if let Some(dir) = agentcom_dir {
        let p = dir.join(exe_name);
        if p.exists() {
            return Ok(p);
        }
    }
    which::which(exe_name)
        .context("could not find `agentcom-deepseek-adapter` beside agentcom or on PATH")
}

pub struct SpawnSpec<'a> {
    pub hub_cfg: &'a HubConfig,
    pub agent_cfg: &'a AgentConfig,
    pub claude_exe: &'a Path,
    pub codex_exe: &'a Path,
    pub codex_adapter_exe: &'a Path,
    pub deepseek_adapter_exe: &'a Path,
    pub project_root: &'a Path,
    pub session_id: &'a str,
    /// Resume a previous session instead of starting fresh (crash restart).
    pub resume_session: Option<&'a str>,
    pub system_prompt_append: &'a str,
    pub ipc_port: u16,
    pub ipc_token: &'a str,
    /// Directory containing the agentcom executable, prepended to the
    /// child's PATH so agents can run `agentcom <cmd>` from their Bash tool.
    pub agentcom_dir: Option<&'a Path>,
    /// Isolated Cargo target dir for agent builds. agentcom runs out of its
    /// own `target/` dir; without isolation an agent's `cargo build`/`test`
    /// relinks and locks the live hub's `agentcom.exe` and adapter binaries,
    /// taking the whole hub down. Exported as `CARGO_TARGET_DIR`.
    pub cargo_target_dir: Option<&'a Path>,
}

pub fn spawn_agent(spec: &SpawnSpec) -> Result<Child> {
    let cwd = spec.hub_cfg.agent_cwd(spec.agent_cfg, spec.project_root);
    std::fs::create_dir_all(&cwd)
        .with_context(|| format!("creating agent cwd {}", cwd.display()))?;

    let provider = spec.hub_cfg.agent_provider(spec.agent_cfg);
    if provider == AgentProvider::Codex {
        return spawn_codex_agent(spec, &cwd);
    }
    if provider == AgentProvider::Deepseek {
        return spawn_deepseek_agent(spec, &cwd);
    }

    // .cmd/.bat shims can't be CreateProcess'd directly on Windows.
    let is_shim = spec
        .claude_exe
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);

    let mut cmd = if is_shim {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(spec.claude_exe);
        c
    } else {
        Command::new(spec.claude_exe)
    };

    cmd.arg("-p")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg(&spec.agent_cfg.permission_mode)
        .arg("--append-system-prompt")
        .arg(spec.system_prompt_append);

    if spec.hub_cfg.partial_messages {
        cmd.arg("--include-partial-messages");
    }

    match spec.resume_session {
        Some(prev) => {
            cmd.arg("--resume").arg(prev);
        }
        None => {
            cmd.arg("--session-id").arg(spec.session_id);
        }
    }

    if let Some(model) = spec
        .agent_cfg
        .model
        .as_ref()
        .or(spec.hub_cfg.default_model.as_ref())
    {
        cmd.arg("--model").arg(model);
    }
    // Agents run headless — nobody can answer permission prompts, so any
    // tool not pre-approved here is auto-denied. Coordination through the
    // hub CLI must always work, even when the user pre-approves nothing.
    let mut allowed: Vec<String> = spec.agent_cfg.allowed_tools.clone().unwrap_or_default();
    if !allowed
        .iter()
        .any(|t| t == "Bash" || t.starts_with("Bash(agentcom"))
    {
        allowed.push("Bash(agentcom:*)".to_string());
    }
    cmd.arg("--allowedTools").arg(allowed.join(","));
    if let Some(max_turns) = spec.agent_cfg.max_turns_per_prompt {
        cmd.arg("--max-turns").arg(max_turns.to_string());
    }

    cmd.current_dir(&cwd)
        .env("AGENTCOM_PORT", spec.ipc_port.to_string())
        .env("AGENTCOM_TOKEN", spec.ipc_token)
        .env("AGENTCOM_AGENT", &spec.agent_cfg.name)
        .envs(&spec.agent_cfg.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_build_isolation(&mut cmd, spec);

    // Make sure `agentcom` is callable from the agent's Bash tool even if
    // the binary isn't on the global PATH.
    if let Some(dir) = spec.agentcom_dir {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<PathBuf> = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }

    // Keep console Ctrl+C in the hub's terminal from propagating to children;
    // shutdown is hub-orchestrated (close stdin -> grace -> tree kill).
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .with_context(|| format!("spawning claude for agent {:?}", spec.agent_cfg.name))
}

fn spawn_codex_agent(spec: &SpawnSpec, cwd: &Path) -> Result<Child> {
    let mut cmd = Command::new(spec.codex_adapter_exe);
    cmd.arg("--codex-exe")
        .arg(spec.codex_exe)
        .arg("--cwd")
        .arg(cwd)
        .arg("--session-id")
        .arg(spec.session_id)
        .arg("--system-prompt-file")
        .arg(write_temp_prompt(
            &spec.agent_cfg.name,
            spec.system_prompt_append,
        )?);

    if let Some(model) = spec
        .agent_cfg
        .model
        .as_ref()
        .or(spec.hub_cfg.default_model.as_ref())
    {
        cmd.arg("--model").arg(model);
    }
    if let Some(prev) = spec.resume_session {
        cmd.arg("--resume").arg(prev);
    }

    cmd.current_dir(cwd)
        .env("AGENTCOM_PORT", spec.ipc_port.to_string())
        .env("AGENTCOM_TOKEN", spec.ipc_token)
        .env("AGENTCOM_AGENT", &spec.agent_cfg.name)
        .envs(&spec.agent_cfg.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_build_isolation(&mut cmd, spec);

    if let Some(dir) = spec.agentcom_dir {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<PathBuf> = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }

    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .with_context(|| format!("spawning codex adapter for agent {:?}", spec.agent_cfg.name))
}

fn spawn_deepseek_agent(spec: &SpawnSpec, cwd: &Path) -> Result<Child> {
    let mut cmd = Command::new(spec.deepseek_adapter_exe);
    cmd.arg("--cwd")
        .arg(cwd)
        .arg("--session-id")
        .arg(spec.session_id)
        .arg("--system-prompt-file")
        .arg(write_temp_prompt(
            &spec.agent_cfg.name,
            spec.system_prompt_append,
        )?);

    if let Some(model) = spec
        .agent_cfg
        .model
        .as_ref()
        .or(spec.hub_cfg.default_model.as_ref())
    {
        cmd.arg("--model").arg(model);
    }
    if let Some(prev) = spec.resume_session {
        cmd.arg("--resume").arg(prev);
    }
    let mut allowed: Vec<String> = spec.agent_cfg.allowed_tools.clone().unwrap_or_default();
    if !allowed
        .iter()
        .any(|t| t == "Bash" || t.starts_with("Bash(agentcom"))
    {
        allowed.push("Bash(agentcom:*)".to_string());
    }
    cmd.arg("--allowed-tools").arg(allowed.join(","));
    if let Some(max_turns) = spec.agent_cfg.max_turns_per_prompt {
        cmd.arg("--max-turns").arg(max_turns.to_string());
    }

    cmd.current_dir(cwd)
        .env("AGENTCOM_PORT", spec.ipc_port.to_string())
        .env("AGENTCOM_TOKEN", spec.ipc_token)
        .env("AGENTCOM_AGENT", &spec.agent_cfg.name)
        .envs(&spec.agent_cfg.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_build_isolation(&mut cmd, spec);

    if let Some(dir) = spec.agentcom_dir {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<PathBuf> = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }

    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn().with_context(|| {
        format!(
            "spawning deepseek adapter for agent {:?}",
            spec.agent_cfg.name
        )
    })
}

/// Point an agent's `cargo` at an isolated target dir so building/testing the
/// project can never relink or lock the running hub's own binaries. Only set
/// when the caller provides a dir; respects an explicit user `CARGO_TARGET_DIR`
/// in the hub's own environment by not overriding it here (the hub passes
/// `None` in that case).
fn apply_build_isolation(cmd: &mut Command, spec: &SpawnSpec) {
    if let Some(dir) = spec.cargo_target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
}

fn write_temp_prompt(agent: &str, prompt: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "agentcom-{agent}-{}-system.txt",
        uuid::Uuid::new_v4()
    ));
    // Write owner-only so the system prompt (role, team config, goals) is not
    // visible to other OS users sharing the same /tmp directory.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating temp prompt for {agent}"))?;
        f.write_all(prompt.as_bytes())
            .with_context(|| format!("writing temp prompt for {agent}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, prompt)?;
    }
    Ok(path)
}

/// Kill a child and its entire descendant tree (claude spawns tool
/// subprocesses that `Child::kill` alone would orphan).
pub async fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    #[cfg(not(windows))]
    {
        // Kill the child process directly first (covers the always-present root).
        let _ = tokio::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        // Also try killing its process group (works when the child called
        // setpgid/setsid so its PGID == its own PID, which claude may do via
        // shell job-control). This is a no-op if no group with that PGID exists.
        let _ = tokio::process::Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}
