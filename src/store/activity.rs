//! Per-task activity log — timestamped notes agents append during work.

use super::{now_ts, Store, TaskComment};
use anyhow::{bail, Result};
use rusqlite::params;

impl Store {
    /// Append a comment to a task. Returns the comment id.
    /// Errors if the task does not exist.
    pub fn task_comment(&self, task_id: i64, agent: &str, body: &str) -> Result<i64> {
        let body = body.trim();
        if body.is_empty() {
            bail!("comment body cannot be empty");
        }
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            [task_id],
            |r| r.get(0),
        )?;
        if !exists {
            bail!("task #{task_id} does not exist");
        }
        conn.execute(
            "INSERT INTO task_activity (task_id, agent, body, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![task_id, agent, body, now_ts()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Return all comments for a task, oldest first.
    pub fn task_comments(&self, task_id: i64) -> Result<Vec<TaskComment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, task_id, agent, body, created_at
             FROM task_activity WHERE task_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([task_id], |r| {
            Ok(TaskComment {
                id: r.get(0)?,
                task_id: r.get(1)?,
                agent: r.get(2)?,
                body: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();

        let c1 = s.task_comment(id, "builder", "started working on it").unwrap();
        let c2 = s.task_comment(id, "reviewer", "looks good so far").unwrap();

        let comments = s.task_comments(id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, c1);
        assert_eq!(comments[0].agent, "builder");
        assert_eq!(comments[0].body, "started working on it");
        assert_eq!(comments[1].id, c2);
        assert_eq!(comments[1].agent, "reviewer");
        assert_eq!(comments[1].task_id, id);
    }

    #[test]
    fn empty_body_rejected() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        assert!(s.task_comment(id, "builder", "").is_err());
        assert!(s.task_comment(id, "builder", "   ").is_err());
    }

    #[test]
    fn comment_on_nonexistent_task_errors() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.task_comment(9999, "builder", "hello").is_err());
    }

    #[test]
    fn no_comments_returns_empty() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        assert!(s.task_comments(id).unwrap().is_empty());
    }
}
