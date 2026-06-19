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

/// True for a human session identity: the legacy literal `human` or the
/// per-session form `human:<id>`. Human peers use the non-destructive
/// per-session inbox cursor; everything else uses the destructive agent inbox.
pub fn is_human_identity(identity: &str) -> bool {
    identity == "human" || identity.starts_with("human:")
}

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
        /// Agent name from `AGENTCOM_AGENT`, or a human session id
        /// (`human:<uuid>`) for terminals/TUIs.
        identity: String,
        /// Session kind for human peers: "human" | "tui" | "cli" | "rest".
        /// Absent for legacy clients and agents.
        #[serde(default)]
        kind: Option<String>,
        /// Optional human-friendly session label.
        #[serde(default)]
        label: Option<String>,
    },
    Send {
        to: String,
        body: String,
        urgent: bool,
    },
    Inbox,
    /// List currently-connected sessions (multi-session awareness).
    Sessions,
    /// Server-internal ONLY: the IPC server emits this to the hub when a
    /// connection closes so the hub can stamp the session disconnected. It is
    /// never sent by real clients (and harmless if one does — a session can
    /// only disconnect itself).
    SessionBye,
    TaskAdd {
        title: String,
        description: String,
        priority: i64,
        depends_on: Vec<i64>,
        /// Auto-block the task if it stays claimed for more than this many minutes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_mins: Option<u64>,
        /// Capability labels the claiming agent must have (all required).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires: Vec<String>,
        /// Recurrence interval ("1d", "7d", "1h", "1w"). Hub creates a fresh copy each time done.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recur: Option<String>,
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
    /// Approve or reject a task sitting in AwaitingReview state.
    /// Hub rejects review attempts by the agent that originally closed the task.
    TaskReview {
        id: i64,
        approve: bool,
        note: String,
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
    /// Set or clear the due date for a task (Unix timestamp; None clears it).
    TaskSetDue {
        id: i64,
        due_at: Option<i64>,
    },
    /// Soft-delete a task: hide from normal listing until restored.
    TaskArchive {
        id: i64,
    },
    /// Un-soft-delete a task: make it visible in normal listing again.
    TaskRestore {
        id: i64,
    },
    /// List all archived (soft-deleted) tasks.
    TaskListArchived,
    Status,
    /// Hot-add an agent to the running hub (already persisted to
    /// agentcom.toml by the client).
    AgentAdd {
        config: Box<crate::config::AgentConfig>,
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
    /// Update an agent's model on next restart without stopping it now.
    AgentSwapModel {
        agent: String,
        model: String,
    },
    /// Change an agent's log verbosity level without restarting it.
    AgentSetLogLevel {
        agent: String,
        /// One of: "debug", "info", "warn", "error"
        level: String,
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
    /// Reply to Hello: the (possibly server-confirmed) session identity.
    HelloOk {
        session_id: String,
    },
    /// Active sessions (reply to a Sessions request).
    Sessions {
        sessions: Vec<crate::store::sessions::SessionRow>,
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

impl Request {
    /// Construct a TaskAdd request with all optional fields defaulted.
    #[allow(dead_code)]
    pub fn task_add(
        title: impl Into<String>,
        description: impl Into<String>,
        priority: i64,
        depends_on: Vec<i64>,
    ) -> Self {
        Request::TaskAdd {
            title: title.into(),
            description: description.into(),
            priority,
            depends_on,
            timeout_mins: None,
            requires: vec![],
            recur: None,
        }
    }

    /// Construct a TaskList request with all optional filters defaulted to None.
    #[allow(dead_code)]
    pub fn task_list(status: Option<String>, search: Option<String>, tag: Option<String>) -> Self {
        Request::TaskList { status, search, tag }
    }

    /// Construct a TaskEdit request (PATCH — None fields are unchanged).
    #[allow(dead_code)]
    pub fn task_edit(
        id: i64,
        title: Option<String>,
        description: Option<String>,
        priority: Option<i64>,
    ) -> Self {
        Request::TaskEdit { id, title, description, priority }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_back_compat_without_kind_label() {
        // An old client that predates session identity sends no kind/label.
        let json = r#"{"cmd":"hello","token":"x","identity":"human"}"#;
        match serde_json::from_str::<Request>(json).unwrap() {
            Request::Hello {
                token,
                identity,
                kind,
                label,
            } => {
                assert_eq!(token, "x");
                assert_eq!(identity, "human");
                assert!(kind.is_none() && label.is_none());
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn hello_with_kind_label_roundtrips() {
        let req = Request::Hello {
            token: "t".into(),
            identity: "human:abc".into(),
            kind: Some("tui".into()),
            label: Some("term".into()),
        };
        let s = serde_json::to_string(&req).unwrap();
        match serde_json::from_str::<Request>(&s).unwrap() {
            Request::Hello { kind, label, .. } => {
                assert_eq!(kind.as_deref(), Some("tui"));
                assert_eq!(label.as_deref(), Some("term"));
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn human_identity_classification() {
        assert!(is_human_identity("human"));
        assert!(is_human_identity("human:abc-123"));
        assert!(!is_human_identity("builder"));
        assert!(!is_human_identity("rest-api"));
    }
}
