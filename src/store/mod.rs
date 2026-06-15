//! Persistent state: tasks, messages, run history.
//!
//! rusqlite is synchronous; every operation here is a sub-millisecond
//! point query against a local WAL-mode database, so methods are called
//! directly from the hub loop behind a `Mutex` rather than through
//! `spawn_blocking` plumbing.

pub mod activity;
pub mod files;
pub mod messages;
pub mod schema;
pub mod tasks;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    pub(crate) conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // Pre-create the file with 0600 permissions on Unix so the database
        // is never momentarily world-readable before rusqlite sets it up.
        #[cfg(unix)]
        if !path.exists() {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .mode(0o600)
                .open(path)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn record_run_start(&self, agent: &str, session_id: &str, now: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO runs (agent, session_id, started_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![agent, session_id, now],
        )?;
        Ok(())
    }

    pub fn record_run_end(
        &self,
        agent: &str,
        session_id: &str,
        cost_usd: f64,
        turns: u64,
        end_reason: &str,
        now: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET ended_at = ?1, cost_usd = ?2, turns = ?3, end_reason = ?4
             WHERE agent = ?5 AND session_id = ?6",
            rusqlite::params![now, cost_usd, turns, end_reason, agent, session_id],
        )?;
        Ok(())
    }

    pub fn clean_session(&self, keep_runs: bool) -> Result<CleanStats> {
        let conn = self.conn.lock().unwrap();
        // Delete child tables before tasks to avoid FK constraint violations.
        conn.execute("DELETE FROM task_activity", [])?;
        conn.execute("DELETE FROM task_deps", [])?;
        let tasks = conn.execute("DELETE FROM tasks", [])?;
        let messages = conn.execute("DELETE FROM messages", [])?;
        let file_claims = conn.execute("DELETE FROM file_claims", [])?;
        let runs = if !keep_runs {
            conn.execute("DELETE FROM runs", [])?
        } else {
            0
        };
        Ok(CleanStats { tasks, messages, file_claims, runs })
    }
}

pub struct CleanStats {
    pub tasks: usize,
    pub messages: usize,
    pub file_claims: usize,
    pub runs: usize,
}

pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Claimed,
    Done,
    Blocked,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(TaskStatus::Open),
            "claimed" => Some(TaskStatus::Claimed),
            "done" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    /// 0 is the highest priority.
    pub priority: i64,
    pub claimed_by: Option<String>,
    pub blocked_reason: Option<String>,
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Unix timestamp after which this task is considered overdue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<i64>,
    /// Auto-block this task if it stays claimed for more than this many minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_mins: Option<u64>,
    /// Capability labels the claiming agent must have (all required).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Soft-deleted: hidden from normal task list until restored.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_archived: bool,
    /// Recurrence interval ("1h", "1d", "7d", "1w"). Hub creates a fresh copy when done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recur: Option<String>,
    /// Unix timestamp when the next recurrence should become visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// Times the post-close hook has run for this task. Stops at 2 to prevent looping.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub hook_attempts: u32,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub depends_on: Vec<i64>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}



/// Portable snapshot of a task used for export/import.
/// `depends_on` holds the original source task IDs; the importer remaps them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub title: String,
    pub description: String,
    pub priority: i64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Source-DB task IDs this task depends on. Remapped to new IDs on import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<i64>,
    /// Original source DB id (informational; not used on import).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_mins: Option<u64>,
    /// Capability labels required on the claiming agent (all must be present).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

impl Default for Task {
    fn default() -> Self {
        Self {
            id: 0,
            title: String::new(),
            description: String::new(),
            status: TaskStatus::Open,
            priority: 2,
            claimed_by: None,
            blocked_reason: None,
            note: None,
            tags: vec![],
            pinned: false,
            due_at: None,
            timeout_mins: None,
            requires: vec![],
            is_archived: false,
            recur: None,
            next_run_at: None,
            hook_attempts: 0,
            created_by: String::new(),
            created_at: 0,
            updated_at: 0,
            depends_on: vec![],
        }
    }
}

impl Default for TaskSnapshot {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            priority: 2,
            status: "open".to_string(),
            tags: vec![],
            depends_on: vec![],
            source_id: None,
            due_at: None,
            timeout_mins: None,
            requires: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComment {
    pub id: i64,
    pub task_id: i64,
    pub agent: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub from_who: String,
    pub to_who: String,
    pub body: String,
    pub urgent: bool,
    pub delivered: bool,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
}

#[cfg(test)]
mod clean_tests {
    use super::*;

    #[test]
    fn clean_wipes_tasks_messages_file_claims() {
        let s = Store::open_in_memory().unwrap();
        s.task_add("t", "", 2, &[], "human").unwrap();
        s.msg_send("alice", &["bob".into()], "hi", false).unwrap();
        s.files_claim("alice", &["src/a.rs".into()]).unwrap();

        let stats = s.clean_session(false).unwrap();
        assert!(stats.tasks >= 1, "expected at least 1 task deleted");
        assert!(stats.messages >= 1, "expected at least 1 message deleted");

        let remaining_tasks = s.task_list(None, None).unwrap();
        assert!(remaining_tasks.is_empty(), "task list should be empty after clean");

        let remaining_msgs = s.msg_list(None, None, 1000).unwrap();
        assert!(remaining_msgs.is_empty(), "messages should be empty after clean");

        let remaining_claims = s.files_list().unwrap();
        assert!(remaining_claims.is_empty(), "file claims should be empty after clean");
    }

    #[test]
    fn clean_keep_runs_preserves_runs() {
        let s = Store::open_in_memory().unwrap();
        s.task_add("t", "", 2, &[], "human").unwrap();
        s.msg_send("alice", &["bob".into()], "hi", false).unwrap();
        s.record_run_start("builder", "sess1", 1_000_000).unwrap();

        let stats = s.clean_session(true).unwrap();
        assert!(stats.tasks >= 1, "expected at least 1 task deleted");
        assert!(stats.messages >= 1, "expected at least 1 message deleted");
        assert_eq!(stats.runs, 0, "keep_runs=true should not delete runs");

        let conn = s.conn.lock().unwrap();
        let run_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(run_count, 1, "run row should be preserved when keep_runs=true");
    }

    #[test]
    fn clean_returns_zero_stats_when_empty() {
        let s = Store::open_in_memory().unwrap();
        let stats = s.clean_session(false).unwrap();
        assert_eq!(stats.tasks, 0);
        assert_eq!(stats.messages, 0);
        assert_eq!(stats.file_claims, 0);
        assert_eq!(stats.runs, 0);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn hub_db_created_with_0600_permissions() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("hub.db");
        Store::open(&db_path).unwrap();
        let mode = std::fs::metadata(&db_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "hub.db must be owner-read/write only, got {mode:o}");
    }
}
