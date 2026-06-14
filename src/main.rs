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
    let _ = cli::JSON_MODE.set(cli.json);
    match cli.command {
        Command::Init { force, template, analyze } => {
            let cwd = std::env::current_dir()?;
            if analyze {
                let summary = config::scan_project(&cwd);
                println!("detected: {summary}");
                let prompt = format!(
                    "You are configuring agentcom, a multi-agent CLI framework. \
                     Given this project: {summary}\n\n\
                     Output ONLY a valid agentcom.toml config (no markdown, no explanation). \
                     Choose appropriate agent roles (composer, builder, reviewer etc.) \
                     and set project_name. Use model claude-sonnet-4-6 for all agents."
                );
                let out = std::process::Command::new("claude")
                    .args(["-p", &prompt])
                    .output()
                    .context("claude not found — install Claude Code to use --analyze")?;
                let toml_str = String::from_utf8_lossy(&out.stdout);
                let dest = cwd.join(crate::paths::CONFIG_FILE);
                if dest.exists() && !force {
                    anyhow::bail!("{} already exists (use --force to overwrite)", dest.display());
                }
                if toml::from_str::<toml::Value>(&toml_str).is_ok() {
                    config::write_config_file(&dest, &toml_str)?;
                    println!("wrote {} (AI-generated)", dest.display());
                } else {
                    eprintln!("warning: AI output was not valid TOML — falling back to template");
                    let path = config::write_example_template(&cwd, force, template)?;
                    println!("wrote {}", path.display());
                }
            } else {
                let path = config::write_example_template(&cwd, force, template)?;
                println!("wrote {}", path.display());
            }
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
            finish_tasks,
            restart,
        } => {
            let free_mode = match (&free, &for_, &usage) {
                (Some(goal), _, _) => Some(config::FreeMode {
                    goal: goal.clone(),
                    duration: for_.as_deref().map(config::parse_duration).transpose()?,
                    usage_pct: usage,
                    finish_tasks,
                }),
                (None, Some(_), _) | (None, None, Some(_)) => {
                    anyhow::bail!("--for/--usage require --free \"<goal>\"")
                }
                _ => {
                    if finish_tasks {
                        anyhow::bail!("--finish-tasks has no effect without --free")
                    }
                    None
                }
            };
            run_up(agents, tasks, headless, free_mode, budget, restart).await
        }
        Command::Agent(cli::AgentCmd::Budget { agent, json }) => run_agent_budget(agent, json),
        Command::Agent(cli::AgentCmd::Capabilities { name }) => run_agent_capabilities(name),
        Command::Agent(cli::AgentCmd::Inspect { name }) => run_agent_inspect(name).await,
        Command::Agent(cmd) => cli::run_agent_cmd(cmd).await,
        Command::Doctor => run_doctor().await,
        Command::Check => run_check(),
        Command::Logs { agent, lines, follow } => run_logs(agent, lines, follow),
        Command::Completions { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(shell, &mut Cli::command(), "agentcom", &mut std::io::stdout());
            Ok(())
        }
        Command::Budget => run_budget(),
        Command::Messages { from, to, count, json } => run_messages(from, to, count, json),
        Command::Replay { lines, agent } => run_replay(Some(lines), agent),
        Command::Config(cli::ConfigCmd::Show) => {
            let cwd = std::env::current_dir()?;
            let project_root = paths::find_project_root(&cwd)
                .context("no agentcom.toml found — run `agentcom init` first")?;
            let cfg = config::HubConfig::load(&project_root)?;
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
        Command::Config(cli::ConfigCmd::Set { key, value }) => {
            let cwd = std::env::current_dir()?;
            let project_root = paths::find_project_root(&cwd)
                .context("no agentcom.toml found — run `agentcom init` first")?;
            config::config_set(&project_root, &key, &value)?;
            Ok(())
        }
        Command::Version => {
            println!("agentcom {}", env!("CARGO_PKG_VERSION"));
            println!("git commit:  {}", option_env!("GIT_HASH").unwrap_or("unknown"));
            println!("build time:  {}", option_env!("BUILD_TIME").unwrap_or("unknown"));
            println!("rustc:       {}", option_env!("RUSTC_VERSION").unwrap_or("unknown"));
            Ok(())
        }
        Command::Task(cli::TaskCmd::Export { format, output }) => run_task_export(&format, output.as_deref()),
        Command::Task(cli::TaskCmd::Import { file }) => run_task_import(&file),
        Command::Task(cli::TaskCmd::Stats { json }) => run_task_stats(json),
        Command::Task(cli::TaskCmd::Watch { id: Some(task_id), interval, .. }) => {
            run_task_watch_single(task_id, interval.unwrap_or(5))
        }
        Command::Task(cli::TaskCmd::Watch { id: None, no_color, interval }) => {
            run_task_watch_board(no_color, interval.unwrap_or(2))
        }
        Command::Task(cli::TaskCmd::Trace { id }) => run_task_trace(id),
        Command::Task(cli::TaskCmd::Deps { id }) => run_task_deps(id),
        Command::Task(cli::TaskCmd::Graph) => run_task_graph(),
        Command::Task(cli::TaskCmd::Due { id, date, clear }) => run_task_due(id, date, clear).await,
        Command::Task(cli::TaskCmd::BulkDone { ids, note }) => run_bulk_done(ids, note).await,
        Command::Task(cli::TaskCmd::BulkClaim { ids }) => run_bulk_claim(ids).await,
        Command::Task(cli::TaskCmd::SaveTemplate { id, name }) => {
            run_task_save_template(id, name).await
        }
        Command::Task(cli::TaskCmd::FromTemplate { name, title }) => {
            run_task_from_template(name, title).await
        }
        Command::Task(cli::TaskCmd::Add { title, description, priority, depends_on, timeout, requires, nl: true }) => {
            run_task_add_nl(title, description, priority, depends_on, timeout, requires).await
        }
        Command::Metrics { agent, json } => run_metrics(agent, json),
        Command::Summary { json } => run_summary(json),
        Command::Snapshot { file } => run_snapshot(file),
        Command::Restore { file } => run_restore(file),
        Command::Audit { event, agent, since, count, json } => run_audit(event, agent, since, count, json),
        Command::Context { file, agent } => run_context_push(file, agent).await,
        Command::Preflight { verbose } => run_preflight(verbose),
        Command::Cost { agent, json } => run_cost(agent, json),
        other => cli::run_client(other).await,
    }
}

async fn run_up(
    only_agents: Option<Vec<String>>,
    seed_tasks: Vec<String>,
    headless: bool,
    free_mode: Option<config::FreeMode>,
    budget_override: Option<f64>,
    restart: bool,
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

    // --restart: stop a running hub before starting fresh
    if restart {
        if let Ok(info) = ipc::client::read_hub_json(&paths::hub_json_path(&project_root)?) {
            if let Ok(mut client) = ipc::client::Client::connect_to(
                info.port,
                &info.token,
                "human",
            )
            .await
            {
                println!("stopping running hub (pid {})...", info.pid);
                let _ = client.request(&crate::ipc::Request::Stop { agent: None }).await;
                // Wait up to 5s for the hub to shut down
                for _ in 0..50 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if ipc::client::read_hub_json(&paths::hub_json_path(&project_root)?).is_err()
                    {
                        break;
                    }
                }
                // Fall through — if the hub.json is still there the new hub will
                // overwrite it.
            }
        }
    }

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

/// Load, validate, and print a summary of agentcom.toml.
/// Exits with code 0 if valid, 1 if invalid.
fn run_check() -> Result<()> {
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    let cwd = std::env::current_dir()?;
    let project_root = match paths::find_project_root(&cwd) {
        Some(r) => r,
        None => {
            eprintln!("{BOLD}agentcom check{RESET} — FAIL");
            eprintln!("  no agentcom.toml found — run `agentcom init` first");
            std::process::exit(1);
        }
    };

    let cfg = match config::HubConfig::load(&project_root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{BOLD}agentcom check{RESET} — FAIL");
            eprintln!("  config error: {e:#}");
            std::process::exit(1);
        }
    };

    println!("{BOLD}agentcom check{RESET} — OK");
    println!("  project:     {}", cfg.project_name);
    println!("  agents:      {} configured", cfg.agents.len());
    println!("  max_agents:  {}", cfg.max_agents);
    println!("  auto_commit: {}", if cfg.auto_commit { "enabled" } else { "disabled" });
    if let Some(max) = cfg.max_total_budget_usd {
        println!("  max budget:  ${max:.2}");
    } else {
        println!("  max budget:  (unlimited)");
    }
    println!();

    // Per-agent summary
    for a in &cfg.agents {
        let provider = cfg
            .agent_provider(a)
            .to_string();
        let model = a.model.as_deref().unwrap_or("(default)");
        let budget = a
            .max_budget_usd
            .map(|b| format!("${b:.2}"))
            .unwrap_or_else(|| "(unlimited)".to_string());
        let tools = a
            .allowed_tools
            .as_ref()
            .map(|t| t.join(","))
            .unwrap_or_else(|| "(all)".to_string());
        println!("  agent  {:<15} provider={:<8} model={:<20} budget={:<12} tools={}", a.name, provider, model, budget, tools);
    }
    println!();

    // Warnings
    let mut warnings: Vec<String> = Vec::new();
    for a in &cfg.agents {
        if a.allowed_tools.is_none() {
            warnings.push(format!(
                "agent {:?} has no allowed_tools set — all tools available",
                a.name
            ));
        }
        if a.max_budget_usd.is_none() {
            warnings.push(format!(
                "agent {:?} has no max_budget_usd — no per-agent spend cap",
                a.name
            ));
        }
        if a.max_rpm.is_none() {
            warnings.push(format!(
                "agent {:?} has no max_rpm — no per-agent rate limit",
                a.name
            ));
        }
    }
    if cfg.agents.len() > cfg.max_agents / 2 {
        warnings.push(format!(
            "fleet at {}/{} capacity — consider increasing max_agents",
            cfg.agents.len(),
            cfg.max_agents
        ));
    }

    if warnings.is_empty() {
        println!("  {BOLD}no warnings{RESET}");
    } else {
        for w in &warnings {
            println!("  ⚠  {w}");
        }
    }

    Ok(())
}

