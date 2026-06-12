//! Persistent state: tasks, messages, run history.
//!
//! rusqlite is synchronous; every operation here is a sub-millisecond
//! point query against a local WAL-mode database, so methods are called
//! directly from the hub loop behind a `Mutex` rather than through
//! `spawn_blocking` plumbing.

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
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub depends_on: Vec<i64>,
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
