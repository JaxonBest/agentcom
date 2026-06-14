//! Command-line surface. Client-mode subcommands connect to a running hub
//! over IPC; `init`/`up` are local.

use crate::ipc::client::Client;
use crate::ipc::{Request, Response};
use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use std::sync::OnceLock;

pub static JSON_MODE: OnceLock<bool> = OnceLock::new();

#[derive(Parser)]
#[command(
    name = "agentcom",
    version,
    about = "Local coordination hub for mixed Claude Code, Codex, and DeepSeek coding-agent fleets."
)]
pub struct Cli {
    /// Output JSON instead of human-readable text for status and task list
    #[arg(long, global = true)]
    pub json: bool,

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
        /// Analyse the current project and suggest a tailored fleet before writing the config
        #[arg(long)]
        analyze: bool,
    },
    /// Start the hub and the agent fleet (TUI by default)
    ///
    /// Examples:
    ///   agentcom up
    ///   agentcom up --free "ship the login page" --for 2h --budget 5.00
    ///   agentcom up --headless
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
        /// In free mode, let agents finish their current task before stopping
        /// (instead of killing them immediately when the time/usage limit fires)
        #[arg(long)]
        finish_tasks: bool,
        /// Stop a running hub first, then start a fresh one. Useful after config
        /// changes without having to run `agentcom stop && agentcom up`.
        #[arg(long)]
        restart: bool,
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
    /// Read hub log files without needing a running hub
    ///
    /// Examples:
    ///   agentcom logs
    ///   agentcom logs -n 200
    ///   agentcom logs --agent builder
    ///   agentcom logs --follow
    Logs {
        /// Filter lines to those containing this agent name (case-insensitive substring)
        #[arg(long)]
        agent: Option<String>,
        /// Number of most-recent lines to show
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        /// Follow the log live (like tail -f), Ctrl+C to stop
        #[arg(short, long)]
        follow: bool,
    },
    /// Generate shell completion scripts
    ///
    /// Examples:
    ///   agentcom completions bash >> ~/.bash_completion
    ///   agentcom completions zsh > ~/.zfunc/_agentcom
    ///   agentcom completions fish > ~/.config/fish/completions/agentcom.fish
    Completions {
        shell: clap_complete::Shell,
    },
    /// Show per-agent spend and turn counts from the local DB (no hub needed)
    Budget,
    /// Print version and build metadata (git commit, build time, rustc version)
    Version,
    /// Read the loaded agentcom.toml config
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Print the loaded agentcom.toml as JSON (useful for scripting and debugging)
    Show,
    /// Modify a config value without manually editing agentcom.toml
    ///
    /// Supports top-level keys like `project_name`, `auto_commit`, `max_agents`,
    /// and nested agent fields via dot notation: `agent.<name>.model`.
    ///
    /// Examples:
    ///   agentcom config set project_name "my-app"
    ///   agentcom config set auto_commit true
    ///   agentcom config set agent.builder.model claude-sonnet-4-6
    Set {
        /// Config key path, e.g. `project_name` or `agent.builder.model`
        key: String,
        /// Value to set (booleans and numbers are auto-detected)
        value: String,
    },
}