async fn run_doctor() -> Result<()> {
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

    // 2. codex CLI (optional) — prefer the desktop app bundle via resolve_codex_exe()
    match agent::spawn::resolve_codex_exe()
        .ok()
        .and_then(|exe| std::process::Command::new(exe).arg("--version").output().ok())
        .filter(|o| o.status.success())
    {
        Some(out) => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  {GREEN}✓{RESET} codex CLI         {ver}");
            ok += 1;
        }
        None => {
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

    // 4. codex auth: OPENAI_API_KEY or ~/.codex/auth.json OAuth tokens
    let has_api_key = std::env::var("OPENAI_API_KEY").is_ok();
    let has_oauth = codex_oauth_credentials_present();
    if has_api_key {
        println!("  {GREEN}✓{RESET} OPENAI_API_KEY    set");
        ok += 1;
    } else if has_oauth {
        println!("  {GREEN}✓{RESET} codex auth         ~/.codex/auth.json (OAuth)");
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

            // 6. hub.json — detect running or stale hub
            if let Ok(hub_json_path) = paths::hub_json_path(&root) {
                if hub_json_path.exists() {
                    if let Ok(info) = ipc::client::read_hub_json(&hub_json_path) {
                        match ipc::client::Client::connect_to(
                            info.port,
                            &info.token,
                            "human",
                        ).await {
                            Ok(_) => {
                                println!("  {GREEN}✓{RESET} hub               running (pid {})", info.pid);
                                ok += 1;
                            }
                            Err(_) => {
                                println!(
                                    "  {YELLOW}!{RESET} hub               stale hub.json found (hub crashed or was killed) — run `agentcom stop` to clean it up"
                                );
                                warn += 1;
                            }
                        }
                    }
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

/// Returns true if ~/.codex/auth.json exists and contains non-null OAuth tokens,
/// meaning the Codex desktop app is authenticated without needing OPENAI_API_KEY.
fn codex_oauth_credentials_present() -> bool {
    let path = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".codex").join("auth.json"))
        .filter(|p| p.exists());
    let Some(path) = path else { return false };
    let Ok(contents) = std::fs::read_to_string(path) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents) else { return false };
    v.get("tokens")
        .and_then(|t| t.get("access_token"))
        .map(|t| !t.is_null())
        .unwrap_or(false)
}

fn run_logs(agent_filter: Option<String>, lines: usize, follow: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    let log_dir = paths::log_dir(&project_root)?;

    // Collect hub.log* files sorted by mtime ascending (oldest first) so we
    // can read them in order and take the last N lines across all files.
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = std::fs::read_dir(&log_dir)
        .with_context(|| format!("reading log dir {}", log_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("hub.log"))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .collect();

    if entries.is_empty() {
        anyhow::bail!("no hub log files found in {}", log_dir.display());
    }

    // Oldest first so we can stream them in order and take last N overall.
    entries.sort_by_key(|a| a.1);

    let filter = agent_filter.as_deref().map(|s| s.to_lowercase());

    let line_matches = |line: &str| -> bool {
        match &filter {
            Some(f) => line.to_lowercase().contains(f.as_str()),
            None => true,
        }
    };

    // Read all files in chronological order, collect matching lines, then
    // show the last N. This handles daily-rotated files transparently.
    let mut all_lines: Vec<String> = Vec::new();
    for (path, _) in &entries {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        for line in content.lines() {
            if line_matches(line) {
                all_lines.push(line.to_owned());
            }
        }
    }

    let start = all_lines.len().saturating_sub(lines);
    for line in &all_lines[start..] {
        println!("{line}");
    }

    if follow {
        // Follow the most-recent file (last in ascending-mtime sort).
        let mut current_path = entries.last().unwrap().0.clone();
        let mut pos = std::fs::metadata(&current_path)
            .map(|m| m.len())
            .unwrap_or(0);

        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));

            // Check for a newer file (daily rollover).
            let newest = most_recent_hub_log(&log_dir);
            if let Some(ref new_path) = newest {
                if new_path != &current_path {
                    current_path = new_path.clone();
                    pos = 0;
                }
            }

            let new_len = std::fs::metadata(&current_path)
                .map(|m| m.len())
                .unwrap_or(pos);
            if new_len > pos {
                use std::io::{Read, Seek};
                let mut file = std::fs::File::open(&current_path)
                    .with_context(|| format!("opening {}", current_path.display()))?;
                file.seek(std::io::SeekFrom::Start(pos))?;
                let mut buf = String::new();
                file.read_to_string(&mut buf)?;
                pos = new_len;
                for line in buf.lines() {
                    if line_matches(line) {
                        println!("{line}");
                    }
                }
            }
        }
    }

    Ok(())
}

fn most_recent_hub_log(log_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(log_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("hub.log"))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}

fn run_budget() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first to generate spend data");
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut stmt = conn.prepare(
        "SELECT agent, SUM(cost_usd), SUM(turns) FROM runs GROUP BY agent ORDER BY SUM(cost_usd) DESC",
    )?;
    let rows: Vec<(String, f64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        println!("no spend data yet");
        return Ok(());
    }
    println!("{:<14} {:>10}  {:>8}", "AGENT", "COST (USD)", "TURNS");
    println!("{}", "-".repeat(36));
    let (mut total_cost, mut total_turns) = (0f64, 0i64);
    for (agent, cost, turns) in &rows {
        println!("{:<14} ${:>9.4}  {:>8}", agent, cost, turns);
        total_cost += cost;
        total_turns += turns;
    }
    println!("{}", "-".repeat(36));
    println!("{:<14} ${:>9.4}  {:>8}", "TOTAL", total_cost, total_turns);
    Ok(())
}

fn run_agent_budget(agent: Option<String>, json_out: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first to generate spend data");
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let mut rows: Vec<(String, f64, i64, i64)> = Vec::new();
    if let Some(ref name) = agent {
        let mut stmt = conn.prepare(
            "SELECT agent, SUM(cost_usd), SUM(turns),
                    SUM(COALESCE(ended_at, strftime('%s','now')) - started_at)
             FROM runs WHERE agent = ?1 GROUP BY agent",
        )?;
        for row in stmt.query_map(rusqlite::params![name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })? {
            rows.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT agent, SUM(cost_usd), SUM(turns),
                    SUM(COALESCE(ended_at, strftime('%s','now')) - started_at)
             FROM runs GROUP BY agent ORDER BY SUM(cost_usd) DESC",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))? {
            rows.push(row?);
        }
    }

    if rows.is_empty() {
        if json_out {
            println!("[]");
        } else {
            println!("no spend data yet");
        }
        return Ok(());
    }

    #[derive(serde::Serialize)]
    struct Row {
        agent: String,
        total_usd: f64,
        turns: i64,
        cost_per_turn: f64,
        active_secs: i64,
        burn_rate_usd_per_hour: f64,
    }
    let data: Vec<Row> = rows
        .into_iter()
        .map(|(agent, cost, turns, active_secs)| {
            let cost_per_turn = if turns > 0 { cost / turns as f64 } else { 0.0 };
            let burn_rate = if active_secs > 0 {
                cost / (active_secs as f64 / 3600.0)
            } else {
                0.0
            };
            Row { agent, total_usd: cost, turns, cost_per_turn, active_secs, burn_rate_usd_per_hour: burn_rate }
        })
        .collect();

    if json_out {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    println!(
        "{:<14} {:>10}  {:>8}  {:>12}  {:>14}",
        "AGENT", "COST (USD)", "TURNS", "COST/TURN", "BURN $/hr"
    );
    println!("{}", "-".repeat(66));
    for r in &data {
        let active_h = r.active_secs as f64 / 3600.0;
        println!(
            "{:<14} ${:>9.4}  {:>8}  ${:>11.4}  ${:>13.4}  ({:.1}h active)",
            r.agent, r.total_usd, r.turns, r.cost_per_turn, r.burn_rate_usd_per_hour, active_h
        );
    }
    Ok(())
}

fn run_agent_capabilities(name: String) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let cfg = config::HubConfig::load(&project_root)?;
    let agent = cfg.agents.iter().find(|a| a.name == name)
        .ok_or_else(|| anyhow::anyhow!("agent '{name}' not found in agentcom.toml"))?;
    let caps = agent.capabilities.clone();
    if caps.is_empty() {
        println!("{name}: no capabilities declared");
    } else {
        println!("{name} capabilities: {}", caps.join(", "));
    }
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        println!("(no hub database — cannot check task requires)");
        return Ok(());
    }
    let store = store::Store::open(&db)?;
    let tasks = store.task_list(None, None)?;
    let open_with_req: Vec<_> = tasks.iter()
        .filter(|t| t.status == store::TaskStatus::Open && !t.requires.is_empty())
        .collect();
    if open_with_req.is_empty() {
        println!("no open tasks have requires constraints");
        return Ok(());
    }
    println!("\n{:<6} {:<40} {:<16} {}", "ID", "TITLE", "REQUIRES", "QUALIFIES");
    println!("{}", "-".repeat(80));
    for t in &open_with_req {
        let qualifies = t.requires.iter().all(|r| caps.contains(r));
        let missing: Vec<_> = t.requires.iter().filter(|r| !caps.contains(*r)).collect();
        let qual_str = if qualifies {
            "yes".to_string()
        } else {
            format!("no (missing: {})", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","))
        };
        println!("{:<6} {:<40} {:<16} {}",
            t.id,
            &t.title[..t.title.len().min(38)],
            t.requires.join(","),
            qual_str);
    }
    Ok(())
}

