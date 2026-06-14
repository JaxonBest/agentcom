mod agent;
mod cli;
mod config;
mod hub;
mod ipc;
mod paths;
mod prompt;
mod protocol;
mod store;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { force, template } => {
            let cwd = std::env::current_dir()?;
            let path = config::write_example_template(&cwd, force, template)?;
            println!("wrote {}", path.display());
            println!("edit your agent fleet, then run: agentcom up");
            Ok(())
        }
        Command::Up {
            agents,
            tasks,
            headless,
            free,
            for_,
            budget,
            usage,
        } => {
            let free_mode = match (&free, &for_, &usage) {
                (Some(goal), _, _) => Some(config::FreeMode {
                    goal: goal.clone(),
                    duration: for_.as_deref().map(config::parse_duration).transpose()?,
                    usage_pct: usage,
                }),
                (None, Some(_), _) | (None, None, Some(_)) => {
                    anyhow::bail!("--for/--usage require --free \"<goal>\"")
                }
                _ => None,
            };
            run_up(agents, tasks, headless, free_mode, budget).await
        }
        Command::Agent(cmd) => cli::run_agent_cmd(cmd).await,
        Command::Doctor => run_doctor(),
        other => cli::run_client(other).await,
    }
}

async fn run_up(
    only_agents: Option<Vec<String>>,
    seed_tasks: Vec<String>,
    headless: bool,
    free_mode: Option<config::FreeMode>,
    budget_override: Option<f64>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    // If we're running straight out of the project's `target/` dir, agent (or
    // manual) rebuilds will relink the very binary we're executing and the
    // adapter exes we spawn children from — which kills the hub. Stage a copy
    // in a stable location and relaunch from there. This never returns when it
    // relaunches (it exits with the child's status).
    maybe_relaunch_from_stable(&project_root).await?;

    let mut cfg = config::HubConfig::load(&project_root)?;

    // Every fleet gets a composer: the coordinator the human talks to in
    // the chat pane. Define your own [[agent]] name = "composer" to
    // customize it; it sits outside the max_agents worker cap.
    if cfg.agent(config::COMPOSER_NAME).is_none() {
        cfg.agents
            .insert(0, config::composer_default(cfg.default_model.as_deref()));
        // max_agents is the WORKER cap — the injected composer gets its own
        // slot on top of it.
        cfg.max_agents = (cfg.max_agents + 1).max(cfg.agents.len());
    }

    if let Some(b) = budget_override {
        cfg.max_total_budget_usd = Some(b);
    }

    init_logging(&project_root, headless)?;

    // Refuse to double-start against a live hub.
    if let Ok(info) = ipc::client::read_hub_json(&paths::hub_json_path(&project_root)?) {
        if ipc::client::Client::connect_to(info.port, &info.token, "human")
            .await
            .is_ok()
        {
            anyhow::bail!(
                "a hub for this project is already running (pid {}) — stop it first with `agentcom stop`",
                info.pid
            );
        }
    }

    let mut hub = hub::Hub::new(cfg, project_root, free_mode).await?;

    for t in &seed_tasks {
        hub.store().task_add(t, "", 1, &[], "human")?;
    }

    hub.spawn_agents(only_agents.as_deref())?;

    if headless {
        println!("agentcom hub running (headless). Ctrl+C to stop.");
        hub.run().await
    } else {
        let ipc_tx = hub.ipc_tx.clone();
        let ui_tx = hub.ui_tx.clone();
        let buffers = hub.buffers();
        let store = hub.store();
        let agent_names: Vec<String> = hub.cfg.agents.iter().map(|a| a.name.clone()).collect();
        let project = hub.cfg.project_name.clone();

        let hub_task = tokio::spawn(async move { hub.run().await });
        let tui_result = tui::run(project, agent_names, ipc_tx, ui_tx, buffers, store).await;
        let hub_result = hub_task.await;
        tui_result?;
        // Distinguish a hub-task panic (JoinError) from a hub error so the
        // reason lands in the log rather than a torn-down terminal.
        match hub_result {
            Ok(inner) => inner?,
            Err(join_err) => {
                tracing::error!("hub task terminated abnormally: {join_err}");
                return Err(anyhow::anyhow!("hub task panicked: {join_err}"));
            }
        }
        Ok(())
    }
}

