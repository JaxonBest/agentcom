//! The hub: spawns and supervises agent children, routes messages, drives
//! the autonomy loop, and serves IPC clients. Single-owner state — the hub
//! loop is the only writer of `HubState`, so there are no locks on the hot
//! path (ring buffers are the one shared structure, owned by reader tasks).

pub mod events;
pub mod interrupt;
pub mod scheduler;

use crate::agent::{spawn, AgentRuntime, AgentState, WriterCmd};
use crate::config::HubConfig;
use crate::ipc::server::{Buffers, IpcServer};
use crate::ipc::{AgentStatusRow, Request, Response};
use crate::protocol::event::CliEvent;
use crate::store::Store;
use crate::tui::ringbuf::RingBuf;
use anyhow::{Context, Result};
use events::{HubEvent, IpcMsg, UiEvent};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

const STOP_GRACE: Duration = Duration::from_secs(5);

pub struct Hub {
    pub cfg: HubConfig,
    pub project_root: PathBuf,
    store: Arc<Store>,
    claude_exe: PathBuf,
    codex_exe: PathBuf,
    codex_adapter_exe: PathBuf,
    deepseek_adapter_exe: PathBuf,
    agents: HashMap<String, AgentRuntime>,
    /// Cumulative cost/turn baselines: `total_cost_usd` in result events is
    /// cumulative per session, so per-agent totals are base + latest.
    session_base: HashMap<String, (f64, u64)>,
    buffers: Buffers,
    bus_tx: mpsc::Sender<HubEvent>,
    bus_rx: mpsc::Receiver<HubEvent>,
    ipc_rx: mpsc::Receiver<IpcMsg>,
    pub ipc_tx: mpsc::Sender<IpcMsg>,
    pub ui_tx: broadcast::Sender<UiEvent>,
    ipc_port: u16,
    ipc_token: String,
    hub_json: PathBuf,
    /// Task ids currently offered to a working agent (avoid double-suggesting).
    suggested: HashMap<String, i64>,
    /// Tasks an agent was offered but didn't claim — never re-offered to the
    /// same agent (prevents an infinite suggest/decline feed loop). An entry
    /// clears when the task changes status.
    declined: HashMap<String, std::collections::HashSet<i64>>,
    stop_deadlines: HashMap<String, Instant>,
    shutting_down: bool,
    free: Option<crate::config::FreeMode>,
    started: Instant,
    /// Last time the composer was nudged in free mode (rate-limits nudges).
    last_nudge: Instant,
    /// Latest observed 5h usage-limit percentage (from rate_limit_event).
    usage_observed_pct: Option<f64>,
}

impl Hub {
    pub async fn new(
        cfg: HubConfig,
        project_root: PathBuf,
        free: Option<crate::config::FreeMode>,
    ) -> Result<Self> {
        let store = Arc::new(Store::open(&crate::paths::db_path(&project_root)?)?);
        let agentcom_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let uses_claude = cfg
            .agents
            .iter()
            .any(|a| cfg.agent_provider(a) == crate::config::AgentProvider::Claude);
        let uses_codex = cfg
            .agents
            .iter()
            .any(|a| cfg.agent_provider(a) == crate::config::AgentProvider::Codex);
        let uses_deepseek = cfg
            .agents
            .iter()
            .any(|a| cfg.agent_provider(a) == crate::config::AgentProvider::Deepseek);
        let claude_exe = if uses_claude {
            spawn::resolve_claude_exe()?
        } else {
            PathBuf::from("claude")
        };
        let codex_exe = if uses_codex {
            spawn::resolve_codex_exe()?
        } else {
            PathBuf::from("codex")
        };
        let codex_adapter_exe = if uses_codex {
            spawn::resolve_codex_adapter_exe(agentcom_dir.as_deref())?
        } else {
            PathBuf::from("agentcom-codex-adapter")
        };
        let deepseek_adapter_exe = if uses_deepseek {
            spawn::resolve_deepseek_adapter_exe(agentcom_dir.as_deref())?
        } else {
            PathBuf::from("agentcom-deepseek-adapter")
        };

        let (bus_tx, bus_rx) = mpsc::channel::<HubEvent>(1024);
        let (ipc_tx, ipc_rx) = mpsc::channel::<IpcMsg>(256);
        let (ui_tx, _) = broadcast::channel::<UiEvent>(4096);
        let buffers: Buffers = Arc::new(RwLock::new(HashMap::new()));

        let server = IpcServer::bind(
            project_root.clone(),
            ipc_tx.clone(),
            buffers.clone(),
            ui_tx.clone(),
        )
        .await?;
        let ipc_port = server.info.port;
        let ipc_token = server.info.token.clone();
        let hub_json = crate::paths::hub_json_path(&project_root)?;
        server
            .write_hub_json(&hub_json)
            .context("writing hub.json")?;
        tokio::spawn(server.run());

        let mut agents = HashMap::new();
        for a in &cfg.agents {
            let buf = Arc::new(RwLock::new(RingBuf::new()));
            buffers.write().unwrap().insert(a.name.clone(), buf.clone());
            agents.insert(a.name.clone(), AgentRuntime::new(a.clone(), buf));
        }

        Ok(Self {
            cfg,
            project_root,
            store,
            claude_exe,
            codex_exe,
            codex_adapter_exe,
            deepseek_adapter_exe,
            agents,
            session_base: HashMap::new(),
            buffers,
            bus_tx,
            bus_rx,
            ipc_rx,
            ipc_tx,
            ui_tx,
            ipc_port,
            ipc_token,
            hub_json,
            suggested: HashMap::new(),
            declined: HashMap::new(),
            stop_deadlines: HashMap::new(),
            shutting_down: false,
            free,
            started: Instant::now(),
            last_nudge: Instant::now(),
            usage_observed_pct: None,
        })
    }

