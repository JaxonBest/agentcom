//! The hub: spawns and supervises agent children, routes messages, drives
//! the autonomy loop, and serves IPC clients. Single-owner state — the hub
//! loop is the only writer of `HubState`, so there are no locks on the hot
//! path (ring buffers are the one shared structure, owned by reader tasks).

pub mod audit;
pub mod events;
pub mod interrupt;
pub mod rest;
pub mod scheduler;
pub mod webhook;

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
    /// Temp system-prompt files created for Codex/Deepseek agents — deleted on exit.
    temp_prompt_paths: HashMap<String, std::path::PathBuf>,
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
    /// Per-agent sliding window of prompt-send Instants for RPM tracking.
    rpm_window: HashMap<String, std::collections::VecDeque<Instant>>,
    audit: audit::AuditLog,
    /// Last observed mtime of agentcom.toml; used for hot-reload detection.
    config_mtime: Option<std::time::SystemTime>,
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

        // Optional REST API server (127.0.0.1 only, Bearer token auth).
        if let Some(rest_port) = cfg.rest_api_port {
            let rest_token = ipc_token.clone();
            tokio::spawn(rest::serve(rest_port, ipc_port, rest_token));
        }

        let mut agents = HashMap::new();
        for a in &cfg.agents {
            let buf = Arc::new(RwLock::new(RingBuf::new()));
            buffers.write().unwrap().insert(a.name.clone(), buf.clone());
            agents.insert(a.name.clone(), AgentRuntime::new(a.clone(), buf));
        }

        let data_dir = crate::paths::data_dir(&project_root)
            .unwrap_or_else(|_| project_root.join(".agentcom"));
        let audit = audit::AuditLog::new(&data_dir);

        Ok(Self {
            cfg,
            project_root,
            store,
            claude_exe,
            codex_exe,
            codex_adapter_exe,
            deepseek_adapter_exe,
            agents,
            temp_prompt_paths: HashMap::new(),
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
            rpm_window: HashMap::new(),
            audit,
            config_mtime: None,
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
        let stagger_ms = self.cfg.stagger_agents_ms.unwrap_or(0);
        for (idx, name) in names.iter().enumerate() {
            if stagger_ms > 0 && idx > 0 {
                let delay = stagger_ms * idx as u64;
                self.log(format!(
                    "stagger: waiting {delay}ms before spawning {name}"
                ));
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            self.spawn_agent(name, None)?;
        }
        Ok(())
    }

    fn spawn_agent(&mut self, name: &str, resume: Option<String>) -> Result<()> {
        let agent_cfg = self
            .cfg
            .agent(name)
            .with_context(|| format!("unknown agent {name:?}"))?
            .clone();
        if let Some(delay_ms) = agent_cfg.spawn_delay_ms {
            if delay_ms > 0 {
                self.log(format!("spawn_delay: waiting {delay_ms}ms before spawning {name}"));
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
        }
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

        let (child, temp_path) = spawn::spawn_agent(&spawn::SpawnSpec {
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
        if let Some(p) = temp_path {
            self.temp_prompt_paths.insert(name.to_string(), p);
        }

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
        self.audit.write("agent_spawn", name, serde_json::json!({"session_id": &session_id}));
        rt.session_id = Some(session_id);
        rt.state = AgentState::Idle;
        rt.state_detail = Some(match &resume {
            Some(s) => format!("resuming {s}"),
            None => "starting".to_string(),
        });
        self.emit_state(name);
        self.log(format!("spawned agent {name}"));
        // If configured, queue the initial_prompt as the agent's first inbox
        // message so it starts working on a specific goal immediately.
        if let Some(prompt) = agent_cfg.initial_prompt.as_deref() {
            let _ = self.store.msg_send("hub", &[name.to_string()], prompt, false);
        }
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

    /// Fire a webhook event if `webhook_url` is configured in agentcom.toml.
    /// Spawns a background task — never blocks the hub loop.
    fn fire_webhook(&self, payload: webhook::Payload) {
        if let Some(url) = self.cfg.webhook_url.as_deref() {
            webhook::fire(url.to_string(), self.cfg.webhook_secret.clone(), payload);
        }
    }

    /// Main loop. Returns when shutdown completes.
    pub async fn run(&mut self) -> Result<()> {
        self.audit.write("hub_start", "hub", serde_json::json!({"pid": std::process::id()}));
        self.fire_webhook(webhook::Payload::new(webhook::Event::HubStart));

        // agentcom.toml may contain webhook_secret; warn if others can read it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let cfg_path = self.project_root.join(crate::paths::CONFIG_FILE);
            if let Ok(meta) = std::fs::metadata(&cfg_path) {
                let mode = meta.permissions().mode();
                if mode & 0o044 != 0 {
                    tracing::warn!(
                        "agentcom.toml is readable by group or others (mode 0{:03o}) — consider: chmod 600 agentcom.toml",
                        mode & 0o777
                    );
                }
            }
        }

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

        self.audit.write("hub_stop", "hub", serde_json::json!({}));
        self.fire_webhook(webhook::Payload::new(webhook::Event::HubStop));
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
        // Extract config values and clone the ui_tx sender BEFORE the mutable
        // borrow of self.agents — calling self.log() inside a get_mut block
        // would borrow &self while self.agents is already mutably borrowed.
        let warn_pct = self.cfg.budget_warn_pct.unwrap_or(80.0);
        let webhook_url = self.cfg.webhook_url.clone();
        let webhook_secret = self.cfg.webhook_secret.clone();
        let ui_tx = self.ui_tx.clone();

        let base = self.session_base.get(agent).copied().unwrap_or((0.0, 0));
        let Some(rt) = self.agents.get_mut(agent) else {
            return;
        };

        if let Some(cost) = total_cost_usd {
            rt.spent_usd = base.0 + cost;
            // Per-agent budget warning: fire once when spend crosses the threshold.
            if let Some(max_budget) = rt.cfg.max_budget_usd {
                let pct = (rt.spent_usd / max_budget) * 100.0;
                if pct >= warn_pct && !rt.budget_warn_fired {
                    rt.budget_warn_fired = true;
                    let spent = rt.spent_usd;
                    let msg = format!(
                        "budget warning: {agent} at {pct:.0}% of ${max_budget:.2} budget (spent ${spent:.4})"
                    );
                    tracing::info!("{msg}");
                    let _ = ui_tx.send(UiEvent::HubLog(msg));
                    if let Some(url) = webhook_url {
                        webhook::fire(
                            url,
                            webhook_secret.clone(),
                            webhook::Payload::new(webhook::Event::BudgetWarning)
                                .with_agent(agent)
                                .with_budget(spent, max_budget, pct),
                        );
                    }
                }
            }
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
            rt.paused_at = Some(std::time::Instant::now());
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
        // Clean up the temp system-prompt file used by Codex/Deepseek agents.
        if let Some(path) = self.temp_prompt_paths.remove(agent) {
            let _ = std::fs::remove_file(&path);
        }
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
        self.audit.write("agent_crash", agent, serde_json::json!({"exit_code": code}));
        self.fire_webhook(
            webhook::Payload::new(webhook::Event::AgentCrash).with_agent(agent),
        );

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

        // Circuit breaker: track rapid consecutive crashes; pause after N in window.
        let window_secs = self.cfg.crash_circuit_breaker_window_secs;
        let max_crashes = self.cfg.crash_circuit_breaker_n;
        let rt = self.agents.get_mut(agent).expect("agent exists");
        if rt.first_crash_at.map(|t| t.elapsed().as_secs() >= window_secs).unwrap_or(false) {
            rt.crash_count = 0;
            rt.first_crash_at = None;
        }
        if rt.first_crash_at.is_none() {
            rt.first_crash_at = Some(now);
        }
        rt.crash_count += 1;
        let circuit_open = rt.crash_count >= max_crashes;

        let should_restart = !circuit_open && (rt.cfg.auto_restart || was_interrupting) && rt.restarts_this_hour < 5;
        if circuit_open {
            let count = rt.crash_count;
            let mins = window_secs / 60;
            rt.state = AgentState::Paused;
            rt.state_detail = Some(format!("circuit breaker: {count} crashes in {mins}min"));
            rt.paused_at = Some(now);
            self.emit_state(agent);
            self.audit.write("circuit_breaker_triggered", agent, serde_json::json!({"crash_count": count, "window_mins": mins}));
            self.log(format!(
                "CIRCUIT BREAKER: {agent} paused after {count} crashes in {mins}min — manual resume required"
            ));
            let msg = format!(
                "CIRCUIT BREAKER: {agent} has crashed {count} times within {mins}min and has been auto-paused. \
                 Use 'agentcom resume {agent}' to re-enable after investigating the root cause."
            );
            let _ = self.store.msg_send("hub", &["composer".to_string()], &msg, true);
            // Release stranded work and file claims.
            let released = self.store.release_claims(agent).unwrap_or(0);
            if released > 0 {
                self.log(format!("{agent}: circuit breaker released {released} claimed task(s) back to open"));
                let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
            }
            let freed = self.store.files_release(agent, &[], true).unwrap_or(0);
            if freed > 0 {
                self.log(format!("{agent}: circuit breaker released {freed} file claim(s)"));
            }
        } else if should_restart {
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
        self.check_stalls();
        self.check_config_reload();
        self.check_deadlock();
        self.check_priority_escalation();
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
            Request::Send { to, body, urgent, .. } => self.do_send(identity, &to, &body, urgent),
            Request::Inbox => match self.store.msg_take_pending(identity) {
                Ok(messages) => Response::Inbox { messages },
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskAdd {
                title,
                description,
                priority,
                depends_on,
                timeout_mins: _,
                requires,
                ..
            } => {
                if title.trim().is_empty() {
                    return Response::err("task title cannot be empty");
                }
                if title.len() > 200 {
                    return Response::err(format!("task title too long ({} chars, max 200)", title.len()));
                }
                if description.len() > 4000 {
                    return Response::err(format!("task description too long ({} chars, max 4000)", description.len()));
                }
                match self
                .store
                .task_add(&title, &description, priority, &depends_on, identity)
            {
                Ok(id) => {
                    if !requires.is_empty() {
                        if let Err(e) = self.store.task_set_requires(id, &requires) {
                            tracing::warn!("task #{id}: failed to set requires: {e}");
                        }
                    }
                    self.log(format!("{identity} added task #{id}: {title}"));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    self.wake_idle();
                    Response::ok_msg(format!("created task #{id}"))
                }
                Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskList { status, search, tag, .. } => {
                let status = match status.as_deref() {
                    None => None,
                    Some(s) => match crate::store::TaskStatus::parse(s) {
                        Some(st) => Some(st),
                        None => return Response::err(format!("unknown status {s:?}")),
                    },
                };
                match self.store.task_list_filtered(status, search.as_deref(), tag.as_deref()) {
                    Ok(tasks) => Response::Tasks { tasks },
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskClaim { id, .. } => {
                // Enforce per-agent parallel task limit if configured
                if let Some(limit) = self.cfg.agents.iter().find(|a| a.name == identity)
                    .and_then(|a| a.max_claimed_tasks)
                {
                    let count = self.store.task_list(Some(crate::store::TaskStatus::Claimed), None)
                        .map(|ts| ts.iter().filter(|t| t.claimed_by.as_deref() == Some(identity)).count())
                        .unwrap_or(0) as u64;
                    if count >= limit {
                        return Response::err(format!(
                            "parallel-task limit reached: {identity} already holds {count}/{limit} task(s)"
                        ));
                    }
                }
                match self.store.task_claim(id, identity) {
                    Ok(t) => {
                        self.clear_declined(id);
                        self.log(format!("{identity} claimed task #{} — {}", t.id, t.title));
                        self.audit.write("task_claim", identity, serde_json::json!({"task_id": t.id, "title": t.title}));
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        Response::ok_msg(format!("claimed task #{} — {}", t.id, t.title))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskDone { id, note, .. } => {
                match self.store.task_done(id, identity, note.as_deref()) {
                    Ok(t) => {
                        self.log(format!("{identity} completed task #{} — {}", t.id, t.title));
                        self.audit.write("task_done", identity, serde_json::json!({"task_id": t.id, "title": t.title}));
                        self.fire_webhook(
                            webhook::Payload::new(webhook::Event::TaskDone)
                                .with_agent(identity)
                                .with_task(t.id, &t.title),
                        );
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
            Request::TaskBlock { id, reason, .. } => {
                match self.store.task_block(id, identity, &reason) {
                    Ok(t) => {
                        self.log(format!("{identity} blocked task #{}: {reason}", t.id));
                        self.audit.write("task_blocked", identity, serde_json::json!({"task_id": t.id, "reason": reason}));
                        self.fire_webhook(
                            webhook::Payload::new(webhook::Event::TaskBlocked)
                                .with_agent(identity)
                                .with_task(t.id, &t.title),
                        );
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        Response::ok_msg(format!("task #{} blocked", t.id))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskReopen { id, .. } => match self.store.task_reopen(id) {
                Ok(t) => {
                    self.clear_declined(id);
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    self.wake_idle();
                    self.audit.write("task_reopen", identity, serde_json::json!({"task_id": t.id}));
                    Response::ok_msg(format!("task #{} reopened", t.id))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskAssign { id, agent, .. } => match self.store.task_assign(id, &agent) {
                Ok(t) => {
                    self.log(format!("{identity} assigned task #{} to {agent}", t.id));
                    self.audit.write("task_assign", identity, serde_json::json!({"task_id": t.id, "assignee": agent}));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    let msg = format!(
                        "[INBOX] task #{} assigned to you by {identity}: {}\n{}",
                        t.id,
                        t.title,
                        t.description
                            .lines()
                            .take(5)
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    self.do_send("hub", &agent, &msg, false);
                    self.wake_idle();
                    Response::ok_msg(format!("task #{} assigned to {agent}", t.id))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskEdit {
                id,
                title,
                description,
                priority,
                ..
            } => {
                if let Some(ref t) = title {
                    if t.trim().is_empty() { return Response::err("task title cannot be empty"); }
                    if t.len() > 200 { return Response::err(format!("task title too long ({} chars, max 200)", t.len())); }
                }
                if let Some(ref d) = description {
                    if d.len() > 4000 { return Response::err(format!("task description too long ({} chars, max 4000)", d.len())); }
                }
                match self
                .store
                .task_update(id, title.as_deref(), description.as_deref(), priority)
            {
                Ok(task) => Response::Tasks {
                    tasks: vec![task],
                },
                Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskGet { id, .. } => match self.store.task_get(id) {
                Ok(Some(task)) => Response::Tasks {
                    tasks: vec![task],
                },
                Ok(None) => Response::err(format!("task #{id} not found")),
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskClone { id, .. } => match self.store.task_clone(id, identity) {
                Ok(t) => {
                    self.log(format!("{identity} cloned task #{id} → #{}", t.id));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("cloned task #{id} → new task #{}", t.id))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskDelete { id, .. } => match self.store.task_delete(id) {
                Ok(()) => Response::ok_msg(format!("task #{id} deleted")),
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskPrune { before_secs, .. } => match self.store.task_prune(before_secs) {
                Ok(count) => Response::Pruned { count },
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskComment { id, body, .. } => {
                match self.store.task_comment(id, identity, &body) {
                    Ok(_) => {
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        Response::ok_msg(format!("comment added to task #{id}"))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskComments { id, .. } => match self.store.task_comments(id) {
                Ok(comments) => Response::Comments { comments },
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskPin { id, .. } => match self.store.task_pin(id) {
                Ok(()) => {
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("task #{id} pinned"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskUnpin { id, .. } => match self.store.task_unpin(id) {
                Ok(()) => {
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("task #{id} unpinned"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskTag { id, label, .. } => match self.store.task_tag(id, &label) {
                Ok(()) => {
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("tagged task #{id} with {label:?}"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskUntag { id, label, .. } => match self.store.task_untag(id, &label) {
                Ok(()) => {
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("removed tag {label:?} from task #{id}"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskSetDue { id, due_at, .. } => {
                match self.store.task_set_due(id, due_at) {
                    Ok(()) => {
                        self.audit.write(
                            "task_set_due",
                            identity,
                            serde_json::json!({"task_id": id, "due_at": due_at}),
                        );
                        let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                        Response::ok_msg(format!("due date updated for task #{id}"))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::TaskArchive { id, .. } => match self.store.task_archive(id) {
                Ok(()) => {
                    self.audit.write("task_archive", identity, serde_json::json!({"task_id": id}));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    Response::ok_msg(format!("task #{id} archived"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskRestore { id, .. } => match self.store.task_restore(id) {
                Ok(()) => {
                    self.audit.write("task_restore", identity, serde_json::json!({"task_id": id}));
                    let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
                    self.wake_idle();
                    Response::ok_msg(format!("task #{id} restored"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            Request::TaskListArchived { .. } => match self.store.task_list_archived() {
                Ok(tasks) => Response::Tasks { tasks },
                Err(e) => Response::err(e.to_string()),
            },
            Request::Status => self.status_response(),
            Request::AgentAdd { config, .. } => self.add_agent_live(identity, config),
            Request::FilesClaim { paths, .. } => {
                if let Err(e) = crate::store::Store::validate_claim_paths(&paths) {
                    return Response::err(e.to_string());
                }
                match self.store.files_claim(identity, &paths) {
                Ok(()) => {
                    self.audit.write("file_claim", identity, serde_json::json!({"paths": paths}));
                    self.fire_webhook(
                        webhook::Payload::new(webhook::Event::FileClaim)
                            .with_agent(identity)
                            .with_paths(paths.clone()),
                    );
                    Response::ok_msg(format!("claimed {} file(s)", paths.len()))
                }
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
                }
            },
            Request::FilesRelease { paths, all, .. } => {
                let claimed_paths = self
                    .store
                    .files_list_for_agent(identity)
                    .unwrap_or_default();
                match self.store.files_release(identity, &paths, all) {
                    Ok(n) => {
                        // Agent-level auto_commit overrides the global default.
                        let effective_auto_commit = self
                            .cfg
                            .agents
                            .iter()
                            .find(|a| a.name == identity)
                            .and_then(|a| a.auto_commit)
                            .unwrap_or(self.cfg.auto_commit);
                        if effective_auto_commit && !claimed_paths.is_empty() {
                            self.auto_commit_changes(identity, &claimed_paths);
                        }
                        self.audit.write("file_release", identity, serde_json::json!({"count": n}));
                        let released_paths: Vec<String> =
                            claimed_paths.iter().map(|c| c.path.clone()).collect();
                        self.fire_webhook(
                            webhook::Payload::new(webhook::Event::FileRelease)
                                .with_agent(identity)
                                .with_paths(released_paths),
                        );
                        self.log(format!("{identity}: released {n} file claim(s)"));
                        Response::ok_msg(format!("released {n} file claim(s)"))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::FilesList => match self.store.files_list() {
                Ok(claims) => Response::Files { claims },
                Err(e) => Response::err(e.to_string()),
            },
            Request::Stop { agent, .. } => match agent {
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
            Request::Pause { agent, .. } => {
                if agent == "all" {
                    let names: Vec<String> = self.agents.keys().cloned().collect();
                    for name in &names {
                        self.pause_agent(name);
                    }
                    self.fire_webhook(webhook::Payload::new(webhook::Event::FleetPaused));
                    Response::ok_msg(format!("paused {} agent(s)", names.len()))
                } else {
                    self.pause_agent(&agent)
                }
            }
            Request::Resume { agent, .. } => {
                if agent == "all" {
                    let names: Vec<String> = self.agents.keys().cloned().collect();
                    for name in &names {
                        self.resume_agent(name);
                    }
                    self.fire_webhook(webhook::Payload::new(webhook::Event::FleetResumed));
                    Response::ok_msg(format!("resumed {} agent(s)", names.len()))
                } else {
                    self.resume_agent(&agent)
                }
            }
            Request::AgentSwapModel { agent, model, .. } => {
                if let Some(rt) = self.agents.get_mut(&agent) {
                    let old_model = rt.cfg.model.clone().unwrap_or_else(|| "default".to_string());
                    rt.cfg.model = Some(model.clone());
                    let msg = format!(
                        "Your model has been swapped from '{old_model}' to '{model}'. \
                        The new model takes effect on your next restart."
                    );
                    let _ = self.store.msg_send("hub", &[agent.clone()], &msg, false);
                    self.log(format!("{agent}: model swapped {old_model} → {model}"));
                    Response::ok_msg(format!("{agent}: model will be '{model}' on next restart"))
                } else {
                    Response::err(format!("unknown agent {agent:?}"))
                }
            }
            Request::AgentSetLogLevel { agent, level, .. } => {
                let valid = matches!(level.as_str(), "debug" | "info" | "warn" | "error");
                if !valid {
                    return Response::err(format!("invalid log level '{level}' — use: debug, info, warn, error"));
                }
                if let Some(rt) = self.agents.get_mut(&agent) {
                    rt.log_level = Some(level.clone());
                    let note = format!("Log verbosity set to '{level}'. This takes effect on your next turn.");
                    let _ = self.store.msg_send("hub", &[agent.clone()], &note, false);
                    self.log(format!("{agent}: log level set to {level}"));
                    Response::ok_msg(format!("{agent}: log level is now '{level}'"))
                } else {
                    Response::err(format!("unknown agent {agent:?}"))
                }
            }
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
            .map(|rt| {
                // Augment state_detail for paused agents with pause duration.
                let detail = if rt.state == crate::agent::AgentState::Paused {
                    if let Some(paused_at) = rt.paused_at {
                        let mins = paused_at.elapsed().as_secs() / 60;
                        if mins > 0 {
                            Some(format!("paused {}m", mins))
                        } else {
                            Some("paused <1m".to_string())
                        }
                    } else {
                        rt.state_detail.clone()
                    }
                } else {
                    rt.state_detail.clone()
                };
                AgentStatusRow {
                    name: rt.cfg.name.clone(),
                    provider: self.cfg.agent_provider(&rt.cfg).to_string(),
                    state: rt.state.as_str().to_string(),
                    detail,
                    session_id: rt.session_id.clone(),
                    spent_usd: rt.spent_usd,
                    turns: rt.turns,
                }
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
        // Release claimed tasks and file claims so other agents can pick them up.
        let released = self.store.release_claims(name).unwrap_or(0);
        if released > 0 {
            self.log(format!(
                "{name}: auto-handoff — {released} claimed task(s) reopened for other agents"
            ));
            let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
        }
        let freed = self.store.files_release(name, &[], true).unwrap_or(0);
        if freed > 0 {
            self.log(format!("{name}: released {freed} file claim(s) on stop"));
        }
        if released > 0 || freed > 0 {
            self.wake_idle();
        }
        self.audit.write("agent_stop", name, serde_json::json!({}));
        Response::ok_msg(format!("stopping {name}"))
    }

    fn pause_agent(&mut self, name: &str) -> Response {
        let Some(rt) = self.agents.get_mut(name) else {
            return Response::err(format!("unknown agent {name:?}"));
        };
        match rt.state {
            AgentState::Idle => {
                rt.state = AgentState::Paused;
                rt.paused_at = Some(std::time::Instant::now());
                self.emit_state(name);
                self.audit.write("agent_pause", name, serde_json::json!({}));
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
            rt.paused_at = None;
            self.emit_state(name);
            self.audit.write("agent_resume", name, serde_json::json!({}));
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

    /// Warn when all active agents appear stuck waiting for file claims held by
    /// each other (a file-claim deadlock). Fires when every non-Stopped/Crashed
    /// agent has been Working for >10 minutes and every one holds at least one
    /// file claim — suggesting no one can make progress.
    fn check_deadlock(&mut self) {
        const DEADLOCK_SECS: u64 = 10 * 60;
        let active: Vec<&crate::agent::AgentRuntime> = self
            .agents
            .values()
            .filter(|rt| {
                matches!(rt.state, AgentState::Working | AgentState::Idle)
                    && !matches!(
                        rt.state,
                        AgentState::Stopped | AgentState::Crashed | AgentState::Paused
                    )
            })
            .collect();
        if active.is_empty() {
            return;
        }
        // All active agents must be Working AND stalled for >DEADLOCK_SECS.
        let all_stalled = active.iter().all(|rt| {
            rt.state == AgentState::Working
                && rt
                    .working_since
                    .map(|t| t.elapsed().as_secs() >= DEADLOCK_SECS)
                    .unwrap_or(false)
        });
        if !all_stalled {
            return;
        }
        // All stalled agents must hold at least one file claim.
        let claims = match self.store.files_list() {
            Ok(c) => c,
            Err(_) => return,
        };
        let agents_with_claims: std::collections::HashSet<&str> =
            claims.iter().map(|c| c.agent.as_str()).collect();
        let all_hold_claims = active
            .iter()
            .all(|rt| agents_with_claims.contains(rt.cfg.name.as_str()));
        if all_hold_claims {
            self.log(
                "DEADLOCK WARNING: all active agents are stalled Working with outstanding file \
                claims — possible circular file-claim dependency. Check 'agentcom files list'."
                    .to_string(),
            );
            let _ = self.store.msg_send(
                "hub",
                &[crate::config::COMPOSER_NAME.to_string()],
                "DEADLOCK DETECTED: all agents have been Working >10 minutes and each holds \
                file claims. Likely circular dependency. Run 'agentcom files list' to diagnose. \
                Consider interrupting the blocked agents to break the cycle.",
                true,
            );
        }
    }

    fn check_priority_escalation(&mut self) {
        let Some(hours) = self.cfg.priority_escalate_after_hours else {
            return;
        };
        let threshold_secs = hours * 3600;
        match self.store.escalate_stale_tasks(threshold_secs) {
            Ok(ids) if !ids.is_empty() => {
                for id in &ids {
                    self.log(format!(
                        "priority escalation: task #{id} priority bumped (open >{hours}h)"
                    ));
                }
                let _ = self.ui_tx.send(UiEvent::TaskBoardChanged);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("priority escalation failed: {e}"),
        }
    }

    /// Re-read agentcom.toml when its mtime changes. Spawns new agents; updates
    /// existing agent configs in memory. Does NOT stop removed agents (logs warning).
    fn check_config_reload(&mut self) {
        if !self.cfg.config_watch {
            return;
        }
        let config_path = self.project_root.join("agentcom.toml");
        let mtime = match std::fs::metadata(&config_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return,
        };
        // First call: record mtime and return without reloading.
        if self.config_mtime.is_none() {
            self.config_mtime = Some(mtime);
            return;
        }
        if self.config_mtime == Some(mtime) {
            return;
        }
        self.config_mtime = Some(mtime);
        self.apply_config_diff(&config_path);
    }

    fn apply_config_diff(&mut self, config_path: &std::path::Path) {
        let project_root = self.project_root.clone();
        let new_cfg = match crate::config::HubConfig::load(config_path.parent().unwrap_or(&project_root)) {
            Ok(c) => c,
            Err(e) => {
                self.log(format!("config hot-reload: parse error (keeping old config): {e}"));
                return;
            }
        };
        self.log("config hot-reload: agentcom.toml changed — applying diff".to_string());

        // Detect removed agents (warn only — do not stop).
        let new_names: std::collections::HashSet<&str> =
            new_cfg.agents.iter().map(|a| a.name.as_str()).collect();
        for old in &self.cfg.agents {
            if !new_names.contains(old.name.as_str()) {
                self.log(format!(
                    "config hot-reload: agent '{}' removed from config — still running (stop it manually)",
                    old.name
                ));
            }
        }

        // Update in-memory config for existing agents; spawn new ones.
        for new_agent in &new_cfg.agents {
            if let Some(rt) = self.agents.get_mut(&new_agent.name) {
                rt.cfg = new_agent.clone();
                self.log(format!("config hot-reload: updated config for '{}'", new_agent.name));
            } else {
                // New agent — add to running fleet.
                self.log(format!("config hot-reload: spawning new agent '{}'", new_agent.name));
                let buf = Arc::new(RwLock::new(crate::tui::ringbuf::RingBuf::new()));
                self.buffers.write().unwrap().insert(new_agent.name.clone(), buf.clone());
                self.agents.insert(
                    new_agent.name.clone(),
                    crate::agent::AgentRuntime::new(new_agent.clone(), buf),
                );
                let name = new_agent.name.clone();
                let _ = self.spawn_agent(&name, None);
            }
        }

        // Preserve agents list order from new config; append any agents not in new config.
        let mut merged = new_cfg.agents.clone();
        for old in &self.cfg.agents {
            if !new_names.contains(old.name.as_str()) {
                merged.push(old.clone());
            }
        }
        self.cfg = crate::config::HubConfig { agents: merged, ..new_cfg };
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
        // Also pick up new (untracked) files within the claimed paths.
        let untracked = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard", "--"])
            .args(&paths)
            .current_dir(&self.project_root)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let all_modified: Vec<&str> = modified
            .lines()
            .chain(unstaged.lines())
            .chain(untracked.lines())
            .filter(|l| !l.is_empty())
            .collect();
        if all_modified.is_empty() {
            return;
        }

        // Deduplicate while preserving order
        let mut seen = std::collections::HashSet::new();
        let deduped: Vec<&&str> = all_modified
            .iter()
            .filter(|p| seen.insert(**p))
            .collect();

        // Filter out paths matching any commit_exclude_pattern glob.
        let unique: Vec<&&str> = deduped
            .into_iter()
            .filter(|p| {
                !self.cfg.commit_exclude_patterns.iter().any(|pat| {
                    glob::Pattern::new(pat)
                        .map(|g| g.matches(p))
                        .unwrap_or(false)
                })
            })
            .collect();

        // Optional test gate: run configured command; skip commit if it fails.
        if let Some(test_cmd) = self.cfg.auto_commit_test_cmd.as_deref() {
            let result = Command::new("sh")
                .args(["-c", test_cmd])
                .current_dir(&self.project_root)
                .output();
            match result {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let code = o.status.code().unwrap_or(-1);
                    self.log(format!(
                        "auto-commit skipped: tests failed (exit {code}) for {agent} — cmd: {test_cmd}"
                    ));
                    return;
                }
                Err(e) => {
                    self.log(format!(
                        "auto-commit skipped: could not run test command for {agent}: {e}"
                    ));
                    return;
                }
            }
        }

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
        let task_info = self.store.claimed_task(agent).ok().flatten();
        let task_prefix = task_info
            .as_ref()
            .map(|t| {
                let title: String = t.title.chars().take(60).collect();
                format!("task #{} {} — ", t.id, title)
            })
            .unwrap_or_default();
        let summary = format!("{agent}: {task_prefix}{files_str}");

        // Body: list every changed path, and optionally the first line of the task description.
        let mut body_lines: Vec<String> = unique.iter().map(|p| format!("  {p}")).collect();
        if let Some(t) = &task_info {
            if !t.description.is_empty() {
                body_lines.push(String::new());
                body_lines.push(format!(
                    "Task: {}",
                    t.description.lines().next().unwrap_or("")
                ));
            }
        }
        let body = body_lines.join("\n");

        let mut commit_cmd = Command::new("git");
        commit_cmd
            .args(["commit", "--author", &author, "-m", &summary, "-m", &body])
            .current_dir(&self.project_root);
        if self.cfg.auto_commit_skip_hooks {
            commit_cmd.arg("--no-verify");
        }
        // Scope the commit to ONLY the agent's files — prevents sweeping up other
        // agents' staged changes that happen to be in the index.
        commit_cmd.arg("--").args(unique.iter().map(|p| **p));

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
                if self.cfg.auto_push {
                    let remote = self.cfg.auto_push_remote.as_deref().unwrap_or("origin");
                    match Command::new("git")
                        .args(["push", remote])
                        .current_dir(&self.project_root)
                        .output()
                    {
                        Ok(po) if po.status.success() => {
                            self.log(format!(
                                "auto-push: pushed [{hash}] to {remote}"
                            ));
                        }
                        Ok(po) => {
                            let stderr = String::from_utf8_lossy(&po.stderr);
                            self.log(format!(
                                "auto-push: push to {remote} failed for {agent}: {stderr}"
                            ));
                        }
                        Err(e) => {
                            self.log(format!(
                                "auto-push: failed to run git push for {agent}: {e}"
                            ));
                        }
                    }
                }
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
