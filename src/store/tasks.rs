//! Task board queries. The scheduler's key query is [`Store::next_claimable`]:
//! highest-priority open task whose dependencies are all done.

use super::{now_ts, Store, Task, TaskStatus};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, Row};

fn task_from_row(row: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row.get("description")?,
        status: TaskStatus::parse(&row.get::<_, String>("status")?).unwrap_or(TaskStatus::Open),
        priority: row.get("priority")?,
        claimed_by: row.get("claimed_by")?,
        blocked_reason: row.get("blocked_reason")?,
        note: row.get("note")?,
        created_by: row.get("created_by")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        depends_on: Vec::new(),
    })
}

fn load_deps(conn: &Connection, task: &mut Task) -> rusqlite::Result<()> {
    let mut stmt =
        conn.prepare_cached("SELECT depends_on_id FROM task_deps WHERE task_id = ?1")?;
    task.depends_on = stmt
        .query_map([task.id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(())
}

impl Store {
    pub fn task_add(
        &self,
        title: &str,
        description: &str,
        priority: i64,
        depends_on: &[i64],
        created_by: &str,
    ) -> Result<i64> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;
        let now = now_ts();
        for dep in depends_on {
            let exists: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)", [dep], |r| {
                    r.get(0)
                })?;
            if !exists {
                bail!("dependency task #{dep} does not exist");
            }
        }
        tx.execute(
            "INSERT INTO tasks (title, description, priority, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![title, description, priority, created_by, now],
        )?;
        let id = tx.last_insert_rowid();
        for dep in depends_on {
            tx.execute(
                "INSERT OR IGNORE INTO task_deps (task_id, depends_on_id) VALUES (?1, ?2)",
                params![id, dep],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn task_get(&self, id: i64) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached("SELECT * FROM tasks WHERE id = ?1")?;
        let task = stmt.query_row([id], task_from_row);
        match task {
            Ok(mut t) => {
                load_deps(&conn, &mut t)?;
                Ok(Some(t))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn task_list(&self, status: Option<TaskStatus>) -> Result<Vec<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut tasks = match status {
            Some(s) => {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM tasks WHERE status = ?1 ORDER BY priority, id",
                )?;
                let rows = stmt.query_map([s.as_str()], task_from_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM tasks ORDER BY
                       CASE status WHEN 'claimed' THEN 0 WHEN 'open' THEN 1
                                   WHEN 'blocked' THEN 2 ELSE 3 END,
                       priority, id",
                )?;
                let rows = stmt.query_map([], task_from_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        for t in &mut tasks {
            load_deps(&conn, t)?;
        }
        Ok(tasks)
    }

    /// Highest-priority open task whose dependencies are all done, excluding
    /// any the scheduler was told to skip (already suggested elsewhere).
    pub fn next_claimable(&self, exclude: &[i64]) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let exclude_csv = exclude
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT * FROM tasks t
             WHERE t.status = 'open'
               AND NOT EXISTS (
                   SELECT 1 FROM task_deps d
                   JOIN tasks dt ON dt.id = d.depends_on_id
                   WHERE d.task_id = t.id AND dt.status != 'done'
               )
               {}
             ORDER BY t.priority, t.id LIMIT 1",
            if exclude.is_empty() {
                String::new()
            } else {
                format!("AND t.id NOT IN ({exclude_csv})")
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        match stmt.query_row([], task_from_row) {
            Ok(mut t) => {
                load_deps(&conn, &mut t)?;
                Ok(Some(t))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The task an agent currently has claimed, if any.
    pub fn claimed_task(&self, agent: &str) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM tasks WHERE status = 'claimed' AND claimed_by = ?1
             ORDER BY priority, id LIMIT 1",
        )?;
        match stmt.query_row([agent], task_from_row) {
            Ok(mut t) => {
                load_deps(&conn, &mut t)?;
                Ok(Some(t))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn task_claim(&self, id: i64, agent: &str) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET status = 'claimed', claimed_by = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'open'",
            params![agent, now_ts(), id],
        )?;
        if updated == 0 {
            let status: Option<String> = conn
                .query_row("SELECT status FROM tasks WHERE id = ?1", [id], |r| r.get(0))
                .ok();
            match status {
                None => bail!("task #{id} does not exist"),
                Some(s) => bail!("task #{id} is not open (status: {s})"),
            }
        }
        drop(conn);
        Ok(self.task_get(id)?.expect("just claimed"))
    }

    pub fn task_done(&self, id: i64, agent: &str, note: Option<&str>) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET status = 'done', note = COALESCE(?1, note), updated_at = ?2
             WHERE id = ?3 AND (claimed_by = ?4 OR ?4 = 'human' OR status = 'open')",
            params![note, now_ts(), id, agent],
        )?;
        if updated == 0 {
            bail!("task #{id} not found, or claimed by another agent");
        }
        drop(conn);
        Ok(self.task_get(id)?.expect("just completed"))
    }

    pub fn task_block(&self, id: i64, agent: &str, reason: &str) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET status = 'blocked', blocked_reason = ?1, claimed_by = NULL,
                              updated_at = ?2
             WHERE id = ?3 AND status IN ('open', 'claimed')
               AND (claimed_by IS NULL OR claimed_by = ?4 OR ?4 = 'human')",
            params![reason, now_ts(), id, agent],
        )?;
        if updated == 0 {
            bail!("task #{id} not found, already done, or claimed by another agent");
        }
        drop(conn);
        Ok(self.task_get(id)?.expect("just blocked"))
    }

    /// Reopen a blocked task (human action, or an agent that resolved the blocker).
    pub fn task_reopen(&self, id: i64) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET status = 'open', blocked_reason = NULL, claimed_by = NULL,
                              updated_at = ?1
             WHERE id = ?2 AND status IN ('blocked', 'claimed')",
            params![now_ts(), id],
        )?;
        if updated == 0 {
            bail!("task #{id} not found or not blocked/claimed");
        }
        drop(conn);
        Ok(self.task_get(id)?.expect("just reopened"))
    }

    /// Release an agent's claims back to open (used when an agent crashes
    /// without resume, so its work isn't stranded).
    pub fn release_claims(&self, agent: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE tasks SET status = 'open', claimed_by = NULL, updated_at = ?1
             WHERE status = 'claimed' AND claimed_by = ?2",
            params![now_ts(), agent],
        )?)
    }

    pub fn open_task_count(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('open','claimed')",
            [],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_ordering() {
        let s = Store::open_in_memory().unwrap();
        let a = s.task_add("first", "", 1, &[], "human").unwrap();
        let b = s.task_add("second", "", 0, &[a], "human").unwrap();

        // b is higher priority but blocked by dep — a must come first.
        let next = s.next_claimable(&[]).unwrap().unwrap();
        assert_eq!(next.id, a);

        s.task_claim(a, "builder").unwrap();
        assert!(s.next_claimable(&[]).unwrap().is_none());

        s.task_done(a, "builder", None).unwrap();
        let next = s.next_claimable(&[]).unwrap().unwrap();
        assert_eq!(next.id, b);
        assert_eq!(next.depends_on, vec![a]);
    }

    #[test]
    fn claim_conflicts_rejected() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        s.task_claim(id, "alice").unwrap();
        assert!(s.task_claim(id, "bob").is_err());
        assert!(s.task_done(id, "bob", None).is_err());
        s.task_done(id, "alice", Some("done it")).unwrap();
        let t = s.task_get(id).unwrap().unwrap();
        assert_eq!(t.status, TaskStatus::Done);
        assert_eq!(t.note.as_deref(), Some("done it"));
    }

    #[test]
    fn block_and_reopen() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        s.task_claim(id, "alice").unwrap();
        s.task_block(id, "alice", "waiting on schema").unwrap();
        assert!(s.next_claimable(&[]).unwrap().is_none());
        s.task_reopen(id).unwrap();
        assert_eq!(s.next_claimable(&[]).unwrap().unwrap().id, id);
    }

    #[test]
    fn release_claims_on_crash() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        s.task_claim(id, "alice").unwrap();
        assert_eq!(s.release_claims("alice").unwrap(), 1);
        assert_eq!(s.next_claimable(&[]).unwrap().unwrap().id, id);
    }

    #[test]
    fn missing_dep_rejected() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.task_add("t", "", 2, &[999], "human").is_err());
    }
}
