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

/// Fleet-level metrics for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetStats {
    pub resolved: u64,
    pub total: u64,
    pub cost_median_usd: f64,
    pub wall_p50_secs: i64,
    pub wall_p95_secs: i64,
    /// Up to 10 chars — '✓' for done, '✗' for blocked, oldest→newest.
    pub last10: String,
}

impl FleetStats {
    pub fn resolved_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.resolved as f64 / self.total as f64 * 100.0
        }
    }
}

impl Store {
    /// Compute fleet-level dashboard statistics directly from the DB.
    pub fn stats_compute(&self) -> Result<FleetStats> {
        let conn = self.conn.lock().unwrap();

        let total: u64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE is_archived = 0",
            [],
            |r| r.get(0),
        )?;
        let resolved: u64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'done' AND is_archived = 0",
            [],
            |r| r.get(0),
        )?;

        // Median cost over done tasks that have a persisted cost.
        let mut costs: Vec<f64> = {
            let mut stmt = conn.prepare(
                "SELECT total_cost_usd FROM tasks \
                 WHERE status = 'done' AND total_cost_usd > 0 \
                 ORDER BY total_cost_usd",
            )?;
            let collected: Vec<f64> = stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            collected
        };
        costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let cost_median_usd = percentile_f64(&costs, 50);

        // Wall-time percentiles using the claimed_at column stamped on task_claim/task_assign.
        let mut wall_secs: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT updated_at - claimed_at FROM tasks \
                 WHERE status = 'done' AND claimed_at IS NOT NULL \
                 AND is_archived = 0 AND updated_at > claimed_at",
            )?;
            let collected: Vec<i64> = stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            collected
        };
        wall_secs.sort_unstable();
        let wall_p50_secs = percentile_i64(&wall_secs, 50);
        let wall_p95_secs = percentile_i64(&wall_secs, 95);

        // Last 10 closed tasks (done or blocked), oldest→newest.
        let last10_statuses: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT status FROM tasks \
                 WHERE status IN ('done', 'blocked') AND is_archived = 0 \
                 ORDER BY updated_at DESC LIMIT 10",
            )?;
            let collected: Vec<String> = stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            collected
        };
        let last10: String = last10_statuses
            .iter()
            .rev()
            .map(|s| if s == "done" { '✓' } else { '✗' })
            .collect();

        Ok(FleetStats {
            resolved,
            total,
            cost_median_usd,
            wall_p50_secs,
            wall_p95_secs,
            last10,
        })
    }
}

fn percentile_f64(sorted: &[f64], pct: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (pct * sorted.len()).saturating_sub(1) / 100;
    sorted[idx.min(sorted.len() - 1)]
}

