pub mod io_tasks;
pub mod spawn;

use crate::config::AgentConfig;
use crate::tui::ringbuf::SharedRingBuf;
use std::time::Instant;
use tokio::sync::mpsc;

/// Commands accepted by the per-agent stdin-writer task. A single writer
/// task owns `ChildStdin`, so JSON lines can never interleave.
#[derive(Debug)]
pub enum WriterCmd {
    /// A complete NDJSON line (without trailing newline).
    Line(String),
    /// Close stdin (graceful-shutdown signal for the child).
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    /// Nothing to feed; the hub wakes it on new work. Note the child emits
    /// its `system/init` only after the first user message arrives, so a
    /// freshly spawned agent is fed eagerly — never gated on init.
    Idle,
    /// A prompt has been written; awaiting the `result` event.
    Working,
    /// Interrupt control request sent; awaiting the aborted turn's `result`.
    Interrupting,
    /// Human-paused: results are processed but nothing new is fed.
    Paused,
    Crashed,
    Stopped,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Interrupting => "interrupting",
            AgentState::Paused => "paused",
            AgentState::Crashed => "crashed",
            AgentState::Stopped => "stopped",
        }
    }
}

/// Hub-owned runtime record for one agent.
pub struct AgentRuntime {
    pub cfg: AgentConfig,
    pub state: AgentState,
    pub state_detail: Option<String>,
    pub session_id: Option<String>,
    pub stdin_tx: Option<mpsc::Sender<WriterCmd>>,
    pub child_pid: Option<u32>,
    pub spent_usd: f64,
    pub turns: u64,
    pub out_buf: SharedRingBuf,
    /// Set while `Interrupting`: when to give up and escalate to a kill.
    pub interrupt_deadline: Option<Instant>,
    /// Pending urgent message ids to deliver as soon as the turn aborts.
    pub pending_urgent: bool,
    /// While Paused was requested mid-turn, defer until the result lands.
    pub pause_requested: bool,
    /// Restart bookkeeping for the crash-loop cap.
    pub restarts_this_hour: u32,
    pub restart_window_start: Option<Instant>,
    /// Set once a budget-warning notification has been emitted this session.
    pub budget_warn_fired: bool,
    /// Timestamp when the agent last transitioned to Working. Used for stall detection.
    pub working_since: Option<Instant>,
    /// True once a stall warning has been logged for the current turn (reset on idle).
    pub stall_warned: bool,
    /// Timestamp when the agent entered the Paused state. Cleared on resume.
    pub paused_at: Option<Instant>,
    /// Number of consecutive crashes in the current window (reset after window expires).
    pub crash_count: u32,
    /// Timestamp of the first crash in the current window.
    pub first_crash_at: Option<Instant>,
    /// Runtime log verbosity override set via 'agentcom agent log-level'.
    pub log_level: Option<String>,
    /// Set in handle_result to signal a deliberate session reset (not a crash).
    /// handle_exit checks this flag to respawn fresh instead of treating it as a crash.
    pub planned_restart: bool,
    /// Include glob set compiled from non-negated AgentConfig.lanes patterns.
    /// None when lanes has no include patterns (no enforcement).
    pub lane_set: Option<globset::GlobSet>,
    /// Exclude glob set compiled from `!`-prefixed AgentConfig.lanes patterns.
    /// A path matching lane_set but also matching lane_exclude_set is rejected.
    pub lane_exclude_set: Option<globset::GlobSet>,
}

fn build_glob_set<'a>(agent_name: &str, patterns: impl Iterator<Item = &'a str>) -> Option<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    let mut added = 0usize;
    for pat in patterns {
        match globset::Glob::new(pat) {
            Ok(g) => { builder.add(g); added += 1; }
            Err(e) => {
                tracing::warn!(
                    agent = %agent_name,
                    pattern = %pat,
                    "invalid lane glob pattern: {e}; skipping"
                );
            }
        }
    }
    if added == 0 {
        return None;
    }
    match builder.build() {
        Ok(gs) => Some(gs),
        Err(e) => {
            tracing::warn!(agent = %agent_name, "failed to build lane glob set: {e}; ignoring");
            None
        }
    }
}

impl AgentRuntime {
    pub fn new(cfg: AgentConfig, out_buf: SharedRingBuf) -> Self {
        // Split lanes into include (plain) and exclude (`!`-prefixed) sets.
        let lane_set = build_glob_set(
            &cfg.name,
            cfg.lanes.iter().filter(|p| !p.starts_with('!')).map(String::as_str),
        );
        let lane_exclude_set = build_glob_set(
            &cfg.name,
            cfg.lanes.iter().filter(|p| p.starts_with('!')).map(|p| &p[1..]),
        );
        Self {
            cfg,
            state: AgentState::Stopped,
            state_detail: None,
            session_id: None,
            stdin_tx: None,
            child_pid: None,
            spent_usd: 0.0,
            turns: 0,
            out_buf,
            interrupt_deadline: None,
            pending_urgent: false,
            pause_requested: false,
            restarts_this_hour: 0,
            restart_window_start: None,
            budget_warn_fired: false,
            working_since: None,
            stall_warned: false,
            paused_at: None,
            crash_count: 0,
            first_crash_at: None,
            log_level: None,
            planned_restart: false,
            lane_set,
            lane_exclude_set,
        }
    }

    pub fn is_running(&self) -> bool {
        !matches!(self.state, AgentState::Crashed | AgentState::Stopped)
    }

    /// Send a line to the child's stdin without blocking the hub loop.
    pub fn write_line(&self, line: String) {
        if let Some(tx) = &self.stdin_tx {
            let tx = tx.clone();
            let name = self.cfg.name.clone();
            tokio::spawn(async move {
                if tx.send(WriterCmd::Line(line)).await.is_err() {
                    tracing::warn!(agent = %name, "stdin writer gone; line dropped");
                }
            });
        }
    }
}