async fn run_agent_inspect(name: String) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let cfg = config::HubConfig::load(&project_root)?;

    // --- Config section ---
    println!("=== agent: {name} ===");
    if let Some(agent) = cfg.agents.iter().find(|a| a.name == name) {
        println!("  model:        {}", agent.model.as_deref().unwrap_or("<inherited>"));
        println!("  provider:     {}", agent.provider.as_ref().map(|p| p.to_string()).unwrap_or_else(|| "<inherited>".into()));
        if let Some(b) = agent.max_budget_usd {
            println!("  budget:       ${b:.2}");
        }
        if !agent.capabilities.is_empty() {
            println!("  capabilities: {}", agent.capabilities.join(", "));
        }
        if let Some(ref g) = agent.idle_goal {
            println!("  idle_goal:    {g}");
        }
        if !agent.env.is_empty() {
            let keys: Vec<_> = agent.env.keys().collect();
            println!("  env_vars:     {}", keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", "));
        }
    } else {
        println!("  (not found in agentcom.toml — may be a dynamic agent)");
    }

    // --- Runtime state via IPC (best-effort) ---
    println!("\n--- runtime ---");
    if let Ok(mut client) = ipc::client::Client::connect().await {
        if let Ok(resp) = client.request(&ipc::Request::Status).await {
            if let ipc::Response::Status { agents, .. } = resp {
                if let Some(row) = agents.iter().find(|a| a.name == name) {
                    println!("  state:   {}", row.state);
                    println!("  turns:   {}", row.turns);
                    println!("  cost:    ${:.4}", row.spent_usd);
                    if let Some(ref d) = row.detail {
                        println!("  detail:  {d}");
                    }
                } else {
                    println!("  (agent not currently running)");
                }
            }
        } else {
            println!("  (hub running but status unavailable)");
        }
    } else {
        println!("  (hub not running)");
    }

    // --- DB section ---
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        return Ok(());
    }
    let conn = rusqlite::Connection::open(&db)?;

    // Last 10 activity entries by this agent
    println!("\n--- recent activity (last 10) ---");
    let mut stmt = conn.prepare(
        "SELECT ta.created_at, t.id, t.title, ta.body \
         FROM task_activity ta JOIN tasks t ON ta.task_id = t.id \
         WHERE ta.agent = ?1 ORDER BY ta.created_at DESC LIMIT 10"
    )?;
    let entries: Vec<(i64, i64, String, String)> = stmt.query_map([&name], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?.filter_map(|r| r.ok()).collect();

    if entries.is_empty() {
        println!("  (none)");
    } else {
        for (ts, task_id, title, body) in entries.iter().rev() {
            let time = fmt_unix_ts(*ts);
            let snippet = if body.len() > 60 { &body[..60] } else { body.as_str() };
            println!("  [{time}] #{task_id} {}: {snippet}", &title[..title.len().min(30)]);
        }
    }

    // Held file claims
    println!("\n--- held files ---");
    let mut stmt2 = conn.prepare(
        "SELECT path FROM file_claims WHERE agent = ?1 ORDER BY path"
    )?;
    let files: Vec<String> = stmt2.query_map([&name], |r| r.get(0))?.filter_map(|r| r.ok()).collect();
    if files.is_empty() {
        println!("  (none)");
    } else {
        for f in &files {
            println!("  {f}");
        }
    }

    Ok(())
}

fn run_messages(from: Option<String>, to: Option<String>, count: usize, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;
    let messages = store.msg_list(from.as_deref(), to.as_deref(), count)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
        return Ok(());
    }
    if messages.is_empty() {
        println!("no messages");
        return Ok(());
    }
    for m in &messages {
        let urgent = if m.urgent { " [URGENT]" } else { "" };
        let status = if m.delivered { "" } else { " (undelivered)" };
        println!("{} -> {}{}{}", m.from_who, m.to_who, urgent, status);
        for line in m.body.lines() {
            println!("  {}", line);
        }
        println!();
    }
    Ok(())
}

fn run_replay(lines: Option<usize>, agent_filter: Option<String>) -> Result<()> {
    /// Reconstruct a human-readable session narrative from hub logs.
    const RESET: &str = "\x1b[0m";
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RED: &str = "\x1b[31m";
    const BLUE: &str = "\x1b[34m";
    const CYAN: &str = "\x1b[36m";

    /// A single parsed log event.
    struct LogEvent {
        timestamp: String,
        _level: String,
        message: String,
    }

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    let log_dir = paths::log_dir(&project_root)?;

    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = std::fs::read_dir(&log_dir)
        .with_context(|| format!("reading log dir {}", log_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("hub.log"))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .collect();

    if entries.is_empty() {
        anyhow::bail!("no hub log files found in {}", log_dir.display());
    }

    entries.sort_by_key(|a| a.1);

    // Regex to parse the default tracing-subscriber format:
    // 2025-01-01T12:00:00.123456Z  INFO agentcom::hub: message
    // or with file/line:
    // 2025-01-01T12:00:00.123456Z  INFO agentcom::hub: message
    // The level is 5 chars wide, padded with spaces.
    let log_re = regex::Regex::new(
        r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+(?:Z|[+-]\d{2}:\d{2}))\s+(\w+)\s+(\S+(?:::\S+)*):\s+(.+)$"
    ).expect("valid regex");

    let filter = agent_filter.as_deref().map(|s| s.to_lowercase());

    // Parse all log files into a flat vec of events.
    let mut events: Vec<LogEvent> = Vec::new();
    for (path, _) in &entries {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        for line in content.lines() {
            if let Some(caps) = log_re.captures(line) {
                let msg = caps[4].to_string();
                // Apply agent filter (check if agent name appears in message)
                if let Some(ref f) = filter {
                    if !msg.to_lowercase().contains(f.as_str()) {
                        continue;
                    }
                }
                events.push(LogEvent {
                    timestamp: caps[1].to_string(),
                    _level: caps[2].to_string(),
                    message: msg,
                });
            }
        }
    }

    // Apply line limit (most recent N events)
    if let Some(n) = lines {
        if events.len() > n {
            events = events.split_off(events.len() - n);
        }
    }

    if events.is_empty() {
        println!("no matching log events found");
        return Ok(());
    }

    // --- Render markdown narrative ---
    println!("# agentcom replay — Session Narrative\n");
    println!(
        "> {} events from {} to {}\n",
        events.len(),
        events.first().unwrap().timestamp,
        events.last().unwrap().timestamp
    );

    // Categorize events for a richer narrative.
    let mut section_open = false;

    for ev in &events {
        let msg = &ev.message;
        let ts_short = &ev.timestamp[..19]; // "2025-01-01T12:00:00"

        // Determine event category for styling
        let (icon, label, style) = if msg.contains(": stopped") || msg.contains("Hub stop") {
            ("🛑", "STOP", RED)
        } else if msg.contains("ctrl-c received") || msg.contains("SIGTERM received") {
            ("🛑", "SHUTDOWN", RED)
        } else if msg.contains("crashed") || msg.contains("exit code") {
            ("💥", "CRASH", RED)
        } else if msg.contains("Hub start") || msg.contains("auto-restarting") {
            ("🚀", "START", GREEN)
        } else if msg.contains("added task #") || msg.contains("cloned task #") {
            ("📋", "TASK", CYAN)
        } else if msg.contains("claimed task #") {
            ("👤", "CLAIM", BLUE)
        } else if msg.contains("completed task #") {
            ("✅", "DONE", GREEN)
        } else if msg.contains("blocked task #") {
            ("🚧", "BLOCKED", YELLOW)
        } else if msg.contains("released") && msg.contains("claimed task") {
            ("↩️", "RELEASE", YELLOW)
        } else if msg.contains("free mode") {
            ("🎯", "FREE", CYAN)
        } else if msg.contains("budget warning") {
            ("💰", "BUDGET", YELLOW)
        } else if msg.contains("assigned task #") {
            ("📩", "ASSIGN", BLUE)
        } else if msg.contains("->") && (msg.contains("from") || msg.contains("to")) {
            ("💬", "MSG", BLUE)
        } else if msg.starts_with("[FREE MODE]") {
            ("🎯", "NUDGE", CYAN)
        } else if msg.contains("spawn") {
            ("🤖", "SPAWN", GREEN)
        } else {
            ("•", "INFO", "")
        };

        // Print section header for new sessions/hubs
        if msg.contains("Hub start") || msg == "ctrl-c received — shutting down" {
            if section_open {
                println!();
            }
            section_open = true;
        }

        let prefix = if style.is_empty() {
            format!("  {} `{}`", icon, ts_short)
        } else {
            format!("  {}{} {}`{}`{}", style, icon, RESET, ts_short, style)
        };

        // Colorize the label
        let label_fmt = if style.is_empty() {
            format!("[{}]", label)
        } else {
            format!("{}[{}]{}", style, label, RESET)
        };

        // Format the message — highlight agent names, task IDs
        let msg_fmt = msg
            .replace(": stopped", "")
            .replace(": process exited unexpectedly", " exited unexpectedly");

        println!("{prefix} {label_fmt} {msg_fmt}");
    }

    println!();
    println!("---");
    println!(
        "> End of replay. {} events shown.",
        events.len()
    );

    Ok(())
}

