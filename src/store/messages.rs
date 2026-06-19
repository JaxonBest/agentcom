//! Inter-agent message persistence. "all" broadcasts fan out into one row
//! per recipient at insert time so delivery tracking stays per-agent.

use super::{now_ts, Message, Store};
use anyhow::Result;
use rusqlite::{params, Row};

fn msg_from_row(row: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get("id")?,
        from_who: row.get("from_who")?,
        to_who: row.get("to_who")?,
        body: row.get("body")?,
        urgent: row.get::<_, i64>("urgent")? != 0,
        delivered: row.get::<_, i64>("delivered")? != 0,
        created_at: row.get("created_at")?,
        delivered_at: row.get("delivered_at")?,
    })
}

impl Store {
    /// Insert a message; `recipients` is the already-expanded list (the hub
    /// expands "all" to every agent except the sender). Returns row ids.
    pub fn msg_send(
        &self,
        from: &str,
        recipients: &[String],
        body: &str,
        urgent: bool,
    ) -> Result<Vec<i64>> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;
        let now = now_ts();
        let mut ids = Vec::with_capacity(recipients.len());
        for to in recipients {
            tx.execute(
                "INSERT INTO messages (from_who, to_who, body, urgent, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![from, to, body, urgent as i64, now],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Pending (undelivered) messages for an agent, oldest first.
    #[cfg(test)]
    pub fn msg_pending(&self, to: &str) -> Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM messages WHERE to_who = ?1 AND delivered = 0 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([to], msg_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Atomically fetch-and-mark-delivered the pending inbox for an agent.
    pub fn msg_take_pending(&self, to: &str) -> Result<Vec<Message>> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;
        let msgs = {
            let mut stmt = tx.prepare(
                "SELECT * FROM messages WHERE to_who = ?1 AND delivered = 0 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([to], msg_from_row)?;
            rows.collect::<rusqlite::Result<Vec<Message>>>()?
        };
        let now = now_ts();
        for m in &msgs {
            tx.execute(
                "UPDATE messages SET delivered = 1, delivered_at = ?1 WHERE id = ?2",
                params![now, m.id],
            )?;
        }
        tx.commit()?;
        Ok(msgs)
    }

    /// Non-destructive per-session read of the shared `human` mailbox.
    ///
    /// Returns messages addressed to `human` that this session has not yet
    /// seen (id beyond its cursor) and advances the session's cursor past
    /// them. It never touches `messages.delivered`, so every other session
    /// still sees the same messages exactly once for itself. This is the fix
    /// for the multi-session cannibalization bug: agent inboxes stay
    /// destructive (`msg_take_pending`); human inboxes are per-session cursors.
    pub fn msg_read_for_session(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;
        let msgs = {
            let mut stmt = tx.prepare(
                "SELECT * FROM messages
                 WHERE to_who = 'human'
                   AND id > COALESCE(
                       (SELECT last_read_msg_id FROM message_cursors WHERE session_id = ?1), 0)
                 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([session_id], msg_from_row)?;
            rows.collect::<rusqlite::Result<Vec<Message>>>()?
        };
        if let Some(max_id) = msgs.iter().map(|m| m.id).max() {
            tx.execute(
                "INSERT INTO message_cursors (session_id, last_read_msg_id, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET
                     last_read_msg_id = excluded.last_read_msg_id,
                     updated_at = excluded.updated_at",
                params![session_id, max_id, now_ts()],
            )?;
        }
        tx.commit()?;
        Ok(msgs)
    }

    /// Recent traffic for the TUI message feed (delivered or not).
    pub fn msg_recent(&self, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM (SELECT * FROM messages ORDER BY id DESC LIMIT ?1) ORDER BY id",
        )?;
        let rows = stmt.query_map([limit as i64], msg_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn msg_pending_count(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE delivered = 0",
            [],
            |r| r.get(0),
        )?)
    }

    /// Offline browse: filter by from/to agent, return at most `limit` rows newest-first.
    pub fn msg_list(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let lim = limit as i64;
        // Each branch uses positional ?N that match the params! order.
        let rows: Vec<Message> = match (from, to) {
            (None, None) => {
                let mut s = conn.prepare(
                    "SELECT * FROM (SELECT * FROM messages ORDER BY id DESC LIMIT ?1) ORDER BY id",
                )?;
                let v = s.query_map([lim], msg_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            }
            (Some(f), None) => {
                let mut s = conn.prepare(
                    "SELECT * FROM (SELECT * FROM messages WHERE from_who = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id",
                )?;
                let v = s.query_map(rusqlite::params![f, lim], msg_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            }
            (None, Some(t)) => {
                let mut s = conn.prepare(
                    "SELECT * FROM (SELECT * FROM messages WHERE to_who = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id",
                )?;
                let v = s.query_map(rusqlite::params![t, lim], msg_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            }
            (Some(f), Some(t)) => {
                let mut s = conn.prepare(
                    "SELECT * FROM (SELECT * FROM messages WHERE from_who = ?1 AND to_who = ?2 ORDER BY id DESC LIMIT ?3) ORDER BY id",
                )?;
                let v = s.query_map(rusqlite::params![f, t, lim], msg_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            }
        };
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_list_filters() {
        let s = Store::open_in_memory().unwrap();
        s.msg_send("alice", &["bob".into()], "hi bob", false).unwrap();
        s.msg_send("bob", &["alice".into()], "hi alice", false).unwrap();
        s.msg_send("alice", &["carol".into()], "hi carol", false).unwrap();

        assert_eq!(s.msg_list(None, None, 100).unwrap().len(), 3);
        assert_eq!(s.msg_list(Some("alice"), None, 100).unwrap().len(), 2);
        assert_eq!(s.msg_list(None, Some("bob"), 100).unwrap().len(), 1);
        assert_eq!(s.msg_list(Some("alice"), Some("bob"), 100).unwrap().len(), 1);
        assert_eq!(s.msg_list(Some("nobody"), None, 100).unwrap().len(), 0);
        // limit works
        assert_eq!(s.msg_list(None, None, 2).unwrap().len(), 2);
    }

    #[test]
    fn take_pending_marks_delivered() {
        let s = Store::open_in_memory().unwrap();
        s.msg_send("alice", &["bob".into()], "hi bob", false)
            .unwrap();
        s.msg_send("alice", &["bob".into()], "again", true).unwrap();
        s.msg_send("alice", &["carol".into()], "hi carol", false)
            .unwrap();

        let taken = s.msg_take_pending("bob").unwrap();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].body, "hi bob");
        assert!(taken[1].urgent);

        assert!(s.msg_take_pending("bob").unwrap().is_empty());
        assert_eq!(s.msg_pending("carol").unwrap().len(), 1);
        assert_eq!(s.msg_pending_count().unwrap(), 1);
    }

    #[test]
    fn two_sessions_each_receive_human_broadcast() {
        let s = Store::open_in_memory().unwrap();
        s.session_register("human:s1", "cli", None).unwrap();
        s.session_register("human:s2", "cli", None).unwrap();
        s.msg_send("composer", &["human".into()], "fleet update", false)
            .unwrap();

        // Both sessions see the same message — no cannibalization.
        assert_eq!(s.msg_read_for_session("human:s1").unwrap().len(), 1);
        assert_eq!(s.msg_read_for_session("human:s2").unwrap().len(), 1);
        // Re-reading the same session returns nothing (cursor advanced).
        assert!(s.msg_read_for_session("human:s1").unwrap().is_empty());
    }

    #[test]
    fn fresh_session_starts_at_head() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..3 {
            s.msg_send("composer", &["human".into()], &format!("m{i}"), false)
                .unwrap();
        }
        // A session that connects AFTER the backlog must not replay it.
        s.session_register("human:late", "cli", None).unwrap();
        assert!(s.msg_read_for_session("human:late").unwrap().is_empty());

        // ...but it does see messages that arrive after it connected.
        s.msg_send("composer", &["human".into()], "after", false)
            .unwrap();
        assert_eq!(s.msg_read_for_session("human:late").unwrap().len(), 1);
    }

    #[test]
    fn agent_inbox_still_destructive() {
        // The human cursor fix must NOT change agent inbox semantics.
        let s = Store::open_in_memory().unwrap();
        s.msg_send("composer", &["builder".into()], "do it", false)
            .unwrap();
        assert_eq!(s.msg_take_pending("builder").unwrap().len(), 1);
        assert!(s.msg_take_pending("builder").unwrap().is_empty());
    }
}
