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
    Status,
    /// Hot-add an agent to the running hub (already persisted to
    /// agentcom.toml by the client).
    AgentAdd {
        config: crate::config::AgentConfig,
    },
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
    },
    /// Streamed repeatedly in `Tail { follow: true }` mode.
    TailLine {
        line: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusRow {
    pub name: String,
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
