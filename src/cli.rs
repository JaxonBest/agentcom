//! Command-line surface. Client-mode subcommands connect to a running hub
//! over IPC; `init`/`up` are local.

use crate::ipc::client::Client;
use crate::ipc::{Request, Response};
use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentcom",
    version,
    about = "Local coordination hub for mixed Claude Code, Codex, and DeepSeek coding-agent fleets."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create an agentcom.toml in the current directory
    Init {
        /// Overwrite an existing agentcom.toml
        #[arg(long)]
        force: bool,
        /// Fleet archetype: solo, team (default), or mixed
        #[arg(long, default_value = "team")]
        template: crate::config::ConfigTemplate,
    },
    /// Start the hub and the agent fleet (TUI by default)
    Up {
        /// Only start these agents (comma-separated names)
        #[arg(long, value_delimiter = ',')]
        agents: Option<Vec<String>>,
        /// Seed task(s) for the board (repeatable)
        #[arg(long = "task")]
        tasks: Vec<String>,
        /// Run without the TUI dashboard
        #[arg(long)]
        headless: bool,
        /// FREE MODE: a standing goal the fleet keeps working toward until a
        /// stop condition fires (combine with --for / --budget / --usage)
        #[arg(long, value_name = "GOAL")]
        free: Option<String>,
        /// Stop after this much wall-clock time (e.g. 2h, 90m, 1h30m)
        #[arg(long = "for", value_name = "DURATION")]
        for_: Option<String>,
        /// Stop once total spend reaches this many USD (overrides
        /// max_total_budget_usd from agentcom.toml)
        #[arg(long)]
        budget: Option<f64>,
        /// Stop when the 5-hour usage limit reaches this percent (0-100)
        #[arg(long, value_name = "PERCENT")]
        usage: Option<f64>,
    },
    /// Show hub and agent status
    Status,
    /// Send a message to an agent (or "all")
    Send {
        to: String,
        body: String,
        /// Interrupt the recipient's current turn for immediate delivery
        #[arg(long)]
        urgent: bool,
    },
    /// Urgently message an agent, aborting its in-progress turn
    Interrupt { agent: String, body: String },
    /// Read (and consume) your pending messages
    Inbox,
    /// Task board operations
    #[command(subcommand)]
    Task(TaskCmd),
    /// Manage the agent fleet
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Advisory file claims — claim files before editing so agents don't
    /// overwrite each other's work
    #[command(subcommand)]
    Files(FilesCmd),
    /// Show an agent's recent output
    Tail {
        agent: String,
        /// Number of backlog lines
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep following live output
        #[arg(short, long)]
        follow: bool,
    },
    /// Stop one agent, or the whole hub if no agent is given
    Stop { agent: Option<String> },
    /// Pause an agent (finishes its current turn, then waits)
    Pause { agent: String },
    /// Resume a paused agent
    Resume { agent: String },
    /// Pre-flight check for all providers and config — no hub required
    Doctor,
}