fn run_task_export(format: &str, output: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;

    let formatted = if format == "json" {
        let snapshots = store.task_export_all()?;
        serde_json::to_string_pretty(&snapshots)?
    } else {
        let tasks = store.task_list(None, None)?;
        if tasks.is_empty() {
            "no tasks".to_string()
        } else {
            let open: Vec<_> = tasks.iter().filter(|t| t.status == store::TaskStatus::Open).collect();
            let claimed: Vec<_> = tasks.iter().filter(|t| t.status == store::TaskStatus::Claimed).collect();
            let done: Vec<_> = tasks.iter().filter(|t| t.status == store::TaskStatus::Done).collect();
            let blocked: Vec<_> = tasks.iter().filter(|t| t.status == store::TaskStatus::Blocked).collect();

            let mut buf = String::new();
            let print_section = |buf: &mut String, header: &str, prefix: &str, items: &[&store::Task]| {
                if items.is_empty() {
                    return;
                }
                buf.push_str(&format!("## {header}\n"));
                for t in items {
                    let suffix = t.blocked_reason.as_deref()
                        .or(t.note.as_deref())
                        .map(|s| format!(" — {}", s.chars().take(60).collect::<String>()))
                        .unwrap_or_default();
                    buf.push_str(&format!("{prefix} #{} {}{suffix}\n", t.id, t.title));
                }
                buf.push('\n');
            };

            print_section(&mut buf, "Open", "- [ ]", &open);
            print_section(&mut buf, "In Progress", "- [~]", &claimed);
            print_section(&mut buf, "Blocked", "- [~]", &blocked);
            print_section(&mut buf, "Done", "- [x]", &done);
            buf
        }
    };

    if let Some(path) = output {
        std::fs::write(path, &formatted)
            .with_context(|| format!("failed to write to {path:?}"))?;
        println!("wrote task board to {path}");
    } else {
        print!("{formatted}");
    }

    Ok(())
}

fn run_task_import(file: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }

    let content = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {file:?}"))?;
    let snapshots: Vec<store::TaskSnapshot> = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {file:?} as JSON task snapshot"))?;

    let total = snapshots.len();
    let mut skipped = 0usize;
    let valid: Vec<store::TaskSnapshot> = snapshots.into_iter().filter_map(|s| {
        let mut s = s;
        if s.title.trim().is_empty() {
            eprintln!("skipping task with empty title");
            skipped += 1;
            return None;
        }
        if s.priority > 4 {
            eprintln!("warning: task {:?} has priority {} (out of range 0-4), clamping to 4", s.title, s.priority);
            s.priority = 4;
        }
        Some(s)
    }).collect();

    let store = store::Store::open(&db)?;
    let new_ids = store.bulk_import_tasks(&valid, "human")?;

    println!(
        "imported {}/{} task(s) from {file:?}: {}",
        new_ids.len(),
        total,
        new_ids
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if skipped > 0 {
        eprintln!("{skipped} task(s) skipped");
        anyhow::bail!("{skipped} task(s) were skipped due to validation errors");
    }
    Ok(())
}