    pub fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    pub fn buffers(&self) -> Buffers {
        self.buffers.clone()
    }

    /// Spawn every configured agent (or the named subset), in config order —
    /// the composer is first, so it gets first crack at seeded goals.
    pub fn spawn_agents(&mut self, only: Option<&[String]>) -> Result<()> {
        // Fresh session: reset any tasks/file-claims left over from the
        // previous run so agents don't silently resume old work on restart.
        let released = self.store.release_all_claimed().unwrap_or(0);
        let freed = self.store.files_release_all_agents().unwrap_or(0);
        if released > 0 {
            self.log(format!(
                "session start: reset {released} claimed task(s) to open"
            ));
        }
        if freed > 0 {
            self.log(format!(
                "session start: cleared {freed} stale file claim(s)"
            ));
        }

        let names: Vec<String> = self
            .cfg
            .agents
            .iter()
            .map(|a| a.name.clone())
            .filter(|n| {
                n == crate::config::COMPOSER_NAME
                    || only.map(|o| o.iter().any(|x| x == n)).unwrap_or(true)
            })
            .collect();
        for name in names {
            self.spawn_agent(&name, None)?;
        }
        Ok(())
    }

    fn spawn_agent(&mut self, name: &str, resume: Option<String>) -> Result<()> {
        let agent_cfg = self
            .cfg
            .agent(name)
            .with_context(|| format!("unknown agent {name:?}"))?
            .clone();
        let session_id = uuid::Uuid::new_v4().to_string();
        let sys_prompt =
            crate::prompt::system_prompt_append(&self.cfg, &agent_cfg, self.free.as_ref());
        let agentcom_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));

        // Isolate agent `cargo` builds from the live hub's binaries. agentcom
        // runs out of its own `target/`; an agent rebuilding the project would
        // otherwise relink and lock the running `agentcom.exe`/adapter exes and
        // take the hub down. Honor an explicit user CARGO_TARGET_DIR if set.
        let cargo_target_dir = if std::env::var_os("CARGO_TARGET_DIR").is_some() {
            None
        } else {
            crate::paths::data_dir(&self.project_root)
                .ok()
                .map(|d| d.join("agent-target"))
        };

        let child = spawn::spawn_agent(&spawn::SpawnSpec {
            hub_cfg: &self.cfg,
            agent_cfg: &agent_cfg,
            claude_exe: &self.claude_exe,
            codex_exe: &self.codex_exe,
            codex_adapter_exe: &self.codex_adapter_exe,
            deepseek_adapter_exe: &self.deepseek_adapter_exe,
            project_root: &self.project_root,
            session_id: &session_id,
            resume_session: resume.as_deref(),
            system_prompt_append: &sys_prompt,
            ipc_port: self.ipc_port,
            ipc_token: &self.ipc_token,
            agentcom_dir: agentcom_dir.as_deref(),
            cargo_target_dir: cargo_target_dir.as_deref(),
        })?;

        let rt = self.agents.get_mut(name).expect("agent exists");
        let base = (rt.spent_usd, rt.turns);
        self.session_base.insert(name.to_string(), base);

        let handles = crate::agent::io_tasks::attach(
            name.to_string(),
            child,
            rt.out_buf.clone(),
            self.bus_tx.clone(),
            self.ui_tx.clone(),
        );
        rt.stdin_tx = Some(handles.stdin_tx);
        rt.child_pid = handles.pid;
        rt.session_id = Some(session_id);
        rt.state = AgentState::Idle;
        rt.state_detail = Some(match &resume {
            Some(s) => format!("resuming {s}"),
            None => "starting".to_string(),
        });
        self.emit_state(name);
        self.log(format!("spawned agent {name}"));
        // Feed eagerly: the child emits nothing (not even init) until its
        // first user message, so gating on init would deadlock.
        self.try_feed(name);
        Ok(())
    }

    fn emit_state(&self, name: &str) {
        if let Some(rt) = self.agents.get(name) {
            let _ = self.ui_tx.send(UiEvent::StateChange {
                agent: name.to_string(),
                state: rt.state.as_str().to_string(),
                detail: rt.state_detail.clone(),
            });
        }
    }

    fn log(&self, msg: String) {
        tracing::info!("{msg}");
        let _ = self.ui_tx.send(UiEvent::HubLog(msg));
    }

    /// Main loop. Returns when shutdown completes.
    pub async fn run(&mut self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Catch SIGTERM so we log why the hub is dying and do a graceful
        // shutdown instead of being killed silently. The `if !sigterm_fired`
        // guard disables the branch after the first delivery so we don't
        // re-poll a completed future on subsequent iterations.
        let sigterm = hub_sigterm_signal();
        tokio::pin!(sigterm);
        let mut sigterm_fired = false;

        loop {
            tokio::select! {
                Some(ev) = self.bus_rx.recv() => self.handle_bus_event(ev),
                Some(msg) = self.ipc_rx.recv() => self.handle_ipc(msg),
                _ = tick.tick() => self.handle_tick(),
                _ = tokio::signal::ctrl_c() => {
                    self.log("ctrl-c received — shutting down".into());
                    self.begin_shutdown();
                }
                _ = &mut sigterm, if !sigterm_fired => {
                    sigterm_fired = true;
                    self.log("SIGTERM received — shutting down".into());
                    self.begin_shutdown();
                }
            }
            if self.shutting_down && self.all_agents_down() {
                break;
            }
        }

        let _ = std::fs::remove_file(&self.hub_json);
        let _ = self.ui_tx.send(UiEvent::Shutdown);
        Ok(())
    }

    fn all_agents_down(&self) -> bool {
        self.agents.values().all(|a| !a.is_running())
    }

    fn handle_bus_event(&mut self, ev: HubEvent) {
        match ev {
            HubEvent::Cli { agent, event } => self.handle_cli_event(&agent, event),
            HubEvent::CliRaw { agent, line } => {
                tracing::debug!(agent = %agent, line, "raw cli line");
            }
            HubEvent::Stderr { agent, line } => {
                tracing::debug!(agent = %agent, line, "agent stderr");
            }
            HubEvent::Exited { agent, code } => self.handle_exit(&agent, code),
        }
    }

    fn handle_cli_event(&mut self, agent: &str, event: CliEvent) {
        match event {
            CliEvent::System {
                subtype,
                session_id,
                ..
            } if subtype == "init" => {
                let Some(rt) = self.agents.get_mut(agent) else {
                    return;
                };
                if let Some(sid) = session_id {
                    rt.session_id = Some(sid);
                }
                if rt
                    .state_detail
                    .as_deref()
                    .is_some_and(|d| d == "starting" || d.starts_with("resuming"))
                {
                    rt.state_detail = None;
                    self.emit_state(agent);
                }
                let Some(rt) = self.agents.get(agent) else {
                    return;
                };
                if let Some(sid) = rt.session_id.clone() {
                    let _ = self
                        .store
                        .record_run_start(agent, &sid, crate::store::now_ts());
                }
            }
            CliEvent::Result {
                subtype,
                total_cost_usd,
                num_turns,
                ..
            } => self.handle_result(agent, &subtype, total_cost_usd, num_turns),
            CliEvent::RateLimitEvent { rate_limit_info } => {
                if let Some(pct) = crate::protocol::event::rate_limit_pct(&rate_limit_info) {
                    // A fresh "allowed" with no percentage only means "not
                    // blocked" — don't let it mask a higher prior reading
                    // unless it carries real utilization data.
                    let has_numeric = ["utilization", "used_percent", "usedPercent", "percentUsed"]
                        .iter()
                        .any(|k| rate_limit_info.get(k).is_some());
                    if has_numeric || pct > self.usage_observed_pct.unwrap_or(0.0) {
                        self.usage_observed_pct = Some(pct);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_result(
        &mut self,
        agent: &str,
        subtype: &str,
        total_cost_usd: Option<f64>,
        num_turns: Option<u64>,
    ) {
        let base = self.session_base.get(agent).copied().unwrap_or((0.0, 0));
        let Some(rt) = self.agents.get_mut(agent) else {
            return;
        };

        if let Some(cost) = total_cost_usd {
            rt.spent_usd = base.0 + cost;
        }
        if let Some(turns) = num_turns {
            rt.turns = base.1 + turns;
        }
        if let Some(sid) = rt.session_id.clone() {
            let _ = self.store.record_run_end(
                agent,
                &sid,
                rt.spent_usd - base.0,
                rt.turns - base.1,
                subtype,
                crate::store::now_ts(),
            );
        }
        // A suggestion the agent didn't claim is never re-offered to it —
        // but it must be re-offered to everyone else, or an open task can
        // strand while the rest of the fleet idles.
        let mut declined_open_task = false;
        if let Some(tid) = self.suggested.remove(agent) {
            let still_open = self
                .store
                .task_get(tid)
                .ok()
                .flatten()
                .map(|t| t.status == crate::store::TaskStatus::Open)
                .unwrap_or(false);
            if still_open {
                self.declined
                    .entry(agent.to_string())
                    .or_default()
                    .insert(tid);
                declined_open_task = true;
            }
        }

        let rt = self.agents.get_mut(agent).expect("agent exists");
        let was_interrupting = rt.state == AgentState::Interrupting;
        rt.interrupt_deadline = None;

        if rt.pause_requested {
            rt.pause_requested = false;
            rt.state = AgentState::Paused;
            rt.state_detail = None;
            self.emit_state(agent);
            return;
        }
        if self.shutting_down || rt.state == AgentState::Stopped {
            // During a graceful shutdown (finish_tasks=true), a working agent
            // that finishes its current turn is moved to Stopped so the hub
            // can eventually exit. Idle agents were already stopped by
            // begin_graceful_shutdown; working ones reach here naturally.
            if self.shutting_down && rt.is_running() {
                rt.state = AgentState::Stopped;
                rt.state_detail = None;
                // Close stdin and register a force-kill deadline so child
                // processes are not orphaned after the hub exits.
                if let Some(tx) = &rt.stdin_tx {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(WriterCmd::Close).await;
                    });
                }
                self.stop_deadlines
                    .insert(agent.to_string(), Instant::now() + STOP_GRACE);
                self.emit_state(agent);
                self.log(format!("{agent}: finished task, shutting down"));
            }
            return;
        }

        if was_interrupting {
            self.log(format!("{agent}: turn aborted, delivering urgent message"));
        }
        // Turn over — back to idle, then immediately try to feed.
        let rt = self.agents.get_mut(agent).expect("agent exists");
        rt.pending_urgent = false;
        rt.state = AgentState::Idle;
        self.emit_state(agent);
        self.try_feed(agent);
        if declined_open_task {
            self.wake_idle();
        }
    }

    fn handle_exit(&mut self, agent: &str, code: Option<i32>) {
        self.stop_deadlines.remove(agent);
        let Some(rt) = self.agents.get_mut(agent) else {
            return;
        };
        rt.stdin_tx = None;
        rt.child_pid = None;

        if rt.state == AgentState::Stopped || self.shutting_down {
            rt.state = AgentState::Stopped;
            rt.state_detail = None;
            self.emit_state(agent);
            self.log(format!("{agent}: stopped"));
            return;
        }

        let was_interrupting = rt.state == AgentState::Interrupting;
        rt.state = AgentState::Crashed;
        rt.state_detail = code.map(|c| format!("exit code {c}"));
        self.emit_state(agent);
        self.log(format!(
            "{agent}: process exited unexpectedly (code {code:?})"
        ));

        // Crash-loop cap: at most 5 restarts per rolling hour.
        let now = Instant::now();
        let rt = self.agents.get_mut(agent).expect("agent exists");
        if rt
            .restart_window_start
            .map(|t| now.duration_since(t) > Duration::from_secs(3600))
            .unwrap_or(true)
        {
            rt.restart_window_start = Some(now);
            rt.restarts_this_hour = 0;
        }

        let should_restart = (rt.cfg.auto_restart || was_interrupting) && rt.restarts_this_hour < 5;
        if should_restart {
            rt.restarts_this_hour += 1;
            let resume = rt.session_id.clone();
            let name = agent.to_string();
            self.log(format!(
                "{agent}: auto-restarting (attempt {} this hour)",
                self.agents[agent].restarts_this_hour
            ));
            if let Err(e) = self.spawn_agent(&name, resume) {
                self.log(format!("{name}: restart failed: {e}"));
                let _ = self.store.release_claims(&name);
            }
        } else {
            // Stranded work goes back on the board, file claims free up.
            let released = self.store.release_claims(agent).unwrap_or(0);
            if released > 0 {
                self.log(format!(
                    "{agent}: released {released} claimed task(s) back to open"
                ));
                let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
            }
            let freed = self.store.files_release(agent, &[], true).unwrap_or(0);
            if freed > 0 {
                self.log(format!("{agent}: released {freed} file claim(s)"));
            }
        }
    }

    fn handle_tick(&mut self) {
        // Interrupt escalation: no result within the timeout -> tree kill
        // (the Exited handler then auto-restarts with --resume).
        let now = Instant::now();
        let expired: Vec<String> = self
            .agents
            .iter()
            .filter(|(_, rt)| {
                rt.state == AgentState::Interrupting
                    && rt.interrupt_deadline.map(|d| now >= d).unwrap_or(false)
            })
            .map(|(n, _)| n.clone())
            .collect();
        for name in expired {
            self.log(format!(
                "{name}: interrupt timed out — force-killing process tree"
            ));
            if let Some(pid) = self.agents[&name].child_pid {
                tokio::spawn(spawn::kill_tree(pid));
            }
        }

        // Stop-grace escalation.
        let overdue: Vec<(String, u32)> = self
            .stop_deadlines
            .iter()
            .filter(|(_, d)| now >= **d)
            .filter_map(|(n, _)| {
                self.agents
                    .get(n)
                    .and_then(|a| a.child_pid)
                    .map(|p| (n.clone(), p))
            })
            .collect();
        for (name, pid) in overdue {
            self.stop_deadlines.remove(&name);
            self.log(format!("{name}: stop grace expired — force-killing"));
            tokio::spawn(spawn::kill_tree(pid));
        }

        // Global budget enforcement.
        if let Some(max) = self.cfg.max_total_budget_usd {
            let total: f64 = self.agents.values().map(|a| a.spent_usd).sum();
            if total >= max && !self.shutting_down {
                self.log(format!(
                    "global budget ${max:.2} exhausted (spent ${total:.2}) — shutting down"
                ));
                self.begin_shutdown();
            }
        }

        self.free_mode_tick();
    }

    /// Free mode: enforce time/usage stop conditions; when the whole fleet
    /// goes idle, nudge the composer to generate the next round of work.
    fn free_mode_tick(&mut self) {
        let Some(free) = self.free.clone() else {
            return;
        };
        if self.shutting_down {
            return;
        }

        if let Some(limit) = free.duration {
            if self.started.elapsed() >= limit {
                self.log(format!(
                    "free mode: time limit ({}m) reached — shutting down",
                    limit.as_secs() / 60
                ));
                if free.finish_tasks {
                    self.begin_graceful_shutdown();
                } else {
                    self.begin_shutdown();
                }
                return;
            }
        }
        if let (Some(threshold), Some(observed)) = (free.usage_pct, self.usage_observed_pct) {
            if observed >= threshold {
                self.log(format!(
                    "free mode: 5h usage limit at {observed:.0}% (threshold {threshold:.0}%) — shutting down"
                ));
                if free.finish_tasks {
                    self.begin_graceful_shutdown();
                } else {
                    self.begin_shutdown();
                }
                return;
            }
        }

        let nudge_interval = Duration::from_secs(
            std::env::var("AGENTCOM_FREE_NUDGE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        );
        let all_idle = self
            .agents
            .values()
            .filter(|a| a.is_running())
            .all(|a| a.state == AgentState::Idle);
        let composer_ready = self
            .agents
            .get(crate::config::COMPOSER_NAME)
            .map(|a| a.state == AgentState::Idle)
            .unwrap_or(false);
        if all_idle && composer_ready && self.last_nudge.elapsed() >= nudge_interval {
            self.last_nudge = Instant::now();
            let mut remaining = Vec::new();
            if let Some(limit) = free.duration {
                let left = limit.saturating_sub(self.started.elapsed());
                remaining.push(format!("{}m of time", left.as_secs() / 60));
            }
            if let Some(max) = self.cfg.max_total_budget_usd {
                let total: f64 = self.agents.values().map(|a| a.spent_usd).sum();
                remaining.push(format!("${:.2} of budget", (max - total).max(0.0)));
            }
            let remaining = if remaining.is_empty() {
                "no explicit limits".to_string()
            } else {
                format!("{} remaining", remaining.join(", "))
            };
            let nudge = format!(
                "[FREE MODE] Standing goal: {goal}\n\
                 The whole team is idle ({remaining}). Review `agentcom task list` and the codebase, \
                 then queue the next most valuable work toward the goal (decompose into file-disjoint \
                 tasks; recruit if it parallelizes). Quality over quantity — if nothing genuinely \
                 valuable remains, tell the human via `agentcom send human` and do not invent busywork.",
                goal = free.goal,
            );
            self.log("free mode: fleet idle — nudging composer".to_string());
            self.do_send("hub", crate::config::COMPOSER_NAME, &nudge, false);
        }
    }

    fn handle_ipc(&mut self, msg: IpcMsg) {
        let IpcMsg {
            identity,
            request,
            reply,
        } = msg;
        let resp = self.dispatch(&identity, request);
        let _ = reply.send(resp);
    }

    fn dispatch(&mut self, identity: &str, request: Request) -> Response {
        tracing::debug!(identity, request = ?request, "ipc request");
        match request {
            Request::Hello { .. } => Response::err("unexpected hello"),
            Request::Send { to, body, urgent } => self.do_send(identity, &to, &body, urgent),
            Request::Inbox => match self.store.msg_take_pending(identity) {
                Ok(messages) => Response::Inbox { messages },
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskAdd {
                title,
                description,
                priority,
                depends_on,
            } => match self
                .store
                .task_add(&title, &description, priority, &depends_on, identity)
            {
                Ok(id) => {
                    self.log(format!("{identity} added task #{id}: {title}"));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    self.wake_idle();
                    Response::ok_msg(format!("created task #{id}"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskList { status, search } => {
                let status = match status.as_deref() {
                    None => None,
                    Some(s) => match crate::store::TaskStatus::parse(s) {
                        Some(st) => Some(st),
                        None => return Response::err(format!("unknown status {s:?}")),
                    },
                };
                match self.store.task_list(status, search.as_deref()) {
                    Ok(tasks) => Response::Tasks { tasks },
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskClaim { id } => match self.store.task_claim(id, identity) {
                Ok(t) => {
                    self.clear_declined(id);
                    self.log(format!("{identity} claimed task #{} — {}", t.id, t.title));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("claimed task #{} — {}", t.id, t.title))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskDone { id, note } => {
                match self.store.task_done(id, identity, note.as_deref()) {
                    Ok(t) => {
                        self.log(format!("{identity} completed task #{} — {}", t.id, t.title));
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        // Close the loop: whoever filed the task hears that
                        // it's finished (the composer relies on this to
                        // report completions to the human).
                        if t.created_by != identity && self.agents.contains_key(&t.created_by) {
                            let body = format!(
                                "task #{} \"{}\" completed by {}{}",
                                t.id,
                                t.title,
                                identity,
                                t.note
                                    .as_deref()
                                    .map(|n| format!(" — {n}"))
                                    .unwrap_or_default()
                            );
                            let creator = t.created_by.clone();
                            self.do_send("hub", &creator, &body, false);
                        }
                        // A completed dependency may unblock queued work.
                        self.wake_idle();
                        Response::ok_msg(format!("task #{} done — {}", t.id, t.title))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskBlock { id, reason } => {
                match self.store.task_block(id, identity, &reason) {
                    Ok(t) => {
                        self.log(format!("{identity} blocked task #{}: {reason}", t.id));
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        Response::ok_msg(format!("task #{} blocked", t.id))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskReopen { id } => match self.store.task_reopen(id) {
                Ok(t) => {
                    self.clear_declined(id);
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    self.wake_idle();
                    Response::ok_msg(format!("task #{} reopened", t.id))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskEdit {
                id,
                title,
                description,
                priority,
            } => match self
                .store
                .task_update(id, title.as_deref(), description.as_deref(), priority)
            {
                Ok(task) => Response::Tasks {
                    tasks: vec![task],
                },
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskGet { id } => match self.store.task_get(id) {
                Ok(Some(task)) => Response::Tasks {
                    tasks: vec![task],
                },
                Ok(None) => Response::err(format!("task #{id} not found")),
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskDelete { id } => match self.store.task_delete(id) {
                Ok(()) => Response::ok_msg(format!("task #{id} deleted")),
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskPrune { before_secs } => match self.store.task_prune(before_secs) {
                Ok(count) => Response::Pruned { count },
                Err(e) => Response::err(e.to_string()),
            },
            Request::Status => self.status_response(),
            Request::AgentAdd { config } => self.add_agent_live(identity, config),
            Request::FilesClaim { paths } => match self.store.files_claim(identity, &paths) {
                Ok(()) => Response::ok_msg(format!("claimed {} file(s)", paths.len())),
                Err(conflicts) => {
                    let detail: Vec<String> = conflicts
                        .iter()
                        .map(|c| format!("{} (held by {})", c.path, c.agent))
                        .collect();
                    Response::err(format!(
                        "claim rejected — already held: {}. Coordinate with the holder via `agentcom send` before touching these files.",
                        detail.join(", ")
                    ))
                }
            },
            Request::FilesRelease { paths, all } => {
                let claimed_paths = self
                    .store
                    .files_list_for_agent(identity)
                    .unwrap_or_default();
                match self.store.files_release(identity, &paths, all) {
                    Ok(n) => {
                        // Auto-commit any changed files if enabled
                        if self.cfg.auto_commit && !claimed_paths.is_empty() {
                            self.auto_commit_changes(identity, &claimed_paths);
                        }
                        Response::ok_msg(format!("released {n} file claim(s)"))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::FilesList => match self.store.files_list() {
                Ok(claims) => Response::Files { claims },
                Err(e) => Response::err(e.to_string()),
            },
            Request::Stop { agent } => match agent {
                Some(name) => self.stop_agent(&name),
                None => {
                    if identity != "human" {
                        self.log(format!(
                            "WARN: {identity} attempted to stop the hub — rejected (only the human operator can stop the hub)"
                        ));
                        return Response::err(
                            "only the human operator can stop the hub; \
                             use `agentcom stop <agent-name>` to stop a specific agent",
                        );
                    }
                    self.begin_shutdown();
                    Response::ok_msg("hub shutting down")
                }
            },
            Request::Pause { agent } => self.pause_agent(&agent),
            Request::Resume { agent } => self.resume_agent(&agent),
            Request::Tail { .. } => Response::err("tail is handled by the ipc server"),
        }
    }

    /// Hot-add an agent while the hub is running. Existing agents learn
    /// about the newcomer when they next check `agentcom status`; their
    /// system prompts list teammates from spawn time only.
    fn add_agent_live(
        &mut self,
        requested_by: &str,
        config: crate::config::AgentConfig,
    ) -> Response {
        if let Err(e) = crate::config::validate_agent_name(&config.name) {
            return Response::err(e.to_string());
        }
        if self.agents.contains_key(&config.name) {
            return Response::err(format!("agent {:?} already exists", config.name));
        }
        if self.agents.len() >= self.cfg.max_agents {
            return Response::err(format!(
                "fleet is at max_agents = {} — decompose into tasks instead, or raise the cap in agentcom.toml",
                self.cfg.max_agents
            ));
        }
        let name = config.name.clone();
        self.log(format!(
            "{requested_by} recruited new agent {name} (role: {})",
            config.role
        ));
        let buf = Arc::new(RwLock::new(RingBuf::new()));
        self.buffers
            .write()
            .unwrap()
            .insert(name.clone(), buf.clone());
        self.cfg.agents.push(config.clone());
        self.agents
            .insert(name.clone(), AgentRuntime::new(config, buf));
        match self.spawn_agent(&name, None) {
            Ok(()) => Response::ok_msg(format!("agent {name} is live")),
            Err(e) => {
                self.log(format!("{name}: live spawn failed: {e}"));
                Response::err(format!("added but failed to spawn: {e}"))
            }
        }
    }

    /// A task whose status changed is eligible for re-suggestion everywhere.
    fn clear_declined(&mut self, task_id: i64) {
        for set in self.declined.values_mut() {
            set.remove(&task_id);
        }
    }

    fn status_response(&self) -> Response {
        let mut rows: Vec<AgentStatusRow> = self
            .cfg
            .agents
            .iter()
            .filter_map(|a| self.agents.get(&a.name))
            .map(|rt| AgentStatusRow {
                name: rt.cfg.name.clone(),
                provider: self.cfg.agent_provider(&rt.cfg).to_string(),
                state: rt.state.as_str().to_string(),
                detail: rt.state_detail.clone(),
                session_id: rt.session_id.clone(),
                spent_usd: rt.spent_usd,
                turns: rt.turns,
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Response::Status {
            project: self.cfg.project_name.clone(),
            agents: rows,
            open_tasks: self.store.open_task_count().unwrap_or(0),
            pending_msgs: self.store.msg_pending_count().unwrap_or(0),
            total_cost_usd: self.agents.values().map(|a| a.spent_usd).sum(),
            free: self.free.as_ref().map(|f| {
                let mut parts = vec![format!("goal: {}", f.goal)];
                if let Some(limit) = f.duration {
                    let left = limit.saturating_sub(self.started.elapsed());
                    parts.push(format!("{}m left", left.as_secs() / 60));
                }
                if let (Some(threshold), observed) = (f.usage_pct, self.usage_observed_pct) {
                    parts.push(format!(
                        "usage {:.0}%/{threshold:.0}%",
                        observed.unwrap_or(0.0)
                    ));
                }
                parts.join(" · ")
            }),
        }
    }

    fn stop_agent(&mut self, name: &str) -> Response {
        let Some(rt) = self.agents.get_mut(name) else {
            return Response::err(format!("unknown agent {name:?}"));
        };
        if !rt.is_running() {
            return Response::ok_msg(format!("{name} is not running"));
        }
        rt.state = AgentState::Stopped;
        rt.state_detail = None;
        if let Some(tx) = &rt.stdin_tx {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(WriterCmd::Close).await;
            });
        }
        self.stop_deadlines
            .insert(name.to_string(), Instant::now() + STOP_GRACE);
        self.emit_state(name);
        Response::ok_msg(format!("stopping {name}"))
    }

    fn pause_agent(&mut self, name: &str) -> Response {
        let Some(rt) = self.agents.get_mut(name) else {
            return Response::err(format!("unknown agent {name:?}"));
        };
        match rt.state {
            AgentState::Idle => {
                rt.state = AgentState::Paused;
                self.emit_state(name);
                Response::ok_msg(format!("{name} paused"))
            }
            AgentState::Working | AgentState::Interrupting => {
                rt.pause_requested = true;
                Response::ok_msg(format!("{name} will pause when its current turn ends"))
            }
            AgentState::Paused => Response::ok_msg(format!("{name} is already paused")),
            _ => Response::err(format!("{name} is {}", rt.state.as_str())),
        }
    }

    fn resume_agent(&mut self, name: &str) -> Response {
        let Some(rt) = self.agents.get_mut(name) else {
            return Response::err(format!("unknown agent {name:?}"));
        };
        rt.pause_requested = false;
        if rt.state == AgentState::Paused {
            rt.state = AgentState::Idle;
            self.emit_state(name);
            self.try_feed(name);
            Response::ok_msg(format!("{name} resumed"))
        } else {
            Response::ok_msg(format!("{name} is {}", rt.state.as_str()))
        }
    }

    fn begin_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        let names: Vec<String> = self.agents.keys().cloned().collect();
        for name in names {
            if self.agents[&name].is_running() {
                self.stop_agent(&name);
            }
        }
    }

    /// Graceful shutdown: set `shutting_down` but only stop idle agents.
    /// Working agents are allowed to finish their current turn; they will
    /// be stopped automatically in `handle_turn_end` when they next complete.
    fn begin_graceful_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.log("free mode: stopping idle agents, letting working agents finish their current task".to_string());
        let names: Vec<String> = self.agents.keys().cloned().collect();
        for name in names {
            if self.agents[&name].state == AgentState::Idle && self.agents[&name].is_running() {
                self.stop_agent(&name);
            }
        }
    }

    /// After an agent releases file claims, auto-commit any modified files
    /// to git using the agent's name as author.
    fn auto_commit_changes(&self, agent: &str, claimed: &[crate::store::files::FileClaim]) {
        use std::process::Command;

        // Collect the claimed paths (normalized, in repo-relative form)
        let paths: Vec<&str> = claimed.iter().map(|c| c.path.as_str()).collect();

        // Ask git which of these paths have been modified (staged or unstaged)
        let modified = match Command::new("git")
            .args(["diff", "--name-only", "--cached", "--"])
            .args(&paths)
            .current_dir(&self.project_root)
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => return, // not a git repo or git not available — skip
        };
        let unstaged = match Command::new("git")
            .args(["diff", "--name-only", "--"])
            .args(&paths)
            .current_dir(&self.project_root)
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => return,
        };

        let all_modified: Vec<&str> = modified
            .lines()
            .chain(unstaged.lines())
            .filter(|l| !l.is_empty())
            .collect();
        if all_modified.is_empty() {
            return;
        }

        // Deduplicate while preserving order
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&&str> = all_modified
            .iter()
            .filter(|p| seen.insert(**p))
            .collect();

        // Stage the files
        if Command::new("git")
            .arg("add")
            .args(&unique)
            .current_dir(&self.project_root)
            .output()
            .map_or(true, |o| !o.status.success())
        {
            self.log(format!(
                "auto-commit: failed to stage files for {agent}"
            ));
            return;
        }

        // Build the author string — per-agent override, then global, then defaults
        let agent_cfg = self.cfg.agents.iter().find(|a| a.name == agent);
        let author_name = agent_cfg
            .and_then(|a| a.auto_commit_author_name.as_deref())
            .or(self.cfg.auto_commit_author_name.as_deref())
            .unwrap_or(agent);
        let default_email = format!("{agent}@agentcom.local");
        let author_email = agent_cfg
            .and_then(|a| a.auto_commit_author_email.as_deref())
            .or(self.cfg.auto_commit_author_email.as_deref())
            .unwrap_or(&default_email);
        let author = format!("{author_name} <{author_email}>");

        // Build a commit message — include claimed task title when available
        let file_count = unique.len();
        let files_str = if file_count == 1 {
            "1 file changed".to_string()
        } else {
            format!("{file_count} files changed")
        };
        let task_prefix = self
            .store
            .claimed_task(agent)
            .ok()
            .flatten()
            .map(|t| {
                let title: String = t.title.chars().take(60).collect();
                format!("task #{} {} — ", t.id, title)
            })
            .unwrap_or_default();
        let summary = format!("{agent}: {task_prefix}{files_str}");

        let mut commit_cmd = Command::new("git");
        commit_cmd
            .args(["commit", "--author", &author, "-m", &summary])
            .current_dir(&self.project_root);
        if self.cfg.auto_commit_skip_hooks {
            commit_cmd.arg("--no-verify");
        }

        match commit_cmd.output() {
            Ok(o) if o.status.success() => {
                let hash = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .find_map(|l| l.strip_prefix('['))
                    .and_then(|s| s.split(' ').nth(1))
                    .unwrap_or("unknown")
                    .to_string();
                self.log(format!(
                    "auto-commit [{hash}]: {summary} (author: {author})"
                ));
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Exit code 1 with hook output means a pre-commit hook rejected
                // the commit — surface it clearly so the operator can act.
                let hint = if o.status.code() == Some(1)
                    && (stderr.contains("hook") || stderr.contains("Hook"))
                {
                    " (pre-commit hook rejected — set auto_commit_skip_hooks=true to bypass)"
                } else {
                    ""
                };
                self.log(format!(
                    "auto-commit: git commit failed for {agent}{hint}: {stderr}"
                ));
            }
            Err(e) => {
                self.log(format!("auto-commit: failed to run git for {agent}: {e}"));
            }
        }
    }
}

/// Resolves when SIGTERM is delivered to the process (Unix), or never (other
/// platforms). Used by the hub run loop to catch external kills and log them.
async fn hub_sigterm_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}