#[derive(Subcommand)]
pub enum TaskCmd {
    /// Add a task to the shared board
    Add {
        title: String,
        #[arg(short, long, default_value = "")]
        description: String,
        /// 0 = highest priority
        #[arg(short, long, default_value_t = 2)]
        priority: i64,
        /// Task ids this depends on (repeatable)
        #[arg(long = "dep")]
        depends_on: Vec<i64>,
    },
    /// List tasks
    List {
        /// Filter: open | claimed | done | blocked
        #[arg(long)]
        status: Option<String>,
    },
    /// Claim an open task
    Claim { id: i64 },
    /// Mark a task done
    Done {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
    /// Mark a task blocked
    Block {
        id: i64,
        #[arg(long)]
        reason: String,
    },
    /// Reopen a blocked (or stuck-claimed) task
    Reopen { id: i64 },
}

#[derive(Subcommand)]
pub enum FilesCmd {
    /// Claim paths before editing them (rejected if another agent holds any)
    Claim {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Release your claims (specific paths, or --all)
    Release {
        paths: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show who holds what
    List,
}

#[derive(Subcommand)]
pub enum AgentCmd {
    /// Add an agent: writes it to agentcom.toml and, if the hub is running,
    /// spawns it live immediately
    Add {
        /// Agent name (lowercase letters, digits, '-', '_')
        name: String,
        /// What this agent owns and how it should behave
        #[arg(short, long)]
        role: String,
        /// Runtime provider: claude, codex, or deepseek
        #[arg(long)]
        provider: Option<crate::config::AgentProvider>,
        #[arg(short, long)]
        model: Option<String>,
        /// Allowed tools (comma-separated). Default: Bash,Read,Edit,Write,Glob,Grep
        #[arg(short, long, value_delimiter = ',')]
        tools: Option<Vec<String>>,
        /// Working directory, relative to the project root
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        #[arg(long, default_value = "acceptEdits")]
        permission_mode: String,
        /// Cumulative USD spend cap for this agent
        #[arg(long)]
        budget: Option<f64>,
        /// Max turns per fed prompt
        #[arg(long)]
        max_turns: Option<u32>,
        /// Only write the config; don't spawn into a running hub
        #[arg(long)]
        no_spawn: bool,
    },
    /// List configured agents (with live state if the hub is running)
    List,
}

#[derive(Args, Clone)]
pub struct UpArgs {
    pub agents: Option<Vec<String>>,
    pub tasks: Vec<String>,
    pub headless: bool,
}

/// Run a client-mode command against the running hub and print the result.
pub async fn run_client(command: Command) -> Result<()> {
    let mut client = Client::connect().await?;
    match command {
        Command::Status => {
            let resp = client.request(&Request::Status).await?;
            print_status(resp)
        }
        Command::Send { to, body, urgent } => {
            let resp = client.request(&Request::Send { to, body, urgent }).await?;
            print_simple(resp)
        }
        Command::Interrupt { agent, body } => {
            let resp = client
                .request(&Request::Send {
                    to: agent,
                    body,
                    urgent: true,
                })
                .await?;
            print_simple(resp)
        }
        Command::Inbox => {
            let resp = client.request(&Request::Inbox).await?;
            match resp {
                Response::Inbox { messages } if messages.is_empty() => {
                    println!("inbox empty");
                    Ok(())
                }
                Response::Inbox { messages } => {
                    for m in messages {
                        let urgency = if m.urgent { " URGENT" } else { "" };
                        println!("[{}{}] {}", m.from_who, urgency, m.body);
                    }
                    Ok(())
                }
                other => print_simple(other),
            }
        }
        Command::Task(task_cmd) => {
            let req = match task_cmd {
                TaskCmd::Add {
                    title,
                    description,
                    priority,
                    depends_on,
                } => Request::TaskAdd {
                    title,
                    description,
                    priority,
                    depends_on,
                },
                TaskCmd::List { status } => Request::TaskList { status },
                TaskCmd::Claim { id } => Request::TaskClaim { id },
                TaskCmd::Done { id, note } => Request::TaskDone { id, note },
                TaskCmd::Block { id, reason } => Request::TaskBlock { id, reason },
                TaskCmd::Reopen { id } => Request::TaskReopen { id },
            };
            let resp = client.request(&req).await?;
            match resp {
                Response::Tasks { tasks } => {
                    print_tasks(&tasks);
                    Ok(())
                }
                other => print_simple(other),
            }
        }
        Command::Tail {
            agent,
            lines,
            follow,
        } => {
            client
                .send(&Request::Tail {
                    agent,
                    lines,
                    follow,
                })
                .await?;
            loop {
                match client.next_response().await? {
                    Some(Response::TailLine { line }) => println!("{line}"),
                    Some(Response::Ok { .. }) | None => break,
                    Some(Response::Err { message }) => bail!("{message}"),
                    Some(_) => {}
                }
            }
            Ok(())
        }
        Command::Files(files_cmd) => {
            let req = match files_cmd {
                FilesCmd::Claim { paths } => Request::FilesClaim { paths },
                FilesCmd::Release { paths, all } => Request::FilesRelease { paths, all },
                FilesCmd::List => Request::FilesList,
            };
            let resp = client.request(&req).await?;
            match resp {
                Response::Files { claims } if claims.is_empty() => {
                    println!("no file claims");
                    Ok(())
                }
                Response::Files { claims } => {
                    for c in claims {
                        println!("{:<14} {}", c.agent, c.path);
                    }
                    Ok(())
                }
                other => print_simple(other),
            }
        }
        Command::Stop { agent } => {
            let resp = client.request(&Request::Stop { agent }).await?;
            print_simple(resp)
        }
        Command::Pause { agent } => {
            let resp = client.request(&Request::Pause { agent }).await?;
            print_simple(resp)
        }
        Command::Resume { agent } => {
            let resp = client.request(&Request::Resume { agent }).await?;
            print_simple(resp)
        }
        Command::Init { .. } | Command::Up { .. } | Command::Agent(_) | Command::Doctor => {
            unreachable!("handled in main")
        }
    }
}

/// `agentcom agent ...` — hybrid commands: they edit agentcom.toml locally
/// and talk to the hub only if one is running.
pub async fn run_agent_cmd(cmd: AgentCmd) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = crate::paths::find_project_root(&cwd)
        .ok_or_else(|| anyhow::anyhow!("no agentcom.toml found — run `agentcom init` first"))?;

    match cmd {
        AgentCmd::Add {
            name,
            role,
            model,
            provider,
            tools,
            cwd,
            permission_mode,
            budget,
            max_turns,
            no_spawn,
        } => {
            let config = crate::config::AgentConfig {
                name: name.clone(),
                role,
                cwd,
                provider,
                model,
                allowed_tools: Some(tools.unwrap_or_else(|| {
                    ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
                        .map(String::from)
                        .to_vec()
                })),
                permission_mode,
                max_turns_per_prompt: max_turns,
                max_budget_usd: budget,
                auto_restart: true,
            };
            // Validate the combined config first; only persist after the
            // hub (if running) has also accepted — a cap rejection must not
            // leave a half-added agent in the file.
            let (path, text) = crate::config::render_with_agent(&project_root, &config)?;

            if !no_spawn {
                if let Ok(mut client) = Client::connect().await {
                    match client.request(&Request::AgentAdd { config }).await? {
                        Response::Ok { message } => {
                            std::fs::write(&path, text)?;
                            println!("added {name:?} to {}", path.display());
                            println!("{}", message.unwrap_or_else(|| "live".into()));
                            return Ok(());
                        }
                        Response::Err { message } => bail!("hub rejected {name:?}: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
            }
            std::fs::write(&path, text)?;
            println!("added {name:?} to {}", path.display());
            if !no_spawn {
                println!("hub not running — {name} will start with the next `agentcom up`");
            }
            Ok(())
        }
        AgentCmd::List => {
            let cfg = crate::config::HubConfig::load(&project_root)?;
            let live: std::collections::HashMap<String, crate::ipc::AgentStatusRow> =
                match Client::connect().await {
                    Ok(mut client) => match client.request(&Request::Status).await? {
                        Response::Status { agents, .. } => {
                            agents.into_iter().map(|a| (a.name.clone(), a)).collect()
                        }
                        _ => Default::default(),
                    },
                    Err(_) => Default::default(),
                };
            for a in &cfg.agents {
                let state = live
                    .get(&a.name)
                    .map(|r| format!("{} · ${:.2} · {} turns", r.state, r.spent_usd, r.turns))
                    .unwrap_or_else(|| "not running".into());
                println!("{:<14} {state}", a.name);
                println!("      {}", a.role);
            }
            // Hub may know agents added live before the config was reloaded.
            for (name, r) in &live {
                if cfg.agent(name).is_none() {
                    println!("{:<14} {} (hub only)", name, r.state);
                }
            }
            Ok(())
        }
    }
}

fn print_simple(resp: Response) -> Result<()> {
    match resp {
        Response::Ok { message } => {
            println!("{}", message.unwrap_or_else(|| "ok".into()));
            Ok(())
        }
        Response::Err { message } => bail!("{message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_status(resp: Response) -> Result<()> {
    match resp {
        Response::Status {
            project,
            agents,
            open_tasks,
            pending_msgs,
            total_cost_usd,
            free,
        } => {
            println!(
                "{project} — {} agent(s), {open_tasks} open task(s), {pending_msgs} pending message(s), ${total_cost_usd:.2} spent",
                agents.len()
            );
            if let Some(free) = free {
                println!("  FREE MODE · {free}");
            }
            for a in agents {
                let detail = a.detail.map(|d| format!(" — {d}")).unwrap_or_default();
                println!(
                    "  {:<14} {:<6} {:<13} ${:<8.2} {} turns{detail}",
                    a.name, a.provider, a.state, a.spent_usd, a.turns
                );
            }
            Ok(())
        }
        other => print_simple(other),
    }
}

fn print_tasks(tasks: &[crate::store::Task]) {
    if tasks.is_empty() {
        println!("no tasks");
        return;
    }
    for t in tasks {
        let who = t
            .claimed_by
            .as_deref()
            .map(|w| format!(" @{w}"))
            .unwrap_or_default();
        let deps = if t.depends_on.is_empty() {
            String::new()
        } else {
            format!(
                " deps:[{}]",
                t.depends_on
                    .iter()
                    .map(|d| format!("#{d}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let extra = match t.status {
            crate::store::TaskStatus::Blocked => t
                .blocked_reason
                .as_deref()
                .map(|r| format!(" — blocked: {r}"))
                .unwrap_or_default(),
            crate::store::TaskStatus::Done => t
                .note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        println!(
            "#{:<4} p{} {:<8}{who}{deps} {}{extra}",
            t.id,
            t.priority,
            t.status.as_str(),
            t.title
        );
        if !t.description.is_empty() {
            for line in t.description.lines() {
                println!("      {line}");
            }
        }
    }
}
