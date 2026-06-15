//! Channel message types flowing through the hub.

use crate::ipc::{Request, Response};
use crate::protocol::event::CliEvent;
use tokio::sync::oneshot;

/// Events arriving at the central hub loop.
#[derive(Debug)]
pub enum HubEvent {
    /// A parsed (or raw) line from a child's stdout.
    Cli { agent: String, event: CliEvent },
    /// A raw line that failed to parse (logged, shown in output pane).
    CliRaw { agent: String, line: String },
    /// A line from a child's stderr.
    Stderr { agent: String, line: String },
    /// Child process exited.
    Exited { agent: String, code: Option<i32> },
    /// Result of a post-close hook run.
    HookResult {
        task_id: i64,
        task_title: String,
        agent: String,
        success: bool,
        /// Captured stdout+stderr, truncated to 4KB.
        output: String,
    },
}

/// A request forwarded from an IPC connection (or the TUI) to the hub loop.
#[derive(Debug)]
pub struct IpcMsg {
    pub identity: String,
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}

/// Broadcast to UI consumers (TUI, `tail --follow` connections).
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A display line appended to an agent's output buffer.
    AgentLine {
        agent: String,
        line: String,
    },
    /// Streaming text delta appended to some agent's open tail line
    /// (redraw signal; the buffer itself holds the content).
    AgentDelta,
    StateChange {
        agent: String,
        state: String,
        detail: Option<String>,
    },
    TaskBoardChanged,
    MessagesChanged,
    HubLog(String),
    Shutdown,
}
