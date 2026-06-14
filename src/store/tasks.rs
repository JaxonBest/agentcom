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
    let mut stmt = conn.prepare_cached("SELECT depends_on_id FROM task_deps WHERE task_id = ?1")?;
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
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
                [dep],
                |r| r.get(0),
            )?;
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

    pub fn task_update(
        &self,
        id: i64,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<i64>,
    ) -> Result<Task> {
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(t) = title {
            sets.push("title = ?".to_string());
            params.push(Box::new(t.to_string()));
        }
        if let Some(d) = description {
            sets.push("description = ?".to_string());
            params.push(Box::new(d.to_string()));
        }
        if let Some(p) = priority {
            sets.push("priority = ?".to_string());
            params.push(Box::new(p));
        }
        if sets.is_empty() {
            bail!("nothing to update");
        }
        sets.push("updated_at = ?".to_string());
        params.push(Box::new(now_ts()));

        params.push(Box::new(id));
        let sql = format!(
            "UPDATE tasks SET {} WHERE id = ?",
            sets.join(", ")
        );

        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(&sql, rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())))?;
        if updated == 0 {
            bail!("task #{id} not found");
        }
        drop(conn);
        Ok(self.task_get(id)?.expect("just updated"))
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

    pub fn task_list(
        &self,
        status: Option<TaskStatus>,
        search: Option<&str>,
    ) -> Result<Vec<Task>> {
        let conn = self.conn.lock().unwrap();
        let search_clause = search
            .filter(|s| !s.is_empty())
            .map(|s| {
                let pattern = format!("%{}%", s.replace('%', "%%"));
                (pattern, "(title LIKE ? OR description LIKE ?)".to_string())
            });

        let mut tasks: Vec<Task> = match status {
            Some(s) => {
                let mut sql = "SELECT * FROM tasks WHERE status = ?1".to_string();
                if let Some((_, clause)) = &search_clause {
                    sql.push_str(&format!(" AND {clause}"));
                }
                sql.push_str(" ORDER BY priority, id");
                let mut stmt = conn.prepare_cached(&sql)?;
                if let Some((pattern, _)) = search_clause.as_ref() {
                    let rows = stmt.query_map(params![s.as_str(), pattern, pattern], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    let rows = stmt.query_map([s.as_str()], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                }
            }
            None => {
                let mut sql = String::from(
                    "SELECT * FROM tasks ORDER BY
                       CASE status WHEN 'claimed' THEN 0 WHEN 'open' THEN 1
                                   WHEN 'blocked' THEN 2 ELSE 3 END,
                       priority, id",
                );
                if let Some((pattern, clause)) = &search_clause {
                    // Subquery: first gather matching IDs, then sort them
                    sql = format!(
                        "SELECT * FROM tasks WHERE id IN (SELECT id FROM tasks WHERE {clause})
                         ORDER BY
                           CASE status WHEN 'claimed' THEN 0 WHEN 'open' THEN 1
                                       WHEN 'blocked' THEN 2 ELSE 3 END,
                           priority, id",
                    );
                    let mut stmt = conn.prepare_cached(&sql)?;
                    let rows = stmt.query_map(params![pattern, pattern], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    let mut stmt = conn.prepare_cached(&sql)?;
                    let rows = stmt.query_map([], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                }
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

    /// Returns the IDs of any dependencies of `id` that are not yet done.
    /// Empty vec means the task is claimable from a dependency standpoint.
    pub fn task_unmet_deps(&self, id: i64) -> Result<Vec<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT d.depends_on_id FROM task_deps d
             JOIN tasks t ON t.id = d.depends_on_id
             WHERE d.task_id = ?1 AND t.status != 'done'",
        )?;
        let ids: Vec<i64> = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    pub fn task_claim(&self, id: i64, agent: &str) -> Result<Task> {
        // Check dependency gate before acquiring the write lock so the
        // error message can name the blocking task IDs.
        let unmet = self.task_unmet_deps(id)?;
        if !unmet.is_empty() {
            let blocking: Vec<String> = unmet.iter().map(|i| format!("#{i}")).collect();
            bail!(
                "task #{id} cannot be claimed: waiting on {}",
                blocking.join(", ")
            );
        }

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

    /// Permanently delete a task. Only allowed for open, done, or blocked
    /// tasks — claimed tasks must be released or completed first.
    pub fn task_delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Check the current status — refuse to delete claimed tasks.
        let status: Option<String> = conn
            .query_row("SELECT status FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .ok();
        match status {
            None => bail!("task #{id} not found"),
            Some(ref s) if s == "claimed" => {
                bail!("task #{id} is claimed — release or complete it first");
            }
            Some(_) => {}
        }

        // Remove dependency links where this task depends on others.
        conn.execute("DELETE FROM task_deps WHERE task_id = ?1", [id])?;
        // Remove dependency links where other tasks depend on this one.
        conn.execute("DELETE FROM task_deps WHERE depends_on_id = ?1", [id])?;
        // Delete the task itself.
        conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Delete old done/blocked tasks older than `before_secs` seconds.
    /// Also cleans up dependency links. Returns the number of tasks removed.
    pub fn task_prune(&self, before_secs: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = now_ts() - before_secs;

        // Gather IDs of tasks to prune.
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM tasks WHERE status IN ('done','blocked') AND updated_at < ?1",
        )?;
        let ids: Vec<i64> = stmt
            .query_map([cutoff], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if ids.is_empty() {
            return Ok(0);
        }

        // Delete dependency links involving these tasks.
        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let placeholders = placeholders.join(",");
        let params: Vec<&dyn rusqlite::types::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();

        conn.execute(
            &format!("DELETE FROM task_deps WHERE task_id IN ({placeholders})"),
            params.as_slice(),
        )?;
        conn.execute(
            &format!("DELETE FROM task_deps WHERE depends_on_id IN ({placeholders})"),
            params.as_slice(),
        )?;

        // Delete the tasks themselves.
        let count = conn.execute(
            &format!("DELETE FROM tasks WHERE id IN ({placeholders})"),
            params.as_slice(),
        )?;
        Ok(count)
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

    /// Reset every claimed task to open — called at session start so work
    /// from the previous run doesn't auto-resume on the next `agentcom up`.
    pub fn release_all_claimed(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE tasks SET status = 'open', claimed_by = NULL, updated_at = ?1
             WHERE status = 'claimed'",
            params![now_ts()],
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

    #[test]
    fn claim_blocked_by_unfinished_dep() {
        let s = Store::open_in_memory().unwrap();
        let parent = s.task_add("parent", "", 1, &[], "human").unwrap();
        let child = s.task_add("child", "", 0, &[parent], "human").unwrap();

        // Claiming child while parent is still open must fail.
        let err = s.task_claim(child, "alice").unwrap_err();
        assert!(
            err.to_string().contains(&format!("#{parent}")),
            "error should name the blocking task: {err}"
        );

        // Claiming the parent directly must succeed.
        s.task_claim(parent, "alice").unwrap();

        // Child still blocked while parent is claimed (not done).
        assert!(s.task_claim(child, "bob").is_err());

        // Complete parent — now child is claimable.
        s.task_done(parent, "alice", None).unwrap();
        s.task_claim(child, "bob").unwrap();
    }

    #[test]
    fn task_delete_removes_task_and_deps() {
        let s = Store::open_in_memory().unwrap();
        let a = s.task_add("parent", "", 1, &[], "human").unwrap();
        let b = s.task_add("child", "", 0, &[a], "human").unwrap();

        // Can't delete claimed task.
        s.task_claim(a, "alice").unwrap();
        assert!(s.task_delete(a).is_err());
        s.task_done(a, "alice", None).unwrap();

        // Delete the parent (now done) — child's dep link is cleaned up.
        s.task_delete(a).unwrap();
        assert!(s.task_get(a).unwrap().is_none());

        // Child should have no remaining deps.
        let child = s.task_get(b).unwrap().unwrap();
        assert!(child.depends_on.is_empty());
    }

    #[test]
    fn task_update_changes_fields() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("original title", "original desc", 2, &[], "human").unwrap();
        let t = s.task_update(id, Some("new title"), None, Some(0)).unwrap();
        assert_eq!(t.title, "new title");
        assert_eq!(t.description, "original desc");
        assert_eq!(t.priority, 0);
    }

    #[test]
    fn task_prune_removes_old_done_tasks() {
        let s = Store::open_in_memory().unwrap();
        let now = now_ts();

        // Create tasks with artificially old updated_at timestamps
        // by manipulating the database directly (since task_add uses now_ts()).
        let fresh = s.task_add("fresh task", "", 2, &[], "human").unwrap();
        let old_done = s.task_add("old done", "", 2, &[], "human").unwrap();
        let old_blocked = s.task_add("old blocked", "", 2, &[], "human").unwrap();
        let recent_done = s.task_add("recent done", "", 2, &[], "human").unwrap();

        // Set old tasks to have very old timestamps.
        let conn = s.conn.lock().unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = ?1, status = 'done' WHERE id = ?2",
            params![now - 100_000, old_done],
        ).unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = ?1, status = 'blocked' WHERE id = ?2",
            params![now - 100_000, old_blocked],
        ).unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = ?1, status = 'done' WHERE id = ?2",
            params![now - 100, recent_done],
        ).unwrap();
        // fresh stays as 'open' with current timestamp.
        drop(conn);

        // Prune tasks older than 1 day (86400 seconds).
        let count = s.task_prune(86_400).unwrap();
        assert_eq!(count, 2, "should prune old_done and old_blocked");

        assert!(s.task_get(old_done).unwrap().is_none());
        assert!(s.task_get(old_blocked).unwrap().is_none());
        assert!(s.task_get(recent_done).unwrap().is_some());
        assert!(s.task_get(fresh).unwrap().is_some());
    }

    #[test]
    fn task_prune_respects_deps_cleanup() {
        let s = Store::open_in_memory().unwrap();
        let now = now_ts();

        let parent = s.task_add("parent", "", 1, &[], "human").unwrap();
        let child = s.task_add("child", "", 0, &[parent], "human").unwrap();

        let conn = s.conn.lock().unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = ?1, status = 'done' WHERE id = ?2",
            params![now - 200_000, parent],
        ).unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = ?1, status = 'done' WHERE id = ?2",
            params![now - 200_000, child],
        ).unwrap();
        drop(conn);

        let count = s.task_prune(86_400).unwrap();
        assert_eq!(count, 2, "should prune both done tasks");

        // Deps table should have no stale rows.
        let conn = s.conn.lock().unwrap();
        let remaining_deps: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task_deps", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(remaining_deps, 0, "all dep links should be cleaned up");
    }
}
