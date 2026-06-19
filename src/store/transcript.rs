//! Durable, append-only transcript of the conversation + fleet activity.
//!
//! `seq` (AUTOINCREMENT) is the monotonic ordering and pagination key; `ts` is
//! seconds-granularity and only for display/retention (it cannot order the
//! many events the hub emits within one second). The chat TUI backfills
//! scrollback from this table so history survives a hub restart — unlike the
//! in-memory ring buffers, which remain only a live render scratchpad.

use super::{now_ts, Store};
use anyhow::Result;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

/// The kind of a transcript event. Stored as TEXT (mirrors TaskStatus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// A message the human typed to the composer.
    HumanMsg,
    /// A reply from the composer (or any agent) addressed to the human.
    ComposerMsg,
    /// A sealed line of agent output (assistant text, tool use, turn end).
    AgentLine,
    /// An agent state transition (working/idle/paused/...).
    AgentState,
    /// A task lifecycle event (added/claimed/done/blocked/reviewed).
    TaskEvent,
    /// A hub-level log line.
    HubLog,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::HumanMsg => "human_msg",
            EventKind::ComposerMsg => "composer_msg",
            EventKind::AgentLine => "agent_line",
            EventKind::AgentState => "agent_state",
            EventKind::TaskEvent => "task_event",
            EventKind::HubLog => "hub_log",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        Some(match s {
            "human_msg" => EventKind::HumanMsg,
            "composer_msg" => EventKind::ComposerMsg,
            "agent_line" => EventKind::AgentLine,
            "agent_state" => EventKind::AgentState,
            "task_event" => EventKind::TaskEvent,
            "hub_log" => EventKind::HubLog,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub seq: i64,
    pub ts: i64,
    pub kind: EventKind,
    pub actor: String,
    pub body: String,
    pub task_id: Option<i64>,
    pub session_id: Option<String>,
}

fn transcript_from_row(row: &Row) -> rusqlite::Result<TranscriptEvent> {
    let kind_str: String = row.get("kind")?;
    Ok(TranscriptEvent {
        seq: row.get("seq")?,
        ts: row.get("ts")?,
        // Unknown kinds are tolerated as HubLog rather than failing the query,
        // so a forward-compatible writer can never break a reader.
        kind: EventKind::parse(&kind_str).unwrap_or(EventKind::HubLog),
        actor: row.get("actor")?,
        body: row.get("body")?,
        task_id: row.get("task_id")?,
        session_id: row.get("session_id")?,
    })
}

impl Store {
    /// Append an event and return its monotonic `seq`.
    pub fn transcript_append(
        &self,
        kind: EventKind,
        actor: &str,
        body: &str,
        task_id: Option<i64>,
        session_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcript (ts, kind, actor, body, task_id, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now_ts(), kind.as_str(), actor, body, task_id, session_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Last `n` events, oldest-first (ready to render top-to-bottom).
    pub fn transcript_tail(&self, n: usize) -> Result<Vec<TranscriptEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM (SELECT * FROM transcript ORDER BY seq DESC LIMIT ?1) ORDER BY seq",
        )?;
        let rows = stmt.query_map([n as i64], transcript_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Last `n` events from one actor, oldest-first (per-agent output view).
    pub fn transcript_actor_tail(&self, actor: &str, n: usize) -> Result<Vec<TranscriptEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM (SELECT * FROM transcript WHERE actor = ?1 ORDER BY seq DESC LIMIT ?2) ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![actor, n as i64], transcript_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Delete events older than `before_ts`. Returns the number of rows removed.
    pub fn transcript_prune(&self, before_ts: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM transcript WHERE ts < ?1", [before_ts])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(s: &Store, kind: EventKind, actor: &str, body: &str) -> i64 {
        s.transcript_append(kind, actor, body, None, None).unwrap()
    }

    #[test]
    fn append_returns_monotonic_seq() {
        let s = Store::open_in_memory().unwrap();
        let a = append(&s, EventKind::HubLog, "hub", "one");
        let b = append(&s, EventKind::HubLog, "hub", "two");
        let c = append(&s, EventKind::HubLog, "hub", "three");
        assert!(a < b && b < c, "seq must be strictly increasing");
    }

    #[test]
    fn transcript_tail_oldest_first() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..5 {
            append(&s, EventKind::HubLog, "hub", &format!("e{i}"));
        }
        let tail = s.transcript_tail(3).unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].body, "e2");
        assert_eq!(tail[2].body, "e4");
    }

    #[test]
    fn transcript_actor_tail_filters() {
        let s = Store::open_in_memory().unwrap();
        append(&s, EventKind::AgentLine, "a", "a1");
        append(&s, EventKind::AgentLine, "b", "b1");
        append(&s, EventKind::AgentLine, "a", "a2");
        let a_only = s.transcript_actor_tail("a", 10).unwrap();
        assert_eq!(a_only.len(), 2);
        assert!(a_only.iter().all(|e| e.actor == "a"));
        assert_eq!(a_only[0].body, "a1");
        assert_eq!(a_only[1].body, "a2");
    }

    #[test]
    fn kind_roundtrip() {
        for k in [
            EventKind::HumanMsg,
            EventKind::ComposerMsg,
            EventKind::AgentLine,
            EventKind::AgentState,
            EventKind::TaskEvent,
            EventKind::HubLog,
        ] {
            assert_eq!(EventKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(EventKind::parse("bogus"), None);
    }

    #[test]
    fn transcript_prune_removes_old() {
        let s = Store::open_in_memory().unwrap();
        append(&s, EventKind::HubLog, "hub", "old");
        append(&s, EventKind::HubLog, "hub", "new");
        {
            let conn = s.conn.lock().unwrap();
            conn.execute("UPDATE transcript SET ts = 100 WHERE body = 'old'", [])
                .unwrap();
            conn.execute("UPDATE transcript SET ts = 200 WHERE body = 'new'", [])
                .unwrap();
        }
        let removed = s.transcript_prune(150).unwrap();
        assert_eq!(removed, 1);
        let remaining = s.transcript_tail(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].body, "new");
    }
}