fn percentile_i64(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (pct * sorted.len()).saturating_sub(1) / 100;
    sorted[idx.min(sorted.len() - 1)]
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
    /// Waiting for a reviewer agent to approve before transitioning to Done.
    /// Hub auto-files a paired review task when this state is entered.
    AwaitingReview,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::AwaitingReview => "awaiting_review",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(TaskStatus::Open),
            "claimed" => Some(TaskStatus::Claimed),
            "done" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            "awaiting_review" => Some(TaskStatus::AwaitingReview),
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
    /// Total cost in USD accumulated by the closing agent during this task's lifetime.
    /// Populated on task_done; 0 for old rows or tasks closed before this feature.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub total_cost_usd: f64,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub depends_on: Vec<i64>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
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
            total_cost_usd: 0.0,
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

#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn stats_compute_empty_db() {
        let s = Store::open_in_memory().unwrap();
        let stats = s.stats_compute().unwrap();
        assert_eq!(stats.resolved, 0);
        assert_eq!(stats.total, 0);
        assert_eq!(stats.cost_median_usd, 0.0);
        assert_eq!(stats.wall_p50_secs, 0);
        assert!(stats.last10.is_empty());
    }

    #[test]
    fn stats_compute_counts_resolved() {
        let s = Store::open_in_memory().unwrap();
        let a = s.task_add("task-a", "", 1, &[], "human").unwrap();
        let b = s.task_add("task-b", "", 1, &[], "human").unwrap();
        let _c = s.task_add("task-c", "", 1, &[], "human").unwrap(); // stays open
        s.task_claim(a, "builder").unwrap();
        s.task_done(a, "builder", None).unwrap();
        s.task_claim(b, "builder").unwrap();
        s.task_block(b, "builder", "reason").unwrap();

        let stats = s.stats_compute().unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.resolved, 1, "only done tasks count as resolved");
        // last10 includes done + blocked; b is blocked, a is done
        assert!(!stats.last10.is_empty());
    }

    #[test]
    fn stats_compute_persisted_cost_feeds_median() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("work", "", 1, &[], "human").unwrap();
        // Set total_cost_usd directly (simulating a prior task_done with cost)
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET status = 'done', total_cost_usd = 0.05 WHERE id = ?1",
                [id],
            ).unwrap();
        }
        let stats = s.stats_compute().unwrap();
        assert_eq!(stats.resolved, 1);
        assert!((stats.cost_median_usd - 0.05).abs() < 1e-9);
    }

    #[test]
    fn stats_compute_last10_glyphs() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..5 {
            let id = s.task_add(&format!("task-{i}"), "", 1, &[], "human").unwrap();
            s.task_claim(id, "builder").unwrap();
            s.task_done(id, "builder", None).unwrap();
        }
        let stats = s.stats_compute().unwrap();
        // All 5 are done, so all glyphs should be ✓
        assert_eq!(stats.last10.chars().count(), 5);
        assert!(stats.last10.chars().all(|c| c == '✓'));
    }

    #[test]
    fn stats_compute_wall_time_single_task() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("work", "", 1, &[], "human").unwrap();
        {
            let conn = s.conn.lock().unwrap();
            // claimed at T=1000, done at T=1300 → 300 secs wall time
            conn.execute(
                "UPDATE tasks SET status = 'done', claimed_at = 1000, updated_at = 1300 WHERE id = ?1",
                [id],
            ).unwrap();
        }

        let stats = s.stats_compute().unwrap();
        assert_eq!(stats.wall_p50_secs, 300);
        assert_eq!(stats.wall_p95_secs, 300);
    }

    #[test]
    fn stats_compute_wall_time_percentiles() {
        let s = Store::open_in_memory().unwrap();
        // 4 tasks with wall times [100, 200, 300, 900] (seconds).
        // percentile_i64 at p50 with 4 items: idx=(50*4-1)/100=1 → 200
        // percentile_i64 at p95 with 4 items: idx=(95*4-1)/100=3 → 900
        let wall_times: &[i64] = &[100, 200, 300, 900];
        let base_ts: i64 = 10_000;
        for (i, &wall) in wall_times.iter().enumerate() {
            let id = s.task_add(&format!("task-{i}"), "", 1, &[], "human").unwrap();
            let claim_ts = base_ts + (i as i64) * 10_000;
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET status = 'done', claimed_at = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![claim_ts, claim_ts + wall, id],
            ).unwrap();
        }

        let stats = s.stats_compute().unwrap();
        assert_eq!(stats.wall_p50_secs, 200, "p50 of [100,200,300,900] should be 200");
        assert_eq!(stats.wall_p95_secs, 900, "p95 of [100,200,300,900] should be 900");
    }

    #[test]
    fn stats_compute_full_fixture() {
        // Complete fixture: 3 done tasks + 1 blocked + 1 open.
        // Verifies resolved, total, cost_median_usd, wall_p50/p95, and last10 together.
        let s = Store::open_in_memory().unwrap();

        // 3 done tasks with costs [0.10, 0.20, 0.30] and wall times [60, 120, 180].
        // Use explicit updated_at stamps (1001, 1002, 1003) so last10 order is deterministic.
        let costs: &[f64] = &[0.10, 0.20, 0.30];
        let walls: &[i64] = &[60, 120, 180];
        for (i, (&cost, &wall)) in costs.iter().zip(walls).enumerate() {
            let id = s.task_add(&format!("done-{i}"), "", 1, &[], "human").unwrap();
            let conn = s.conn.lock().unwrap();
            let updated_at = 1001 + i as i64;
            let claim_at = updated_at - wall;
            conn.execute(
                "UPDATE tasks SET status = 'done', total_cost_usd = ?1, claimed_at = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![cost, claim_at, updated_at, id],
            ).unwrap();
        }

        // 1 blocked task with updated_at=1004 (most recent closed task).
        let blocked_id = s.task_add("blocked-task", "", 1, &[], "human").unwrap();
        s.task_claim(blocked_id, "builder").unwrap();
        s.task_block(blocked_id, "builder", "stuck").unwrap();
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET updated_at = 1004 WHERE id = ?1",
                [blocked_id],
            ).unwrap();
        }

        // 1 open task (counts toward total only).
        let _open_id = s.task_add("open-task", "", 1, &[], "human").unwrap();

        let stats = s.stats_compute().unwrap();

        assert_eq!(stats.total, 5);
        assert_eq!(stats.resolved, 3);

        // Median of [0.10, 0.20, 0.30]: idx=(50*3-1)/100=1 → 0.20
        assert!((stats.cost_median_usd - 0.20).abs() < 1e-9,
            "cost_median_usd expected 0.20, got {}", stats.cost_median_usd);

        // Wall times sorted [60, 120, 180]; p50 idx=(50*3-1)/100=1 → 120
        assert_eq!(stats.wall_p50_secs, 120, "wall_p50 of [60,120,180] should be 120");
        // p95 idx=(95*3-1)/100=2 → 180
        assert_eq!(stats.wall_p95_secs, 180, "wall_p95 of [60,120,180] should be 180");

        // last10: 4 closed tasks (3 done + 1 blocked), oldest→newest → ✓✓✓✗
        assert_eq!(stats.last10.chars().count(), 4);
        let chars: Vec<char> = stats.last10.chars().collect();
        assert_eq!(chars[3], '✗', "last closed task is blocked → ✗");
        assert!(chars[..3].iter().all(|&c| c == '✓'), "first 3 are done → ✓");
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