fn run_task_stats(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;
    let tasks = store.task_list(None, None)?;

    if tasks.is_empty() {
        println!("no tasks");
        return Ok(());
    }

    let total = tasks.len() as u64;
    let done_tasks: Vec<_> = tasks
        .iter()
        .filter(|t| t.status == store::TaskStatus::Done)
        .collect();
    let open_count = tasks.iter().filter(|t| t.status == store::TaskStatus::Open).count() as u64;
    let claimed_count = tasks.iter().filter(|t| t.status == store::TaskStatus::Claimed).count() as u64;
    let blocked_count = tasks.iter().filter(|t| t.status == store::TaskStatus::Blocked).count() as u64;
    let done_count = done_tasks.len() as u64;

    // Avg completion time: updated_at - created_at for done tasks
    let avg_completion_secs: Option<f64> = if done_count > 0 {
        let sum: i64 = done_tasks
            .iter()
            .map(|t| (t.updated_at - t.created_at).max(0))
            .sum();
        Some(sum as f64 / done_count as f64)
    } else {
        None
    };

    // Throughput: done tasks per hour over the observed time span
    let throughput_per_hour: Option<f64> = if done_count > 0 {
        let min_created = tasks.iter().map(|t| t.created_at).min().unwrap_or(0);
        let max_updated = done_tasks.iter().map(|t| t.updated_at).max().unwrap_or(0);
        let span_secs = (max_updated - min_created).max(1) as f64;
        Some(done_count as f64 / (span_secs / 3600.0))
    } else {
        None
    };

    // Blocked rate: blocked tasks / total
    let blocked_rate_pct = blocked_count as f64 / total as f64 * 100.0;

    // Top claimants by tasks completed
    let mut claimants: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for t in &done_tasks {
        if let Some(agent) = &t.claimed_by {
            *claimants.entry(agent.clone()).or_insert(0) += 1;
        }
    }
    let mut claimant_vec: Vec<(String, u64)> = claimants.into_iter().collect();
    claimant_vec.sort_by_key(|b| std::cmp::Reverse(b.1));

    if json {
        let obj = serde_json::json!({
            "total": total,
            "open": open_count,
            "in_progress": claimed_count,
            "done": done_count,
            "blocked": blocked_count,
            "avg_completion_secs": avg_completion_secs,
            "throughput_per_hour": throughput_per_hour,
            "blocked_rate_pct": blocked_rate_pct,
            "top_claimants": claimant_vec.iter()
                .map(|(a, c)| serde_json::json!({"agent": a, "tasks_done": c}))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    let fmt_duration = |secs: f64| -> String {
        if secs < 60.0 {
            format!("{:.0}s", secs)
        } else if secs < 3600.0 {
            format!("{:.1}m", secs / 60.0)
        } else {
            format!("{:.1}h", secs / 3600.0)
        }
    };

    println!("Task board — {} total", total);
    println!("  open: {}  in-progress: {}  done: {}  blocked: {}", open_count, claimed_count, done_count, blocked_count);
    println!();
    println!("Velocity");
    match avg_completion_secs {
        Some(s) => println!("  avg completion time : {}", fmt_duration(s)),
        None => println!("  avg completion time : n/a (no done tasks)"),
    }
    match throughput_per_hour {
        Some(r) => println!("  throughput          : {:.2} tasks/hour", r),
        None => println!("  throughput          : n/a"),
    }
    println!("  blocked rate        : {:.1}%", blocked_rate_pct);
    if !claimant_vec.is_empty() {
        println!();
        println!("Top contributors");
        for (agent, count) in claimant_vec.iter().take(10) {
            println!("  {:<20} {} task{}", agent, count, if *count == 1 { "" } else { "s" });
        }
    }
    Ok(())
}

fn run_task_watch_board(no_color: bool, interval: u64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .map_err(|e| anyhow::anyhow!("failed to set Ctrl-C handler: {e}"))?;

    let clear = if no_color { "\n" } else { "\x1b[2J\x1b[H" };

    while running.load(std::sync::atomic::Ordering::SeqCst) {
        print!("{clear}");
        match store.task_list(None, None) {
            Ok(tasks) => {
                if no_color {
                    let open = tasks.iter().filter(|t| t.status == store::TaskStatus::Open).count();
                    let claimed = tasks.iter().filter(|t| t.status == store::TaskStatus::Claimed).count();
                    let done = tasks.iter().filter(|t| t.status == store::TaskStatus::Done).count();
                    let blocked = tasks.iter().filter(|t| t.status == store::TaskStatus::Blocked).count();
                    let total = tasks.len();
                    println!("Task board — {total} tasks | {open} open · {claimed} claimed · {done} done · {blocked} blocked");
                    println!("---");
                    for t in &tasks {
                        let who = t.claimed_by.as_deref().map(|w| format!(" @{w}")).unwrap_or_default();
                        let deps = if t.depends_on.is_empty() {
                            String::new()
                        } else {
                            format!(" deps:[{}]", t.depends_on.iter().map(|d| format!("#{d}")).collect::<Vec<_>>().join(","))
                        };
                        let extra = match t.status {
                            store::TaskStatus::Blocked => t.blocked_reason.as_deref().map(|r| format!(" — blocked: {r}")).unwrap_or_default(),
                            store::TaskStatus::Done => t.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default(),
                            _ => String::new(),
                        };
                        println!("#{:<4} p{} {:<8}{who}{deps} {}{extra}", t.id, t.priority, t.status.as_str(), t.title);
                    }
                } else {
                    cli::print_tasks(&tasks);
                }
            }
            Err(e) => eprintln!("error reading tasks: {e}"),
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        for _ in 0..interval {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            if !running.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
        }
    }
    Ok(())
}

fn run_task_watch_single(task_id: i64, interval: u64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .map_err(|e| anyhow::anyhow!("failed to set Ctrl-C handler: {e}"))?;

    let mut last_status = String::new();
    println!("watching task #{task_id} (Ctrl-C to stop)");

    while running.load(std::sync::atomic::Ordering::SeqCst) {
        match store.task_get(task_id) {
            Ok(Some(task)) => {
                let status = task.status.as_str().to_string();
                if status != last_status {
                    let who = task.claimed_by.as_deref().map(|w| format!(" @{w}")).unwrap_or_default();
                    let extra = match task.status {
                        store::TaskStatus::Blocked => task.blocked_reason.as_deref().map(|r| format!(" — {r}")).unwrap_or_default(),
                        store::TaskStatus::Done => task.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default(),
                        _ => String::new(),
                    };
                    println!("[{}] #{} {} — {}{}{extra}", fmt_unix_ts(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64), task.id, status, task.title, who);
                    last_status = status.clone();
                    if matches!(task.status, store::TaskStatus::Done | store::TaskStatus::Blocked) {
                        break;
                    }
                }
            }
            Ok(None) => {
                eprintln!("task #{task_id} not found");
                break;
            }
            Err(e) => eprintln!("error: {e}"),
        }
        for _ in 0..interval {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            if !running.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
        }
    }
    Ok(())
}

fn run_metrics(agent_filter: Option<String>, json_out: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let conn = rusqlite::Connection::open(&db)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let hour_ago = now_secs - 3600;

    let sql = "SELECT agent, started_at, ended_at, cost_usd FROM runs WHERE ended_at IS NOT NULL ORDER BY agent, started_at";
    let mut stmt = conn.prepare(sql)?;
    struct Row { agent: String, dur_secs: f64, cost: f64, started_at: i64 }
    let mut rows: Vec<Row> = Vec::new();
    {
        let iter = stmt.query_map([], |row| {
            let agent: String = row.get(0)?;
            let started: i64 = row.get(1)?;
            let ended: i64 = row.get(2)?;
            let cost: f64 = row.get::<_, Option<f64>>(3)?.unwrap_or(0.0);
            Ok((agent, started, ended, cost))
        })?;
        for item in iter {
            let (agent, started, ended, cost) = item?;
            rows.push(Row { agent, dur_secs: (ended - started).max(0) as f64, cost, started_at: started });
        }
    }

    // Group by agent
    let mut agents: Vec<String> = rows.iter().map(|r| r.agent.clone()).collect();
    agents.sort();
    agents.dedup();

    if let Some(ref f) = agent_filter {
        agents.retain(|a| a == f);
        if agents.is_empty() {
            anyhow::bail!("no runs found for agent '{f}'");
        }
    }

    struct AgentStats {
        name: String,
        total_turns: usize,
        avg_dur: f64,
        min_dur: f64,
        max_dur: f64,
        avg_cost: f64,
        turns_last_hour: usize,
        cost_per_hour: f64,
    }

    let mut stats: Vec<AgentStats> = Vec::new();
    for agent in &agents {
        let agent_rows: Vec<&Row> = rows.iter().filter(|r| &r.agent == agent).collect();
        if agent_rows.is_empty() { continue; }
        let total_turns = agent_rows.len();
        let avg_dur = agent_rows.iter().map(|r| r.dur_secs).sum::<f64>() / total_turns as f64;
        let min_dur = agent_rows.iter().map(|r| r.dur_secs).fold(f64::INFINITY, f64::min);
        let max_dur = agent_rows.iter().map(|r| r.dur_secs).fold(0.0_f64, f64::max);
        let total_cost: f64 = agent_rows.iter().map(|r| r.cost).sum();
        let avg_cost = total_cost / total_turns as f64;
        let turns_last_hour = agent_rows.iter().filter(|r| r.started_at >= hour_ago).count();
        let cost_last_hour: f64 = agent_rows.iter().filter(|r| r.started_at >= hour_ago).map(|r| r.cost).sum();
        stats.push(AgentStats {
            name: agent.clone(),
            total_turns,
            avg_dur,
            min_dur,
            max_dur,
            avg_cost,
            turns_last_hour,
            cost_per_hour: cost_last_hour,
        });
    }

    if stats.is_empty() {
        println!("no completed runs found");
        return Ok(());
    }

    if json_out {
        println!("[");
        for (i, s) in stats.iter().enumerate() {
            let comma = if i + 1 < stats.len() { "," } else { "" };
            println!(
                r#"  {{"agent":"{}", "total_turns":{}, "avg_dur_secs":{:.1}, "min_dur_secs":{:.1}, "max_dur_secs":{:.1}, "avg_cost_usd":{:.6}, "turns_last_hour":{}, "cost_per_hour_usd":{:.6}}}{comma}"#,
                s.name, s.total_turns, s.avg_dur, s.min_dur, s.max_dur, s.avg_cost, s.turns_last_hour, s.cost_per_hour
            );
        }
        println!("]");
    } else {
        println!("{:<24} {:>6} {:>8} {:>8} {:>8} {:>10} {:>8} {:>10}",
            "agent", "turns", "avg(s)", "min(s)", "max(s)", "avg$/turn", "last-hr", "$/hr");
        println!("{}", "-".repeat(90));
        for s in &stats {
            println!("{:<24} {:>6} {:>8.1} {:>8.1} {:>8.1} {:>10.6} {:>8} {:>10.6}",
                s.name, s.total_turns, s.avg_dur, s.min_dur, s.max_dur, s.avg_cost, s.turns_last_hour, s.cost_per_hour);
        }
    }
    Ok(())
}

fn init_logging(project_root: &std::path::Path, headless: bool) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let log_dir = paths::log_dir(project_root)?;
    // Use synchronous (blocking) writes so log entries are never lost in a
    // buffer when the process exits or panics. Non-blocking with a leaked guard
    // meant the background writer thread was killed before it could flush,
    // silently discarding the last N entries and hiding crash causes.
    let file_appender = tracing_appender::rolling::daily(log_dir, "hub.log");

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("agentcom=info"))
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
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

/// Format a unix timestamp as "YYYY-MM-DD HH:MM:SS" without external crates.
fn fmt_unix_ts(ts: i64) -> String {
    // Days since epoch → calendar date via the Gregorian algorithm.
    let secs_of_day = ts.rem_euclid(86400) as u32;
    let days = (ts / 86400) as i64;
    // Shift epoch from 1970-01-01 to 0001-03-01 (civil calendar base).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}:{s:02}")
}

fn run_summary(json_out: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    // Total cost from runs table
    let total_cost_usd: f64;
    let total_turns: i64;
    let agent_count: i64;
    let tasks_done: i64;
    let tasks_total: i64;

    let db = paths::db_path(&project_root)?;
    if db.exists() {
        let conn = rusqlite::Connection::open(&db)?;
        total_cost_usd = conn
            .query_row("SELECT COALESCE(SUM(cost_usd),0.0) FROM runs", [], |r| r.get(0))
            .unwrap_or(0.0);
        total_turns = conn
            .query_row("SELECT COUNT(*) FROM runs WHERE ended_at IS NOT NULL", [], |r| r.get(0))
            .unwrap_or(0);
        agent_count = conn
            .query_row("SELECT COUNT(DISTINCT agent) FROM runs", [], |r| r.get(0))
            .unwrap_or(0);
        tasks_done = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE status='done'", [], |r| r.get(0))
            .unwrap_or(0);
        tasks_total = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap_or(0);
    } else {
        total_cost_usd = 0.0;
        total_turns = 0;
        agent_count = 0;
        tasks_done = 0;
        tasks_total = 0;
    }

    // Commit count from git
    let commit_count = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&project_root)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);

    if json_out {
        println!(
            r#"{{"total_cost_usd":{total_cost_usd:.6},"total_turns":{total_turns},"active_agents":{agent_count},"commits":{commit_count},"tasks_done":{tasks_done},"tasks_total":{tasks_total}}}"#
        );
    } else {
        println!("Session summary");
        println!("  cost       ${total_cost_usd:.4} USD");
        println!("  turns      {total_turns}");
        println!("  agents     {agent_count}");
        println!("  commits    {commit_count}");
        println!("  tasks      {tasks_done}/{tasks_total} done");
    }
    Ok(())
}

fn run_cost(agent_filter: Option<String>, json_out: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let conn = rusqlite::Connection::open(&db)?;

    // Query all completed runs
    let sql = "SELECT agent, cost_usd, turns FROM runs WHERE ended_at IS NOT NULL ORDER BY agent";
    let mut stmt = conn.prepare(sql)?;

    struct AgentRow { agent: String, cost: f64, turns: i64 }
    let mut rows: Vec<AgentRow> = Vec::new();
    {
        let iter = stmt.query_map([], |row| {
            Ok(AgentRow {
                agent: row.get(0)?,
                cost: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                turns: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            })
        })?;
        for r in iter { rows.push(r?); }
    }

    if let Some(ref f) = agent_filter {
        rows.retain(|r| &r.agent == f);
        if rows.is_empty() {
            anyhow::bail!("no runs found for agent '{f}'");
        }
    }

    // Aggregate per agent
    let mut agents: Vec<String> = rows.iter().map(|r| r.agent.clone()).collect();
    agents.sort();
    agents.dedup();

    struct AgentStats { name: String, sessions: usize, total_cost: f64, total_turns: i64 }
    let mut stats: Vec<AgentStats> = agents.iter().map(|a| {
        let agent_rows: Vec<&AgentRow> = rows.iter().filter(|r| &r.agent == a).collect();
        let sessions = agent_rows.len();
        let total_cost = agent_rows.iter().map(|r| r.cost).sum();
        let total_turns = agent_rows.iter().map(|r| r.turns).sum();
        AgentStats { name: a.clone(), sessions, total_cost, total_turns }
    }).collect();

    // Sort most expensive first
    stats.sort_by(|a, b| b.total_cost.partial_cmp(&a.total_cost).unwrap_or(std::cmp::Ordering::Equal));
    let grand_total: f64 = stats.iter().map(|s| s.total_cost).sum();
    let grand_turns: i64 = stats.iter().map(|s| s.total_turns).sum();

    if json_out {
        let entries: Vec<String> = stats.iter().map(|s| {
            let avg = if s.total_turns > 0 { s.total_cost / s.total_turns as f64 } else { 0.0 };
            format!(
                r#"{{"agent":"{}", "sessions":{}, "total_cost_usd":{:.6}, "turns":{}, "avg_cost_per_turn":{:.6}}}"#,
                s.name, s.sessions, s.total_cost, s.total_turns, avg
            )
        }).collect();
        println!(r#"{{"total_cost_usd":{:.6},"agents":[{}]}}"#, grand_total, entries.join(","));
    } else {
        println!("cost breakdown (all time)");
        println!("  {:<20} {:>8}  {:>8}  {:>6}  {:>12}", "agent", "cost", "turns", "sess", "$/turn");
        println!("  {}", "-".repeat(60));
        for s in &stats {
            let avg = if s.total_turns > 0 { s.total_cost / s.total_turns as f64 } else { 0.0 };
            println!("  {:<20} {:>8.4}  {:>8}  {:>6}  {:>12.6}",
                s.name, s.total_cost, s.total_turns, s.sessions, avg);
        }
        println!("  {}", "-".repeat(60));
        let grand_avg = if grand_turns > 0 { grand_total / grand_turns as f64 } else { 0.0 };
        println!("  {:<20} {:>8.4}  {:>8}  {:>6}  {:>12.6}",
            "TOTAL", grand_total, grand_turns, stats.iter().map(|s| s.sessions).sum::<usize>(), grand_avg);
    }

    Ok(())
}

fn run_preflight(verbose: bool) -> Result<()> {
    use std::collections::HashSet;

    // Get staged diff
    let diff_out = std::process::Command::new("git")
        .args(["diff", "--cached", "-U0"])
        .output()
        .context("failed to run git diff --cached")?;
    let diff = String::from_utf8_lossy(&diff_out.stdout);

    if diff.trim().is_empty() {
        println!("preflight: nothing staged");
        return Ok(());
    }

    // Find struct names whose fields were added/changed.
    // Match lines like: +    field_name: Type, (inside a struct body)
    // We heuristically attribute them to structs whose definition changed in the same diff.
    let mut changed_types: HashSet<String> = HashSet::new();

    // Extract struct names from diff lines like: +pub struct Foo {  or  +    pub struct Foo {
    let struct_re = regex::Regex::new(r"^\+.*\bstruct\s+(\w+)\b").unwrap();
    // Also capture enum variants / struct patterns from +    FieldName { or +    FieldName,
    // More importantly: track which struct blocks have field additions.
    // Strategy: track current file being diffed, find "struct X" in context, then look for "+" field lines.

    let mut current_struct: Option<String> = None;
    let mut brace_depth: i32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff --git") || line.starts_with("@@") {
            current_struct = None;
            brace_depth = 0;
        }

        // Detect struct definition in diff context (+ or context lines)
        if let Some(caps) = struct_re.captures(line) {
            current_struct = Some(caps[1].to_string());
            brace_depth = 0;
        }

        // Track brace depth to stay inside the struct
        if let Some(ref name) = current_struct.clone() {
            let opens = line.chars().filter(|&c| c == '{').count() as i32;
            let closes = line.chars().filter(|&c| c == '}').count() as i32;
            brace_depth += opens - closes;
            if brace_depth < 0 {
                current_struct = None;
                brace_depth = 0;
            }

            // A line starting with "+" inside a struct = field addition
            if line.starts_with('+') && !line.starts_with("+++") {
                let content = &line[1..].trim_start();
                // Skip empty lines, comments, and closing braces
                if !content.is_empty() && !content.starts_with("//") && *content != "}" {
                    changed_types.insert(name.clone());
                }
            }
        }
    }

    // Also pick up enum/struct names from changed impl blocks and type aliases
    let enum_re = regex::Regex::new(r"^\+.*\benum\s+(\w+)\b").unwrap();
    for line in diff.lines() {
        if let Some(caps) = enum_re.captures(line) {
            // Only include if the enum itself has additions nearby
            changed_types.insert(caps[1].to_string());
        }
    }

    if changed_types.is_empty() {
        println!("preflight: no struct/enum field changes detected in staged diff");
        return Ok(());
    }

    println!("preflight: changed types: {}", changed_types.iter().cloned().collect::<Vec<_>>().join(", "));
    println!();

    // Find all *.rs files in the repo
    let find_out = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "*.rs"])
        .output()
        .context("failed to list repo files")?;
    let all_rs: Vec<String> = String::from_utf8_lossy(&find_out.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    let mut warned = false;
    for type_name in &changed_types {
        // Look for construction sites: TypeName { or TypeName:: (match but not just a comment)
        let grep_out = std::process::Command::new("git")
            .args(["grep", "-l", "--", &format!(r"{type_name}\s*\{{")])
            .output();
        let mut matching: Vec<String> = Vec::new();
        if let Ok(out) = grep_out {
            matching = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .filter(|f| f.ends_with(".rs"))
                .collect();
        }

        // Also try struct literal pattern with grep across all rs files manually
        if matching.is_empty() {
            for path in &all_rs {
                let content = std::fs::read_to_string(path).unwrap_or_default();
                if content.contains(&format!("{type_name} {{")) || content.contains(&format!("{type_name}{{")) {
                    matching.push(path.clone());
                }
            }
        }

        if matching.is_empty() {
            if verbose {
                println!("  {type_name}: no construction sites found");
            }
            continue;
        }

        println!("  {type_name}: {} file(s) construct this type — check for missing fields:", matching.len());
        for f in &matching {
            println!("    {f}");
        }
        warned = true;
    }

    if !warned {
        println!("preflight: no other files construct the changed types — safe to commit");
    } else {
        println!();
        println!("preflight: review the files above for missing fields before committing");
    }

    Ok(())
}

