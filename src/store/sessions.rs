//! Session registry + per-session inbox cursors.
//!
//! A "session" is a connected human/CLI/TUI peer (agents are NOT sessions).
//! Each session is individually identified (e.g. `human:<uuid>`) so multiple
//! concurrent sessions can read the shared `human` mailbox without consuming
//! each other's messages — delivery is tracked per session via a read cursor
//! in `message_cursors` (see `messages::msg_read_for_session`).

use super::{now_ts, Store};
use anyhow::Result;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
    pub connected_at: i64,
    pub last_seen: i64,
    pub disconnected_at: Option<i64>,
}

fn session_from_row(row: &Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get("id")?,
        kind: row.get("kind")?,
        label: row.get("label")?,
        connected_at: row.get("connected_at")?,
        last_seen: row.get("last_seen")?,
        disconnected_at: row.get("disconnected_at")?,
    })
}

impl Store {
    /// Register (or re-activate) a session. Idempotent: re-registering an
    /// existing id refreshes `last_seen` and clears `disconnected_at` while
    /// preserving the original `connected_at`. The session's inbox cursor is
    /// initialized to the current head of the messages table so a fresh
    /// session does NOT replay the entire historical backlog as "unread".
    pub fn session_register(&self, id: &str, kind: &str, label: Option<&str>) -> Result<()> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;
        let now = now_ts();
        tx.execute(
            "INSERT INTO sessions (id, kind, label, connected_at, last_seen, disconnected_at)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL)
             ON CONFLICT(id) DO UPDATE SET
                 kind = excluded.kind,
                 label = COALESCE(excluded.label, sessions.label),
                 last_seen = excluded.last_seen,
                 disconnected_at = NULL",
            params![id, kind, label, now],
        )?;
        // Start the cursor at the current head so only NEW messages are unread.
        tx.execute(
            "INSERT OR IGNORE INTO message_cursors (session_id, last_read_msg_id, updated_at)
             VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM messages), ?2)",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Bump `last_seen` for a live session (called on every request).
    pub fn session_touch(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET last_seen = ?2, disconnected_at = NULL WHERE id = ?1",
            params![id, now_ts()],
        )?;
        Ok(())
    }

    /// Mark a session disconnected (its cursor is retained for reconnects).
    pub fn session_disconnect(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET disconnected_at = ?2 WHERE id = ?1",
            params![id, now_ts()],
        )?;
        Ok(())
    }

    /// Sessions still connected and seen within `stale_secs` of `now`.
    pub fn session_active(&self, now: i64, stale_secs: i64) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM sessions
             WHERE disconnected_at IS NULL AND last_seen > ?1
             ORDER BY connected_at",
        )?;
        let cutoff = now - stale_secs;
        let rows = stmt.query_map([cutoff], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_touch_disconnect_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        s.session_register("human:1", "tui", Some("term-a")).unwrap();
        let now = now_ts();
        let active = s.session_active(now, 3600).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "human:1");
        assert_eq!(active[0].kind, "tui");
        assert_eq!(active[0].label.as_deref(), Some("term-a"));

        s.session_touch("human:1").unwrap();
        assert_eq!(s.session_active(now, 3600).unwrap().len(), 1);

        s.session_disconnect("human:1").unwrap();
        assert!(s.session_active(now, 3600).unwrap().is_empty());
    }

    #[test]
    fn register_is_idempotent_and_preserves_connected_at() {
        let s = Store::open_in_memory().unwrap();
        s.session_register("human:1", "cli", None).unwrap();
        let first = s.session_active(now_ts(), 3600).unwrap()[0].connected_at;
        s.session_register("human:1", "cli", None).unwrap();
        let active = s.session_active(now_ts(), 3600).unwrap();
        assert_eq!(active.len(), 1, "re-register must not create a duplicate");
        assert_eq!(active[0].connected_at, first);
    }
}