#[derive(Subcommand)]
pub enum TaskCmd {
    /// Add a task to the shared board
    ///
    /// Examples:
    ///   agentcom task add "implement login page" -d "use React + Express"
    ///   agentcom task add "deploy to prod" -p 0 --dep 5 --dep 6
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
        /// Filter by keyword in title/description (case-insensitive)
        #[arg(short, long)]
        search: Option<String>,
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
    /// Edit a task's title, description, and/or priority
    Edit {
        id: i64,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<i64>,
    },
    /// Show a single task by id
    Show { id: i64 },
    /// Permanently delete a task (only open/done/blocked — not claimed)
    Remove { id: i64 },
    /// Delete old done/blocked tasks to keep the board tidy
    ///
    /// Examples:
    ///   agentcom task prune
    ///   agentcom task prune --before 1d
    Prune {
        /// Remove tasks not updated in this long (e.g. 7d, 24h, 90m)
        #[arg(long, default_value = "7d")]
        before: String,
    },
    /// Dump the task board without a running hub
    ///
    /// Examples:
    ///   agentcom task export
    ///   agentcom task export --format json | jq '.[] | select(.status=="open") | .title'
    Export {
        /// Output format: md (default) or json
        #[arg(long, default_value = "md")]
        format: String,
    },
    /// Velocity metrics: completion times, throughput, top contributors
    ///
    /// Examples:
    ///   agentcom task stats
    ///   agentcom task stats --json
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Send an inbox reminder to an agent about a specific task
    ///
    /// Examples:
    ///   agentcom task remind 42 builder
    ///   agentcom task remind 13 "builder-assistant"
    Remind {
        /// Task id
        id: i64,
        /// Agent to remind
        agent: String,
    },
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
    ///
    /// Examples:
    ///   agentcom agent add qa --role "writes and runs tests" --provider claude
    ///   agentcom agent add coder --role "implements features" --model claude-sonnet-4-6 --budget 5.00
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
        /// Disable automatic restart when the agent exits (default: auto-restart is on)
        #[arg(long)]
        no_auto_restart: bool,
        /// Only write the config; don't spawn into a running hub
        #[arg(long)]
        no_spawn: bool,
        /// Git author name for auto-commits by this agent (default: agent name)
        #[arg(long)]
        auto_commit_author_name: Option<String>,
        /// Git author email for auto-commits by this agent (default: <agent>@agentcom.local)
        #[arg(long)]
        auto_commit_author_email: Option<String>,
        /// Extra environment variables injected into this agent's process (repeatable)
        ///
        /// Example: --env ANTHROPIC_API_KEY=sk-... --env DEBUG=1
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,
    },
    /// List configured agents (with live state if the hub is running)
    List,
    /// Remove an agent from config (and stop it in the hub if running)
    Remove {
        /// Agent name to remove
        name: String,
        /// Update config only; don't stop the agent in a running hub
        #[arg(long)]
        no_stop: bool,
    },
}

#[derive(Args, Clone)]
pub struct UpArgs {
    pub agents: Option<Vec<String>>,
    pub tasks: Vec<String>,
    pub headless: bool,
}