async fn run_context_push(file: std::path::PathBuf, agent_name: Option<String>) -> Result<()> {
    let contents = std::fs::read_to_string(&file)
        .with_context(|| format!("cannot read {}", file.display()))?;
    let filename = file.file_name().unwrap_or(file.as_os_str()).to_string_lossy();
    let body = format!("--- context push: {filename} ---\n{contents}");
    let to = agent_name.unwrap_or_else(|| "all".to_string());
    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;
    let resp = client
        .request(&ipc::Request::Send { to: to.clone(), body, urgent: false })
        .await?;
    match resp {
        ipc::Response::Ok { message } => {
            println!("{}", message.unwrap_or_else(|| format!("context pushed to {to}")));
            Ok(())
        }
        ipc::Response::Err { message } => anyhow::bail!("hub: {message}"),
        _ => Ok(()),
    }
}

fn run_snapshot(out_file: Option<std::path::PathBuf>) -> Result<()> {
    use std::io::Write;
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    let db_path = paths::db_path(&project_root)?;
    let cfg_path = project_root.join(paths::CONFIG_FILE);

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let out_path = out_file.unwrap_or_else(|| {
        cwd.join(format!("agentcom-{ts}.snap"))
    });

    let mut f = std::fs::File::create(&out_path)
        .with_context(|| format!("cannot create {}", out_path.display()))?;

    f.write_all(b"ACSNAP01")?;

    // Checkpoint WAL so the DB file is fully up-to-date before we copy bytes.
    if db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(FULL);");
        }
    }

    for (label, path) in [("hub.db", &db_path), ("agentcom.toml", &cfg_path)] {
        if path.exists() {
            let data = std::fs::read(path)?;
            let name = label.as_bytes();
            f.write_all(&(name.len() as u32).to_le_bytes())?;
            f.write_all(name)?;
            f.write_all(&(data.len() as u64).to_le_bytes())?;
            f.write_all(&data)?;
        }
    }

    println!("snapshot saved to {}", out_path.display());
    Ok(())
}

