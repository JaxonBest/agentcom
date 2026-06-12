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
        Command::Init { force } => {
            let cwd = std::env::current_dir()?;
            let path = config::write_example(&cwd, force)?;
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
    let mut cfg = config::HubConfig::load(&project_root)?;

    // Every fleet gets a composer: the coordinator the human talks to in
    // the chat pane. Define your own [[agent]] name = "composer" to
    // customize it; it sits outside the max_agents worker cap.
    if cfg.agent(config::COMPOSER_NAME).is_none() {
        cfg.agents.insert(
            0,
            config::composer_default(cfg.default_model.as_deref()),
        );
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
        hub_result??;
        Ok(())
    }
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