/// Run a client-mode command against the running hub and print the result.
pub async fn run_client(command: Command) -> Result<()> {
    let mut client = match Client::connect().await {
        Ok(c) => c,
        Err(_) => {
            eprintln!("hub not running — start it with: agentcom up");
            return Ok(());
        }
    };
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
                TaskCmd::List { status, search } => Request::TaskList { status, search },
                TaskCmd::Claim { id } => Request::TaskClaim { id },
                TaskCmd::Done { id, note } => Request::TaskDone { id, note },
                TaskCmd::Block { id, reason } => Request::TaskBlock { id, reason },
                TaskCmd::Reopen { id } => Request::TaskReopen { id },
                TaskCmd::Edit {
                    id,
                    title,
                    description,
                    priority,
                } => Request::TaskEdit {
                    id,
                    title,
                    description,
                    priority,
                },
                TaskCmd::Show { id } => Request::TaskGet { id },
                TaskCmd::Remove { id } => Request::TaskDelete { id },
                TaskCmd::Prune { before } => {
                    let before_secs = parse_duration_secs(&before)
                        .ok_or_else(|| anyhow::anyhow!("invalid duration {:?} — use e.g. 7d, 24h, 90m, 60s", before))?;
                    Request::TaskPrune { before_secs }
                }
                TaskCmd::Remind { id, agent } => {
                    // First fetch the task to get its title
                    let get_resp = client.request(&Request::TaskGet { id }).await?;
                    let title = match get_resp {
                        Response::Tasks { tasks } if !tasks.is_empty() => tasks[0].title.clone(),
                        Response::Err { message } => {
                            eprintln!("Error: {message}");
                            return Ok(());
                        }
                        _ => {
                            eprintln!("Error: task #{id} not found");
                            return Ok(());
                        }
                    };
                    let body = format!("Reminder: task #{id} ({title}) is waiting for you");
                    let resp = client.request(&Request::Send { to: agent, body, urgent: false }).await?;
                    return print_simple(resp);
                }
                TaskCmd::Export { .. } | TaskCmd::Stats { .. } => unreachable!("handled in main"),
            };
            let resp = client.request(&req).await?;
            match resp {
                Response::Tasks { tasks } => {
                    print_tasks(&tasks);
                    Ok(())
                }
                Response::Pruned { count } => {
                    println!("pruned {count} task(s)");
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
        Command::Init { .. }
        | Command::Up { .. }
        | Command::Agent(_)
        | Command::Doctor
        | Command::Logs { .. }
        | Command::Completions { .. }
        | Command::Budget
        | Command::Version
        | Command::Config(_) => {
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
            no_auto_restart,
            no_spawn,
            auto_commit_author_name,
            auto_commit_author_email,
            env_vars,
        } => {
            let env: std::collections::BTreeMap<String, String> = env_vars
                .into_iter()
                .filter_map(|s| {
                    let (k, v) = s.split_once('=')?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect();
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
                auto_restart: !no_auto_restart,
                auto_commit_author_name,
                auto_commit_author_email,
                auto_commit: None,
                max_rpm: None,
                env,
                initial_prompt: None,
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
        AgentCmd::Remove { name, no_stop } => {
            crate::config::remove_agent(&project_root, &name)?;
            println!("removed {name:?} from agentcom.toml");
            if !no_stop {
                if let Ok(mut client) = Client::connect().await {
                    let resp = client
                        .request(&Request::Stop {
                            agent: Some(name.clone()),
                        })
                        .await?;
                    match resp {
                        Response::Ok { message } => {
                            println!("{}", message.unwrap_or_else(|| format!("{name} stopped")));
                        }
                        Response::Err { message } => {
                            eprintln!("hub: {message}");
                        }
                        _ => {}
                    }
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
    if *JSON_MODE.get().unwrap_or(&false) {
        return print_json(resp);
    }
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
                println!("  FREE MODE  {free}");
            }
            println!("\n  {:<14} {:<8} {:<13} {:<9} TURNS", "NAME", "PROVIDER", "STATE", "COST");
            for a in agents {
                let detail = a.detail.map(|d| format!("\n    {d}")).unwrap_or_default();
                println!(
                    "  {:<14} {:<8} {:<13} ${:<8.2} {} turns{detail}",
                    a.name, a.provider, a.state, a.spent_usd, a.turns
                );
            }
            Ok(())
        }
        other => print_simple(other),
    }
}

fn print_tasks(tasks: &[crate::store::Task]) {
    if *JSON_MODE.get().unwrap_or(&false) {
        println!(
            "{}",
            serde_json::to_string_pretty(tasks).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    use crate::store::TaskStatus;
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
    let open = tasks.iter().filter(|t| t.status == TaskStatus::Open).count();
    let claimed = tasks.iter().filter(|t| t.status == TaskStatus::Claimed).count();
    let done = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    let blocked = tasks.iter().filter(|t| t.status == TaskStatus::Blocked).count();
    println!("\n{open} open · {claimed} claimed · {done} done · {blocked} blocked");
}

fn print_json(resp: Response) -> Result<()> {
    match resp {
        Response::Status {
            project,
            agents,
            open_tasks,
            pending_msgs,
            total_cost_usd,
            free,
        } => {
            #[derive(serde::Serialize)]
            struct JsonStatus<'a> {
                project: &'a str,
                agents: Vec<&'a crate::ipc::AgentStatusRow>,
                open_tasks: u64,
                pending_msgs: u64,
                total_cost_usd: f64,
                free: Option<&'a str>,
            }
            let s = JsonStatus {
                project: &project,
                agents: agents.iter().collect(),
                open_tasks,
                pending_msgs,
                total_cost_usd,
                free: free.as_deref(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&s).unwrap_or_else(|_| "{}".into())
            );
            Ok(())
        }
        other => print_simple(other),
    }
}

/// Parse a human duration string like "7d", "24h", "90m", "60s" to seconds.
/// Plain integers are treated as seconds.
pub fn parse_duration_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('d') {
        n.parse::<i64>().ok().map(|v| v * 86400)
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<i64>().ok().map(|v| v * 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        n.parse::<i64>().ok().map(|v| v * 60)
    } else if let Some(n) = s.strip_suffix('s') {
        n.parse::<i64>().ok()
    } else {
        s.parse::<i64>().ok()
    }
}