/// When the running hub binary lives inside the project's `target/` dir, copy
/// it (and its adapters) to a stable dir and relaunch from there, so rebuilds
/// of the project can't clobber the executables in use. Returns normally when
/// no relaunch is needed; otherwise it runs the staged copy to completion and
/// exits the process with the child's status.
async fn maybe_relaunch_from_stable(project_root: &std::path::Path) -> Result<()> {
    // Already relaunched, or the user opted out — run in place.
    if std::env::var_os("AGENTCOM_RELAUNCHED").is_some()
        || std::env::var_os("AGENTCOM_NO_RELAUNCH").is_some()
    {
        return Ok(());
    }
    let exe = std::env::current_exe().context("locating current exe")?;
    if !is_inside_build_output(&exe, project_root) {
        return Ok(());
    }

    let stable_dir = paths::bin_dir()?;
    let exe_name = exe
        .file_name()
        .context("current exe has no file name")?
        .to_owned();
    let dest_exe = stable_dir.join(&exe_name);

    if let Err(e) = stage_runtime_binaries(&exe, &stable_dir) {
        // Don't block startup — fall back to running in place, but warn loudly.
        eprintln!(
            "agentcom: could not stage a stable copy ({e:#}); running from build \
             output — a rebuild while this is running may crash the hub. \
             Stop the hub before rebuilding, or set AGENTCOM_NO_RELAUNCH=1 to silence this."
        );
        return Ok(());
    }

    eprintln!(
        "agentcom: started from build output ({}). Relaunching from {} so \
         rebuilds can't clobber the running hub…",
        exe.display(),
        dest_exe.display()
    );

    // Supervisor: run the staged copy attached to this console, and swallow
    // Ctrl+C here so only the child handles graceful shutdown.
    let mut child = tokio::process::Command::new(&dest_exe)
        .args(std::env::args_os().skip(1))
        .env("AGENTCOM_RELAUNCHED", "1")
        .spawn()
        .with_context(|| format!("relaunching {}", dest_exe.display()))?;
    loop {
        tokio::select! {
            status = child.wait() => {
                let code = status.ok().and_then(|s| s.code()).unwrap_or(0);
                std::process::exit(code);
            }
            // The child shares the console and gets the same Ctrl+C; let it run
            // its own shutdown. We just keep waiting.
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

/// True when `exe` lives under `<project_root>/target` (a Cargo build dir).
fn is_inside_build_output(exe: &std::path::Path, project_root: &std::path::Path) -> bool {
    let exe_c = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let target = project_root.join("target");
    let target_c = std::fs::canonicalize(&target).unwrap_or(target);
    exe_c.starts_with(&target_c)
}

/// Copy the hub binary and any sibling adapter exes into `stable_dir`.
fn stage_runtime_binaries(exe: &std::path::Path, stable_dir: &std::path::Path) -> Result<()> {
    let src_dir = exe.parent().context("current exe has no parent dir")?;
    let exe_name = exe.file_name().context("current exe has no file name")?;
    std::fs::copy(exe, stable_dir.join(exe_name))
        .with_context(|| format!("copying {} to {}", exe.display(), stable_dir.display()))?;

    let adapters: [&str; 2] = if cfg!(windows) {
        ["agentcom-codex-adapter.exe", "agentcom-deepseek-adapter.exe"]
    } else {
        ["agentcom-codex-adapter", "agentcom-deepseek-adapter"]
    };
    for adapter in adapters {
        let src = src_dir.join(adapter);
        if src.exists() {
            std::fs::copy(&src, stable_dir.join(adapter))
                .with_context(|| format!("copying adapter {}", src.display()))?;
        }
    }
    Ok(())
}

fn run_doctor() -> Result<()> {
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RED: &str = "\x1b[31m";
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    println!("{BOLD}agentcom doctor{RESET} — pre-flight check\n");

    let mut ok = 0u32;
    let mut warn = 0u32;
    let mut errors = 0u32;

    // 1. claude CLI
    match std::process::Command::new("claude")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  {GREEN}✓{RESET} claude CLI        {ver}");
            ok += 1;
        }
        _ => {
            println!("  {RED}✗{RESET} claude CLI        not found — install Claude Code");
            errors += 1;
        }
    }

    // 2. codex CLI (optional)
    match std::process::Command::new("codex")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  {GREEN}✓{RESET} codex CLI         {ver}");
            ok += 1;
        }
        _ => {
            println!("  {YELLOW}○{RESET} codex CLI         not found (optional — only needed for codex agents)");
            warn += 1;
        }
    }

    // 3. DEEPSEEK_API_KEY
    if std::env::var("DEEPSEEK_API_KEY").is_ok() {
        println!("  {GREEN}✓{RESET} DEEPSEEK_API_KEY  set");
        ok += 1;
    } else {
        println!("  {YELLOW}○{RESET} DEEPSEEK_API_KEY  not set (only needed for deepseek agents)");
        warn += 1;
    }

    // 4. OPENAI_API_KEY (used by codex)
    if std::env::var("OPENAI_API_KEY").is_ok() {
        println!("  {GREEN}✓{RESET} OPENAI_API_KEY    set");
        ok += 1;
    } else {
        println!("  {YELLOW}○{RESET} OPENAI_API_KEY    not set (only needed for codex agents)");
        warn += 1;
    }

    // 5. agentcom.toml — find and validate
    let cwd = std::env::current_dir()?;
    match paths::find_project_root(&cwd) {
        Some(root) => {
            let toml_path = root.join(paths::CONFIG_FILE);
            match config::HubConfig::load(&root) {
                Ok(cfg) => {
                    println!(
                        "  {GREEN}✓{RESET} agentcom.toml     {} ({} agent(s))",
                        toml_path.display(),
                        cfg.agents.len()
                    );
                    ok += 1;
                }
                Err(e) => {
                    println!(
                        "  {RED}✗{RESET} agentcom.toml     {} — parse error: {e}",
                        toml_path.display()
                    );
                    errors += 1;
                }
            }
        }
        None => {
            println!("  {RED}✗{RESET} agentcom.toml     not found — run `agentcom init`");
            errors += 1;
        }
    }

    println!();
    if errors == 0 {
        println!("{GREEN}All checks passed{RESET} ({ok} ok, {warn} optional)");
    } else {
        println!("{RED}{errors} error(s){RESET}  {ok} ok  {warn} optional/warning");
        std::process::exit(1);
    }

    Ok(())
}

fn init_logging(project_root: &std::path::Path, headless: bool) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let log_dir = paths::log_dir(project_root)?;
    let file_appender = tracing_appender::rolling::daily(log_dir, "hub.log");
    // Leak the guard: logging lives for the whole process.
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    Box::leak(Box::new(guard));

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("agentcom=info"))
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_filter(filter());

    if headless {
        tracing_subscriber::registry()
            .with(file_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(filter()),
            )
            .init();
    } else {
        // The TUI owns the terminal — file logging only.
        tracing_subscriber::registry().with(file_layer).init();
    }
    Ok(())
}
