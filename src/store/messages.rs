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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
