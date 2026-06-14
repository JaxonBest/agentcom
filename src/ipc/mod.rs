//! Wire protocol between the hub and `agentcom` client invocations
//! (agents calling `agentcom send ...` via their Bash tool, or humans in
//! other terminals).
//!
//! Transport: NDJSON over TCP on 127.0.0.1. The first frame on every
//! connection must be `Hello { token, identity }`; the token comes from
//! `AGENTCOM_TOKEN` (injected into agent child processes) or `hub.json`.

pub mod client;
pub mod server;

use crate::store::{Message, Task};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Written to the data dir by a running hub; clients discover it from here
/// when env vars aren't present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub project_root: PathBuf,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hello {
        token: String,
        /// Agent name from `AGENTCOM_AGENT`, or "human" for terminals.
        identity: String,
    },
    Send {
        to: String,
        body: String,
        urgent: bool,
    },
    Inbox,
    TaskAdd {
        title: String,
        description: String,
        priority: i64,
        depends_on: Vec<i64>,
    },
    TaskList {
        status: Option<String>,
        /// Filter tasks by keyword (matches title or description, case-insensitive)
        search: Option<String>,
        /// Filter tasks that have this label/tag (exact match).
        #[serde(default)]
        tag: Option<String>,
    },
    TaskClaim {
        id: i64,
    },
    TaskDone {
        id: i64,
        note: Option<String>,
    },
    TaskBlock {
        id: i64,
        reason: String,
    },
    TaskReopen {
        id: i64,
    },
    TaskEdit {
        id: i64,
        title: Option<String>,
        description: Option<String>,
        priority: Option<i64>,
    },
    TaskGet {
        id: i64,
    },
    /// Permanently delete a task from the board.
    TaskDelete {
        id: i64,
    },
    /// Manually route a task to a specific agent (bypasses dep check; works
    /// on open/blocked tasks; hub also sends the agent an inbox message).
    TaskAssign {
        id: i64,
        agent: String,
    },
    /// Prune old done/blocked tasks.
    TaskPrune {
        /// Delete tasks whose updated_at is more than this many seconds ago.
        before_secs: i64,
    },
    /// Clone a task: copy title, description, and priority into a new open task.
    TaskClone {
        id: i64,
    },
    /// Append a timestamped comment to a task's activity log.
    TaskComment {
        id: i64,
        body: String,
    },
    /// Retrieve all comments on a task (newest last).
    TaskComments {
        id: i64,
    },
    /// Pin a task so it sorts before all non-pinned tasks.
    TaskPin {
        id: i64,
    },
    /// Unpin a task.
    TaskUnpin {
        id: i64,
    },
    /// Add a label to a task.
    TaskTag {
        id: i64,
        label: String,
    },
    /// Remove a label from a task.
    TaskUntag {
        id: i64,
        label: String,
    },
    Status,
    /// Hot-add an agent to the running hub (already persisted to
    /// agentcom.toml by the client).
    AgentAdd {
        config: crate::config::AgentConfig,
    },
    FilesClaim {
        paths: Vec<String>,
    },
    FilesRelease {
        paths: Vec<String>,
        all: bool,
    },
    FilesList,
    Tail {
        agent: String,
        lines: usize,
        follow: bool,
    },
    Stop {
        agent: Option<String>,
    },
    Pause {
        agent: String,
    },
    Resume {
        agent: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok {
        #[serde(default)]
        message: Option<String>,
    },
    Err {
        message: String,
    },
    Inbox {
        messages: Vec<Message>,
    },
    Tasks {
        tasks: Vec<Task>,
    },
    Status {
        project: String,
        agents: Vec<AgentStatusRow>,
        open_tasks: u64,
        pending_msgs: u64,
        total_cost_usd: f64,
        /// Free-mode summary line (goal + remaining limits), if active.
        #[serde(default)]
        free: Option<String>,
    },
    Files {
        claims: Vec<crate::store::files::FileClaim>,
    },
    /// Result of a prune operation.
    Pruned {
        count: usize,
    },
    /// Streamed repeatedly in `Tail { follow: true }` mode.
    TailLine {
        line: String,
    },
    /// Result of a TaskClone operation.
    Cloned {
        new_id: i64,
    },
    /// Comments on a task (from TaskComments request).
    Comments {
        comments: Vec<crate::store::TaskComment>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusRow {
    pub name: String,
    pub provider: String,
    pub state: String,
    pub detail: Option<String>,
    pub session_id: Option<String>,
    pub spent_usd: f64,
    pub turns: u64,
}

impl Response {
    pub fn err(msg: impl Into<String>) -> Self {
        Response::Err {
            message: msg.into(),
        }
    }
    pub fn ok() -> Self {
        Response::Ok { message: None }
    }
    pub fn ok_msg(msg: impl Into<String>) -> Self {
        Response::Ok {
            message: Some(msg.into()),
        }
    }
}