fn run_restore(snap_file: std::path::PathBuf) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;

    let data = std::fs::read(&snap_file)
        .with_context(|| format!("cannot read {}", snap_file.display()))?;

    if data.len() < 8 || &data[..8] != b"ACSNAP01" {
        anyhow::bail!("not a valid agentcom snapshot file");
    }

    let mut pos = 8usize;
    while pos < data.len() {
        if pos + 4 > data.len() { break; }
        let name_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + name_len > data.len() { break; }
        let name = std::str::from_utf8(&data[pos..pos+name_len])
            .context("invalid filename in snapshot")?;
        pos += name_len;
        if pos + 8 > data.len() { break; }
        let data_len = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + data_len > data.len() { break; }
        let content = &data[pos..pos+data_len];
        pos += data_len;

        let dest = match name {
            "hub.db" => paths::db_path(&project_root)?,
            "agentcom.toml" => project_root.join(paths::CONFIG_FILE),
            _ => {
                eprintln!("unknown file in snapshot: {name}, skipping");
                continue;
            }
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, content)?;
        println!("restored {name} → {}", dest.display());
    }
    println!("restore complete");
    Ok(())
}

fn run_audit(
    event_filter: Option<String>,
    agent_filter: Option<String>,
    since: Option<String>,
    count: usize,
    json_out: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let log_path = paths::data_dir(&project_root)?.join("audit.log");
    let log_is_empty = !log_path.exists() || log_path.metadata().map(|m| m.len() == 0).unwrap_or(true);
    if log_is_empty {
        // Fall back to synthesizing audit events from the tasks table.
        let db = paths::db_path(&project_root)?;
        if !db.exists() {
            if json_out { println!("[]"); } else { println!("no audit log and no hub database found"); }
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&db)?;
        let mut stmt = conn.prepare(
            "SELECT id, title, status, claimed_by, priority, updated_at FROM tasks ORDER BY updated_at DESC LIMIT ?1"
        )?;
        let rows: Vec<(i64, String, String, Option<String>, i64, i64)> = {
            let iter = stmt.query_map([count as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })?;
            iter.filter_map(|r| r.ok()).collect()
        };
        if json_out {
            let vals: Vec<serde_json::Value> = rows.iter().map(|(id, title, status, agent, _, ts)| {
                serde_json::json!({"event": format!("task_{status}"), "task_id": id, "title": title, "agent": agent, "ts": ts})
            }).collect();
            println!("{}", serde_json::to_string_pretty(&vals)?);
        } else {
            println!("(audit.log empty — showing task activity from DB)");
            println!("{:<24} {:<16} {:<6} {}", "TIME", "EVENT", "ID", "TITLE");
            println!("{}", "-".repeat(70));
            for (id, title, status, agent, _, ts) in &rows {
                let who = agent.as_deref().unwrap_or("-");
                println!("{:<24} {:<16} {:<6} {} @{}", fmt_unix_ts(*ts), format!("task_{status}"), id, title, who);
            }
        }
        return Ok(());
    }

    let since_ts: Option<i64> = since.as_deref().map(|s| {
        // Parse YYYY-MM-DD without chrono: days since epoch * 86400.
        let parts: Vec<&str> = s.splitn(3, '-').collect();
        if parts.len() == 3 {
            if let (Ok(y), Ok(m), Ok(d)) = (
                parts[0].parse::<i64>(),
                parts[1].parse::<i64>(),
                parts[2].parse::<i64>(),
            ) {
                // Rough days-since-epoch: good enough for filtering.
                let days = (y - 1970) * 365 + (y - 1969) / 4 + (m - 1) * 30 + d;
                return days * 86400;
            }
        }
        0
    });

    let content = std::fs::read_to_string(&log_path)?;
    let mut events: Vec<serde_json::Value> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|v: &serde_json::Value| {
            if let Some(ref ef) = event_filter {
                if v["event"].as_str().unwrap_or("") != ef {
                    return false;
                }
            }
            if let Some(ref af) = agent_filter {
                if v["agent"].as_str().unwrap_or("") != af {
                    return false;
                }
            }
            if let Some(ts_min) = since_ts {
                if v["ts"].as_i64().unwrap_or(0) < ts_min {
                    return false;
                }
            }
            true
        })
        .collect();

    // Apply count limit (most recent N, so take from the end).
    if events.len() > count {
        let skip = events.len() - count;
        events = events.into_iter().skip(skip).collect();
    }

    if json_out {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }

    if events.is_empty() {
        println!("no audit events matching filters");
        return Ok(());
    }

    for ev in &events {
        let ts = ev["ts"].as_i64().unwrap_or(0);
        let dt = fmt_unix_ts(ts);
        let event = ev["event"].as_str().unwrap_or("?");
        let agent = ev["agent"].as_str().unwrap_or("?");
        // Print extra fields (everything except ts/event/agent) as key=value.
        let extras: String = ev
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "ts" | "event" | "agent"))
                    .map(|(k, v)| {
                        let s = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        format!(" {k}={s}")
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!("{dt}  {event:<20}  {agent:<14}{extras}");
    }
    Ok(())
}

fn run_task_trace(id: i64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let store = store::Store::open(&db)?;

    // Load task.
    let tasks = store.task_list(None, None)?;
    let task = tasks.iter().find(|t| t.id == id)
        .ok_or_else(|| anyhow::anyhow!("task #{id} not found"))?;

    println!("Task #{id}: {}", task.title);
    println!("Status:    {:?}", task.status);
    println!("Priority:  {}", task.priority);
    if !task.description.is_empty() {
        println!("Desc:      {}", task.description);
    }
    if let Some(ref by) = task.claimed_by {
        println!("Claimed:   {by}");
    }
    if !task.tags.is_empty() {
        println!("Tags:      {}", task.tags.join(", "));
    }
    if let Some(ref note) = task.note {
        println!("Note:      {note}");
    }
    if !task.depends_on.is_empty() {
        println!("Deps:      #{}", task.depends_on.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", #"));
    }

    println!();
    println!("Timeline:");
    println!("  {} created by {}", fmt_unix_ts(task.created_at), task.created_by);

    // Activity log comments.
    let comments = store.task_comments(id)?;
    for c in &comments {
        println!("  {} [{}] {}", fmt_unix_ts(c.created_at), c.agent, c.body);
    }
    println!("  {} last updated", fmt_unix_ts(task.updated_at));
    Ok(())
}

