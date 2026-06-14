//! Task board queries. The scheduler's key query is [`Store::next_claimable`]:
//! highest-priority open task whose dependencies are all done.

use super::{now_ts, Store, Task, TaskSnapshot, TaskStatus};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, Row};

fn parse_tags(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn task_from_row(row: &Row) -> rusqlite::Result<Task> {
    let tags_raw: String = row.get("tags").unwrap_or_default();
    let requires_raw: String = row.get("requires").unwrap_or_default();
    let pinned: i64 = row.get("pinned").unwrap_or(0);
    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row.get("description")?,
        status: TaskStatus::parse(&row.get::<_, String>("status")?).unwrap_or(TaskStatus::Open),
        priority: row.get("priority")?,
        claimed_by: row.get("claimed_by")?,
        blocked_reason: row.get("blocked_reason")?,
        note: row.get("note")?,
        tags: parse_tags(&tags_raw),
        pinned: pinned != 0,
        due_at: row.get("due_at").unwrap_or(None),
        timeout_mins: row.get::<_, Option<i64>>("timeout_mins").unwrap_or(None).map(|v| v as u64),
        requires: parse_tags(&requires_raw),
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
        self.task_list_filtered(status, search, None)
    }

    /// Full task list with optional status, keyword search, and tag filter.
    /// Hub uses `tag` when the client passes `--tag <label>`.
    pub fn task_list_filtered(
        &self,
        status: Option<TaskStatus>,
        search: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<Task>> {
        let conn = self.conn.lock().unwrap();
        let search_clause = search
            .filter(|s| !s.is_empty())
            .map(|s| {
                let pattern = format!("%{}%", s.replace('%', "%%"));
                (pattern, "(title LIKE ? OR description LIKE ?)".to_string())
            });

        let overdue_sort =
            "CASE WHEN due_at IS NOT NULL AND due_at < strftime('%s','now') THEN 0 ELSE 1 END";
        let mut tasks: Vec<Task> = match status {
            Some(ref s) => {
                let mut sql = "SELECT * FROM tasks WHERE status = ?1".to_string();
                if let Some((_, clause)) = &search_clause {
                    sql.push_str(&format!(" AND {clause}"));
                }
                sql.push_str(&format!(" ORDER BY {overdue_sort}, priority, id"));
                let mut stmt = conn.prepare_cached(&sql)?;
                if let Some((ref pattern, _)) = search_clause {
                    let rows = stmt.query_map(
                        params![s.as_str(), pattern, pattern],
                        task_from_row,
                    )?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    let rows = stmt.query_map([s.as_str()], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                }
            }
            None => {
                if let Some((ref pattern, ref clause)) = search_clause {
                    let sql = format!(
                        "SELECT * FROM tasks WHERE id IN (SELECT id FROM tasks WHERE {clause}) \
                         ORDER BY \
                           CASE status WHEN 'claimed' THEN 0 WHEN 'open' THEN 1 \
                                       WHEN 'blocked' THEN 2 ELSE 3 END, \
                           pinned DESC, {overdue_sort}, priority, id"
                    );
                    let mut stmt = conn.prepare_cached(&sql)?;
                    let rows = stmt.query_map(params![pattern, pattern], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    let sql = format!(
                        "SELECT * FROM tasks ORDER BY \
                           CASE status WHEN 'claimed' THEN 0 WHEN 'open' THEN 1 \
                                       WHEN 'blocked' THEN 2 ELSE 3 END, \
                           pinned DESC, {overdue_sort}, priority, id"
                    );
                    let mut stmt = conn.prepare_cached(&sql)?;
                    let rows = stmt.query_map([], task_from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                }
            }
        };
        for t in &mut tasks {
            load_deps(&conn, t)?;
        }
        drop(conn);
        if let Some(label) = tag
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
        {
            tasks.retain(|t| t.tags.contains(&label));
        }
        Ok(tasks)
    }

    /// Highest-priority open task whose dependencies are all done, excluding
    /// any the scheduler was told to skip (already suggested elsewhere).
    ///
    /// `capabilities` is the claiming agent's capability set. Tasks with
    /// non-empty `requires` are only returned when the agent has ALL listed
    /// capabilities. An empty `capabilities` slice skips capability filtering
    /// (backward-compatible: agents without declared capabilities can claim any task).
    pub fn next_claimable(&self, exclude: &[i64], capabilities: &[String]) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let exclude_csv = exclude
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        // When capabilities are declared we may need to skip tasks with
        // unmet requirements — fetch a small batch and post-filter in Rust.
        let limit = if capabilities.is_empty() { 1 } else { 50 };
        let sql = format!(
            "SELECT * FROM tasks t
             WHERE t.status = 'open'
               AND NOT EXISTS (
                   SELECT 1 FROM task_deps d
                   JOIN tasks dt ON dt.id = d.depends_on_id
                   WHERE d.task_id = t.id AND dt.status != 'done'
               )
               {}
             ORDER BY t.pinned DESC,
                      CASE WHEN t.due_at IS NOT NULL AND t.due_at < strftime('%s','now') THEN 0 ELSE 1 END,
                      t.priority, t.id LIMIT {limit}",
            if exclude.is_empty() {
                String::new()
            } else {
                format!("AND t.id NOT IN ({exclude_csv})")
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        let candidates: Vec<Task> = stmt
            .query_map([], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for mut t in candidates {
            // Skip tasks whose requirements exceed the agent's capabilities.
            if !capabilities.is_empty() && !t.requires.is_empty() {
                if !t.requires.iter().all(|r| capabilities.contains(r)) {
                    continue;
                }
            }
            load_deps(&conn, &mut t)?;
            return Ok(Some(t));
        }
        Ok(None)
    }

    /// Set the capability requirements on a task (comma-separated labels).
    /// Pass an empty slice to clear all requirements.
    pub fn task_set_requires(&self, id: i64, requires: &[String]) -> Result<()> {
        let raw = requires
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE tasks SET requires = ?1, updated_at = ?2 WHERE id = ?3",
            params![raw, now_ts(), id],
        )?;
        if rows == 0 {
            anyhow::bail!("task #{id} not found");
        }
        Ok(())
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

    /// Manually route a task to a specific agent, bypassing the normal claim
    /// flow (no dep check, works on open/blocked tasks). Errors if the task
    /// is already claimed by a *different* agent or if it is already done.
    pub fn task_assign(&self, id: i64, agent: &str) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        // Check current status first so we can give a clear error.
        let (status, claimed_by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, claimed_by FROM tasks WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("task #{id} does not exist"))?;

        match status.as_str() {
            "done" => bail!("task #{id} is already done"),
            "claimed" => {
                if claimed_by.as_deref() != Some(agent) {
                    bail!(
                        "task #{id} is already claimed by {}",
                        claimed_by.unwrap_or_default()
                    );
                }
                // Already assigned to the same agent — treat as success.
                drop(conn);
                return Ok(self.task_get(id)?.expect("exists"));
            }
            _ => {}
        }

        conn.execute(
            "UPDATE tasks SET status = 'claimed', claimed_by = ?1, updated_at = ?2 WHERE id = ?3",
            params![agent, now_ts(), id],
        )?;
        drop(conn);
        Ok(self.task_get(id)?.expect("just assigned"))
    }

    /// Clone a task: copy title, description, and priority into a new open task.
    /// The clone has no claimed_by, no note, and no dependencies.
    pub fn task_clone(&self, id: i64, created_by: &str) -> Result<Task> {
        let src = self
            .task_get(id)?
            .ok_or_else(|| anyhow::anyhow!("task #{id} does not exist"))?;
        let new_id = self.task_add(&src.title, &src.description, src.priority, &[], created_by)?;
        Ok(self.task_get(new_id)?.expect("just created"))
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

    /// Add a tag label to a task. No-ops if the label is already present.
    pub fn task_tag(&self, id: i64, label: &str) -> Result<()> {
        let label = label.trim().to_lowercase();
        anyhow::ensure!(!label.is_empty(), "tag label cannot be empty");
        anyhow::ensure!(
            !label.contains(','),
            "tag label cannot contain commas"
        );
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row("SELECT tags FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .map_err(|_| anyhow::anyhow!("task #{id} does not exist"))?;
        let raw = raw.unwrap_or_default();
        let mut tags = parse_tags(&raw);
        if !tags.contains(&label) {
            tags.push(label);
            let new_raw = tags.join(",");
            conn.execute(
                "UPDATE tasks SET tags = ?1, updated_at = ?2 WHERE id = ?3",
                params![new_raw, now_ts(), id],
            )?;
        }
        Ok(())
    }

    /// Remove a tag label from a task. No-ops if the label is not present.
    pub fn task_untag(&self, id: i64, label: &str) -> Result<()> {
        let label = label.trim().to_lowercase();
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row("SELECT tags FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .map_err(|_| anyhow::anyhow!("task #{id} does not exist"))?;
        let raw = raw.unwrap_or_default();
        let tags: Vec<String> = parse_tags(&raw)
            .into_iter()
            .filter(|t| t != &label)
            .collect();
        let new_raw = tags.join(",");
        conn.execute(
            "UPDATE tasks SET tags = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_raw, now_ts(), id],
        )?;
        Ok(())
    }

    /// Set or clear the per-task timeout (minutes claimed before auto-block).
    pub fn task_set_timeout(&self, id: i64, timeout_mins: Option<u64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET timeout_mins = ?1, updated_at = ?2 WHERE id = ?3",
            params![timeout_mins.map(|v| v as i64), now_ts(), id],
        )?;
        if updated == 0 {
            bail!("task #{id} not found");
        }
        Ok(())
    }

    /// Return tasks that have been claimed past their per-task timeout.
    /// Scheduler calls this to find tasks to auto-block.
    pub fn timed_out_tasks(&self) -> Result<Vec<Task>> {
        let conn = self.conn.lock().unwrap();
        // Find claimed tasks where (now - updated_at) > timeout_mins * 60
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM tasks
             WHERE status = 'claimed'
               AND timeout_mins IS NOT NULL
               AND (strftime('%s','now') - updated_at) > timeout_mins * 60",
        )?;
        let collected: Vec<Task> = {
            let rows = stmt.query_map([], task_from_row)?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        drop(stmt);
        let mut tasks = collected;
        for t in &mut tasks {
            load_deps(&conn, t)?;
        }
        Ok(tasks)
    }

    /// Set or clear the due date for a task.
    /// Pass `None` to clear an existing due date.
    pub fn task_set_due(&self, id: i64, due_at: Option<i64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET due_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![due_at, now_ts(), id],
        )?;
        if updated == 0 {
            bail!("task #{id} not found");
        }
        Ok(())
    }

    /// Pin a task so it sorts before all non-pinned tasks in suggestion order.
    pub fn task_pin(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET pinned = 1, updated_at = ?1 WHERE id = ?2",
            params![now_ts(), id],
        )?;
        if updated == 0 {
            bail!("task #{id} not found");
        }
        Ok(())
    }

    /// Unpin a task.
    pub fn task_unpin(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tasks SET pinned = 0, updated_at = ?1 WHERE id = ?2",
            params![now_ts(), id],
        )?;
        if updated == 0 {
            bail!("task #{id} not found");
        }
        Ok(())
    }

    /// Return the tag list for a task.
    pub fn task_tags(&self, id: i64) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row("SELECT tags FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .map_err(|_| anyhow::anyhow!("task #{id} does not exist"))?;
        Ok(parse_tags(&raw.unwrap_or_default()))
    }

    /// Export every task as a portable snapshot (all statuses, ordered by id).
    pub fn task_export_all(&self) -> Result<Vec<TaskSnapshot>> {
        // Use ID order for a stable, reproducible snapshot (not the display sort).
        let conn = self.conn.lock().unwrap();
        let mut tasks: Vec<Task> = {
            let mut stmt = conn.prepare_cached("SELECT * FROM tasks ORDER BY id")?;
            let collected: Vec<Task> = stmt
                .query_map([], task_from_row)?
                .collect::<rusqlite::Result<_>>()?;
            collected
        };
        for t in &mut tasks {
            load_deps(&conn, t)?;
        }
        drop(conn);
        let snapshots = tasks
            .into_iter()
            .map(|t| TaskSnapshot {
                source_id: Some(t.id),
                title: t.title,
                description: t.description,
                priority: t.priority,
                status: t.status.as_str().to_string(),
                tags: t.tags,
                depends_on: t.depends_on,
                due_at: t.due_at,
                timeout_mins: t.timeout_mins,
                requires: t.requires,
            })
            .collect();
        Ok(snapshots)
    }

    /// Bulk-import tasks from a snapshot, remapping dep IDs from the source
    /// DB to the newly created task IDs in this DB.
    /// Returns the list of new task IDs in snapshot order.
    pub fn bulk_import_tasks(
        &self,
        snapshots: &[TaskSnapshot],
        created_by: &str,
    ) -> Result<Vec<i64>> {
        use std::collections::HashMap;

        // Map source_id -> new_id so we can remap dep edges.
        let mut id_map: HashMap<i64, i64> = HashMap::new();
        let mut new_ids: Vec<i64> = Vec::with_capacity(snapshots.len());

        // First pass: create all tasks without deps so we can build the id map.
        for snap in snapshots {
            let new_id =
                self.task_add(&snap.title, &snap.description, snap.priority, &[], created_by)?;
            if let Some(src) = snap.source_id {
                id_map.insert(src, new_id);
            }
            new_ids.push(new_id);
        }

        // Second pass: wire up deps and restore tags + status.
        let conn = self.conn.lock().unwrap();
        for (snap, &new_id) in snapshots.iter().zip(new_ids.iter()) {
            // Remap dep IDs.
            for src_dep in &snap.depends_on {
                if let Some(&new_dep) = id_map.get(src_dep) {
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO task_deps (task_id, depends_on_id) VALUES (?1, ?2)",
                        params![new_id, new_dep],
                    );
                }
            }
            // Restore tags.
            if !snap.tags.is_empty() {
                let raw = snap.tags.join(",");
                let _ = conn.execute(
                    "UPDATE tasks SET tags = ?1 WHERE id = ?2",
                    params![raw, new_id],
                );
            }
            // Restore status if not open (e.g. done tasks in an archive import).
            let status = snap.status.as_str();
            if status != "open" {
                let _ = conn.execute(
                    "UPDATE tasks SET status = ?1 WHERE id = ?2",
                    params![status, new_id],
                );
            }
            // Restore due date if present.
            if snap.due_at.is_some() {
                let _ = conn.execute(
                    "UPDATE tasks SET due_at = ?1 WHERE id = ?2",
                    params![snap.due_at, new_id],
                );
            }
            // Restore timeout if present.
            if let Some(tm) = snap.timeout_mins {
                let _ = conn.execute(
                    "UPDATE tasks SET timeout_mins = ?1 WHERE id = ?2",
                    params![tm as i64, new_id],
                );
            }
        }
        Ok(new_ids)
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
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, a);

        s.task_claim(a, "builder").unwrap();
        assert!(s.next_claimable(&[], &[]).unwrap().is_none());

        s.task_done(a, "builder", None).unwrap();
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
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
        assert!(s.next_claimable(&[], &[]).unwrap().is_none());
        s.task_reopen(id).unwrap();
        assert_eq!(s.next_claimable(&[], &[]).unwrap().unwrap().id, id);
    }

    #[test]
    fn release_claims_on_crash() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        s.task_claim(id, "alice").unwrap();
        assert_eq!(s.release_claims("alice").unwrap(), 1);
        assert_eq!(s.next_claimable(&[], &[]).unwrap().unwrap().id, id);
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
    fn task_clone_copies_fields_and_resets_state() {
        let s = Store::open_in_memory().unwrap();
        let src = s.task_add("original", "desc here", 1, &[], "human").unwrap();
        s.task_claim(src, "alice").unwrap();
        s.task_done(src, "alice", Some("completed")).unwrap();

        let cloned = s.task_clone(src, "builder").unwrap();

        assert_ne!(cloned.id, src);
        assert_eq!(cloned.title, "original");
        assert_eq!(cloned.description, "desc here");
        assert_eq!(cloned.priority, 1);
        assert_eq!(cloned.status, TaskStatus::Open);
        assert!(cloned.claimed_by.is_none());
        assert!(cloned.note.is_none());
        assert!(cloned.depends_on.is_empty());
    }

    #[test]
    fn task_clone_nonexistent_errors() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.task_clone(9999, "builder").is_err());
    }

    #[test]
    fn timed_out_tasks_returned() {
        let s = Store::open_in_memory().unwrap();
        let a = s.task_add("short", "", 1, &[], "human").unwrap();
        let b = s.task_add("long", "", 1, &[], "human").unwrap();

        s.task_claim(a, "alice").unwrap();
        s.task_claim(b, "bob").unwrap();

        // Set a 1-minute timeout and backdate updated_at by 2 minutes to simulate expiry.
        s.task_set_timeout(a, Some(1)).unwrap();
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET updated_at = updated_at - 120 WHERE id = ?1",
                [a],
            ).unwrap();
        }
        s.task_set_timeout(b, Some(60)).unwrap(); // 60 min — not expired yet

        let timed_out = s.timed_out_tasks().unwrap();
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].id, a);
    }

    #[test]
    fn timeout_set_and_clear() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        s.task_set_timeout(id, Some(30)).unwrap();
        let t = s.task_get(id).unwrap().unwrap();
        assert_eq!(t.timeout_mins, Some(30));

        s.task_set_timeout(id, None).unwrap();
        let t = s.task_get(id).unwrap().unwrap();
        assert_eq!(t.timeout_mins, None);
    }

    #[test]
    fn due_date_overdue_task_sorted_before_non_overdue() {
        let s = Store::open_in_memory().unwrap();
        let normal = s.task_add("normal", "", 1, &[], "human").unwrap();
        let overdue = s.task_add("overdue", "", 2, &[], "human").unwrap(); // lower priority

        // Set due date in the past (overdue).
        s.task_set_due(overdue, Some(1_000_000)).unwrap(); // Unix epoch + 1M secs ≈ 1982

        // Scheduler should prefer overdue over normal despite lower priority.
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, overdue);
        assert_eq!(next.due_at, Some(1_000_000));

        // Clear due date — normal should win again.
        s.task_set_due(overdue, None).unwrap();
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, normal);
        assert_eq!(next.due_at, None);
    }

    #[test]
    fn due_date_future_does_not_change_order() {
        let s = Store::open_in_memory().unwrap();
        let high = s.task_add("high priority", "", 0, &[], "human").unwrap();
        let low = s.task_add("low priority", "", 3, &[], "human").unwrap();

        // Set future due date on low-priority task — should NOT boost it.
        let future_ts = now_ts() + 86_400; // 1 day in the future
        s.task_set_due(low, Some(future_ts)).unwrap();

        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, high, "future due date should not change ordering");
    }

    #[test]
    fn tag_filter_in_task_list_filtered() {
        let s = Store::open_in_memory().unwrap();
        let a = s.task_add("alpha", "", 1, &[], "human").unwrap();
        let b = s.task_add("beta", "", 1, &[], "human").unwrap();
        let c = s.task_add("gamma", "", 1, &[], "human").unwrap();

        s.task_tag(a, "bug").unwrap();
        s.task_tag(a, "urgent").unwrap();
        s.task_tag(b, "bug").unwrap();
        // c has no tags

        let bugs = s.task_list_filtered(None, None, Some("bug")).unwrap();
        let ids: Vec<i64> = bugs.iter().map(|t| t.id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        assert!(!ids.contains(&c));

        let urgent = s.task_list_filtered(None, None, Some("urgent")).unwrap();
        assert_eq!(urgent.len(), 1);
        assert_eq!(urgent[0].id, a);

        // None tag == same as task_list
        let all = s.task_list_filtered(None, None, None).unwrap();
        assert_eq!(all.len(), 3);

        // Case-insensitive filter
        let bugs_upper = s.task_list_filtered(None, None, Some("BUG")).unwrap();
        assert_eq!(bugs_upper.len(), 2);
    }

    #[test]
    fn pinned_task_sorted_first() {
        let s = Store::open_in_memory().unwrap();
        let normal = s.task_add("normal", "", 1, &[], "human").unwrap();
        let pinned = s.task_add("pinned", "", 2, &[], "human").unwrap(); // lower priority
        s.task_pin(pinned).unwrap();

        // next_claimable should prefer the pinned task despite lower numeric priority.
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, pinned);
        assert!(next.pinned);

        s.task_unpin(pinned).unwrap();
        let next = s.next_claimable(&[], &[]).unwrap().unwrap();
        assert_eq!(next.id, normal, "after unpin, higher priority wins");
    }

    #[test]
    fn pin_nonexistent_errors() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.task_pin(9999).is_err());
        assert!(s.task_unpin(9999).is_err());
    }

    #[test]
    fn export_all_and_bulk_import_roundtrip() {
        let src = Store::open_in_memory().unwrap();
        let a = src.task_add("alpha", "first task", 1, &[], "human").unwrap();
        let b = src.task_add("beta", "second task", 0, &[a], "human").unwrap();
        src.task_tag(a, "bug").unwrap();
        src.task_claim(a, "alice").unwrap();
        src.task_done(a, "alice", Some("done")).unwrap();

        let snapshots = src.task_export_all().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].title, "alpha");
        assert_eq!(snapshots[0].tags, vec!["bug"]);
        assert_eq!(snapshots[0].status, "done");
        assert_eq!(snapshots[1].depends_on, vec![a]);

        // Import into a fresh DB.
        let dst = Store::open_in_memory().unwrap();
        let new_ids = dst.bulk_import_tasks(&snapshots, "importer").unwrap();
        assert_eq!(new_ids.len(), 2);

        let na = new_ids[0];
        let nb = new_ids[1];

        let ta = dst.task_get(na).unwrap().unwrap();
        assert_eq!(ta.title, "alpha");
        assert_eq!(ta.tags, vec!["bug"]);
        assert_eq!(ta.status, TaskStatus::Done);

        let tb = dst.task_get(nb).unwrap().unwrap();
        assert_eq!(tb.title, "beta");
        assert_eq!(tb.depends_on, vec![na], "dep remapped to new ID");

        // Dep gate: beta should not be claimable because alpha is not done
        // in the original — but we imported alpha as done, so beta IS claimable.
        assert!(dst.next_claimable(&[], &[]).unwrap().is_some());
    }

    #[test]
    fn import_without_source_id_still_creates_tasks() {
        let dst = Store::open_in_memory().unwrap();
        use crate::store::TaskSnapshot;
        let snaps = vec![
            TaskSnapshot {
                title: "t1".into(),
                description: "".into(),
                priority: 2,
                status: "open".into(),
                tags: vec![],
                depends_on: vec![],
                source_id: None,
                due_at: None,
                timeout_mins: None,
                requires: vec![],
            },
            TaskSnapshot {
                title: "t2".into(),
                description: "".into(),
                priority: 2,
                status: "open".into(),
                tags: vec![],
                depends_on: vec![],
                source_id: None,
                due_at: None,
                timeout_mins: None,
                requires: vec![],
            },
        ];
        let ids = dst.bulk_import_tasks(&snaps, "bot").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(dst.task_get(ids[0]).unwrap().is_some());
        assert!(dst.task_get(ids[1]).unwrap().is_some());
    }

    #[test]
    fn task_tag_and_untag() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();

        s.task_tag(id, "bug").unwrap();
        s.task_tag(id, "  Bug  ").unwrap(); // duplicate (normalised) — should be idempotent
        s.task_tag(id, "urgent").unwrap();
        let tags = s.task_tags(id).unwrap();
        assert_eq!(tags, vec!["bug", "urgent"]);

        s.task_untag(id, "bug").unwrap();
        let tags = s.task_tags(id).unwrap();
        assert_eq!(tags, vec!["urgent"]);

        // Untagging a non-existent label is a no-op.
        s.task_untag(id, "missing").unwrap();
        let tags = s.task_tags(id).unwrap();
        assert_eq!(tags, vec!["urgent"]);

        // Tags should be visible on task_get.
        let t = s.task_get(id).unwrap().unwrap();
        assert_eq!(t.tags, vec!["urgent"]);
    }

    #[test]
    fn task_tag_empty_label_rejected() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        assert!(s.task_tag(id, "").is_err());
        assert!(s.task_tag(id, "  ").is_err());
    }

    #[test]
    fn task_tag_comma_rejected() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("t", "", 2, &[], "human").unwrap();
        assert!(s.task_tag(id, "foo,bar").is_err());
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

    #[test]
    fn capability_filter_in_next_claimable() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("rust task", "", 2, &[], "human").unwrap();
        s.task_set_requires(id, &["rust".to_string()]).unwrap();

        // Agent without capabilities: any task is shown (backward-compat).
        assert!(s.next_claimable(&[], &[]).unwrap().is_some());

        // Agent with the required capability: task is offered.
        let caps = vec!["rust".to_string(), "backend".to_string()];
        assert_eq!(s.next_claimable(&[], &caps).unwrap().unwrap().id, id);

        // Agent missing the required capability: task is skipped.
        let wrong_caps = vec!["frontend".to_string()];
        assert!(s.next_claimable(&[], &wrong_caps).unwrap().is_none());
    }

    #[test]
    fn task_set_requires_round_trips() {
        let s = Store::open_in_memory().unwrap();
        let id = s.task_add("work", "", 2, &[], "human").unwrap();
        s.task_set_requires(id, &["rust".to_string(), "sql".to_string()]).unwrap();
        let t = s.task_get(id).unwrap().unwrap();
        assert!(t.requires.contains(&"rust".to_string()));
        assert!(t.requires.contains(&"sql".to_string()));

        // Clear requirements.
        s.task_set_requires(id, &[]).unwrap();
        let t = s.task_get(id).unwrap().unwrap();
        assert!(t.requires.is_empty());
    }
}
