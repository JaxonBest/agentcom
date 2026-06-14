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
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub depends_on: Vec<i64>,
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