fn run_task_deps(id: i64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    // Load all tasks into a map for fast lookup.
    let mut stmt = conn.prepare(
        "SELECT id, title, status FROM tasks ORDER BY id",
    )?;
    let all: std::collections::HashMap<i64, (String, String)> = {
        let mut m = std::collections::HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
            let (tid, title, status) = row?;
            m.insert(tid, (title, status));
        }
        m
    };

    let label = |tid: i64| -> String {
        if let Some((title, status)) = all.get(&tid) {
            let short = if title.len() > 40 { format!("{}…", &title[..40]) } else { title.clone() };
            format!("#{tid} [{status}] {short}")
        } else {
            format!("#{tid} [?]")
        }
    };

    // Upstream: what this task depends on.
    let mut stmt2 = conn.prepare(
        "SELECT depends_on_id FROM task_deps WHERE task_id = ?1 ORDER BY depends_on_id",
    )?;
    let upstream: Vec<i64> = stmt2
        .query_map(rusqlite::params![id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    // Downstream: what depends on this task.
    let mut stmt3 = conn.prepare(
        "SELECT task_id FROM task_deps WHERE depends_on_id = ?1 ORDER BY task_id",
    )?;
    let downstream: Vec<i64> = stmt3
        .query_map(rusqlite::params![id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    println!("Dependencies for {}", label(id));
    println!();

    if upstream.is_empty() {
        println!("Upstream (depends on): none");
    } else {
        println!("Upstream (depends on):");
        for tid in &upstream {
            println!("  ← {}", label(*tid));
        }
    }
    println!();
    if downstream.is_empty() {
        println!("Downstream (blocks): none");
    } else {
        println!("Downstream (blocks):");
        for tid in &downstream {
            println!("  → {}", label(*tid));
        }
    }
    Ok(())
}

/// Parse "YYYY-MM-DD" to a unix timestamp (midnight UTC) without chrono.
fn parse_date_to_ts(s: &str) -> Result<i64> {
    let parts: Vec<&str> = s.splitn(3, '-').collect();
    if parts.len() != 3 {
        anyhow::bail!("invalid date {s:?} — use YYYY-MM-DD");
    }
    let y: i64 = parts[0].parse().context("invalid year")?;
    let m: u32 = parts[1].parse().context("invalid month")?;
    let d: u32 = parts[2].parse().context("invalid day")?;
    if m < 1 || m > 12 || d < 1 || d > 31 {
        anyhow::bail!("invalid date {s:?}");
    }
    // Days from civil epoch (0001-03-01) using Gregorian algorithm.
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400)
}

async fn run_task_due(id: i64, date: Option<String>, clear: bool) -> Result<()> {
    let due_at: Option<i64> = if clear {
        None
    } else if let Some(ref s) = date {
        // Accept unix timestamp directly or YYYY-MM-DD.
        if let Ok(ts) = s.parse::<i64>() {
            Some(ts)
        } else {
            Some(parse_date_to_ts(s)?)
        }
    } else {
        anyhow::bail!("provide a date (YYYY-MM-DD) or --clear");
    };

    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;
    let resp = client.request(&ipc::Request::TaskSetDue { id, due_at }).await?;
    match resp {
        ipc::Response::Ok { message } => {
            println!("{}", message.unwrap_or_else(|| {
                if clear { format!("cleared due date on #{id}") }
                else { format!("set due date on #{id}") }
            }));
            Ok(())
        }
        ipc::Response::Err { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn run_task_graph() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&project_root)?;
    if !db.exists() {
        anyhow::bail!("no hub data found — run `agentcom up` first");
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let mut stmt = conn.prepare("SELECT id, title, status FROM tasks ORDER BY id")?;
    let tasks: Vec<(i64, String, String)> = {
        let mut v = Vec::new();
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            v.push(row?);
        }
        v
    };

    let mut stmt2 = conn.prepare("SELECT task_id, depends_on_id FROM task_deps ORDER BY task_id")?;
    let edges: Vec<(i64, i64)> = {
        let mut v = Vec::new();
        for row in stmt2.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            v.push(row?);
        }
        v
    };

    if tasks.is_empty() {
        println!("no tasks");
        return Ok(());
    }

    println!("```mermaid");
    println!("graph TD");
    for (id, title, status) in &tasks {
        let short = if title.len() > 35 { format!("{}…", &title[..35]) } else { title.clone() };
        println!("  N{id}[\"#{id} {short} [{status}]\"]");
    }
    for (task_id, dep_id) in &edges {
        println!("  N{dep_id} --> N{task_id}");
    }
    println!("```");
    Ok(())
}

async fn run_bulk_done(ids: Vec<i64>, note: Option<String>) -> Result<()> {
    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;
    let mut ok = 0u32;
    let mut errs = 0u32;
    for id in &ids {
        let resp = client
            .request(&ipc::Request::TaskDone { id: *id, note: note.clone() })
            .await?;
        match resp {
            ipc::Response::Ok { .. } => {
                println!("#{id} done");
                ok += 1;
            }
            ipc::Response::Err { message } => {
                eprintln!("#{id} error: {message}");
                errs += 1;
            }
            _ => { errs += 1; }
        }
    }
    println!("{ok}/{} marked done", ids.len());
    if errs > 0 {
        anyhow::bail!("{errs} task(s) failed");
    }
    Ok(())
}

async fn run_bulk_claim(ids: Vec<i64>) -> Result<()> {
    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;
    let mut ok = 0u32;
    let mut errs = 0u32;
    for id in &ids {
        let resp = client
            .request(&ipc::Request::TaskClaim { id: *id })
            .await?;
        match resp {
            ipc::Response::Ok { .. } => {
                println!("#{id} claimed");
                ok += 1;
            }
            ipc::Response::Err { message } => {
                eprintln!("#{id} error: {message}");
                errs += 1;
            }
            _ => { errs += 1; }
        }
    }
    println!("{ok}/{} claimed", ids.len());
    if errs > 0 {
        anyhow::bail!("{errs} task(s) failed");
    }
    Ok(())
}

async fn run_task_add_nl(
    title: String,
    description: String,
    priority: i64,
    depends_on: Vec<i64>,
    timeout: Option<u64>,
    requires: Vec<String>,
) -> Result<()> {
    // Try to expand via claude. If unavailable, fall back to raw text.
    let expanded = try_expand_nl_task(&title).await;
    let (final_title, final_desc, final_priority) = match expanded {
        Some((t, d, p)) => (t, d, p),
        None => (title, description, priority),
    };

    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;
    let resp = client.request(&ipc::Request::TaskAdd {
        title: final_title,
        description: final_desc,
        priority: final_priority,
        depends_on,
        timeout_mins: timeout,
        requires,
        recur: None,
    }).await?;
    match resp {
        ipc::Response::Ok { message } => {
            println!("{}", message.unwrap_or_else(|| "task added".into()));
            Ok(())
        }
        ipc::Response::Tasks { tasks } if !tasks.is_empty() => {
            println!("added #{}: {}", tasks[0].id, tasks[0].title);
            Ok(())
        }
        ipc::Response::Err { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

async fn try_expand_nl_task(text: &str) -> Option<(String, String, i64)> {
    let prompt = format!(
        "Expand this into a clear agentcom task. Return ONLY valid JSON, no markdown fences: \
        {{\"title\":\"...\",\"description\":\"...\",\"priority\":2}}\n\nInput: {text}"
    );
    let output = tokio::process::Command::new("claude")
        .args(["-p", &prompt])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let title = v["title"].as_str()?.to_string();
    let description = v["description"].as_str().unwrap_or("").to_string();
    let priority = v["priority"].as_i64().unwrap_or(2);
    Some((title, description, priority))
}

// ---------------------------------------------------------------------------
// Task templates: save a task config for quick reuse
// ---------------------------------------------------------------------------

fn task_templates_path(project_root: &std::path::Path) -> std::path::PathBuf {
    project_root.join(".agentcom").join("task-templates.toml")
}

async fn run_task_save_template(id: i64, name: String) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let db = paths::db_path(&root)?;
    let store = store::Store::open(&db)?;

    let task = store
        .task_get(id)?
        .with_context(|| format!("task #{id} not found"))?;

    // Load existing templates file (or start fresh).
    let tpl_path = task_templates_path(&root);
    let mut doc: toml_edit::DocumentMut = if tpl_path.exists() {
        std::fs::read_to_string(&tpl_path)?.parse()?
    } else {
        toml_edit::DocumentMut::new()
    };

    // Build the [[template]] entry.
    let mut entry = toml_edit::Table::new();
    entry.insert("name", toml_edit::value(name.clone()));
    entry.insert("title", toml_edit::value(task.title.clone()));
    entry.insert("description", toml_edit::value(task.description.clone()));
    entry.insert("priority", toml_edit::value(task.priority));

    // Append to the [[template]] array.
    let arr = doc
        .entry("template")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("'template' key exists but is not an array of tables")?;

    // Replace existing entry with the same name, or append.
    let existing = arr.iter().position(|t| {
        t.get("name").and_then(|v| v.as_str()) == Some(name.as_str())
    });
    if let Some(idx) = existing {
        *arr.get_mut(idx).unwrap() = entry;
        println!("updated template '{name}' (from task #{id}: {})", task.title);
    } else {
        arr.push(entry);
        println!("saved template '{name}' (from task #{id}: {})", task.title);
    }

    if let Some(parent) = tpl_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tpl_path, doc.to_string())?;
    Ok(())
}

async fn run_task_from_template(name: String, title_override: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = paths::find_project_root(&cwd)
        .context("no agentcom.toml found — run `agentcom init` first")?;
    let tpl_path = task_templates_path(&root);
    anyhow::ensure!(
        tpl_path.exists(),
        "no task templates found — save one first with 'agentcom task save-template <id> <name>'"
    );

    let raw = std::fs::read_to_string(&tpl_path)?;
    let doc: toml::Value = toml::from_str(&raw)?;
    let templates = doc
        .get("template")
        .and_then(|v| v.as_array())
        .context("task-templates.toml has no [[template]] entries")?;

    let tpl = templates
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
        .with_context(|| {
            let names: Vec<&str> = templates
                .iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                .collect();
            format!("template '{name}' not found — available: {}", names.join(", "))
        })?;

    let title = title_override.unwrap_or_else(|| {
        tpl.get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    });
    let description = tpl
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let priority = tpl
        .get("priority")
        .and_then(|v| v.as_integer())
        .unwrap_or(2);

    let mut client = ipc::client::Client::connect()
        .await
        .context("hub not running — start it with: agentcom up")?;

    let resp = client
        .request(&ipc::Request::TaskAdd {
            title: title.clone(),
            description,
            priority,
            depends_on: vec![],
            timeout_mins: None,
            requires: vec![],
            recur: None,
        })
        .await?;

    match resp {
        ipc::Response::Tasks { tasks } if !tasks.is_empty() => {
            println!("created #{}: {} (from template '{name}')", tasks[0].id, tasks[0].title);
        }
        ipc::Response::Ok { message } => {
            println!("{} (from template '{name}')", message.unwrap_or(title));
        }
        ipc::Response::Err { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
    Ok(())
}
