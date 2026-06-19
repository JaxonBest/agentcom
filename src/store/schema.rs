use anyhow::Result;
use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            title           TEXT NOT NULL,
            description     TEXT NOT NULL DEFAULT '',
            status          TEXT NOT NULL DEFAULT 'open',
            priority        INTEGER NOT NULL DEFAULT 2,
            claimed_by      TEXT,
            blocked_reason  TEXT,
            note            TEXT,
            tags            TEXT NOT NULL DEFAULT '',
            created_by      TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS task_deps (
            task_id        INTEGER NOT NULL REFERENCES tasks(id),
            depends_on_id  INTEGER NOT NULL REFERENCES tasks(id),
            PRIMARY KEY (task_id, depends_on_id)
        );

        CREATE TABLE IF NOT EXISTS messages (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            from_who      TEXT NOT NULL,
            to_who        TEXT NOT NULL,
            body          TEXT NOT NULL,
            urgent        INTEGER NOT NULL DEFAULT 0,
            delivered     INTEGER NOT NULL DEFAULT 0,
            created_at    INTEGER NOT NULL,
            delivered_at  INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_messages_inbox
            ON messages (to_who, delivered, created_at);

        CREATE TABLE IF NOT EXISTS file_claims (
            path        TEXT PRIMARY KEY,
            agent       TEXT NOT NULL,
            claimed_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            agent       TEXT NOT NULL,
            session_id  TEXT NOT NULL,
            started_at  INTEGER NOT NULL,
            ended_at    INTEGER,
            cost_usd    REAL NOT NULL DEFAULT 0,
            turns       INTEGER NOT NULL DEFAULT 0,
            end_reason  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_runs_agent ON runs (agent, started_at);
        -- The CLI re-emits system/init on every fed turn; one runs row per
        -- (agent, session) regardless.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_unique ON runs (agent, session_id);

        CREATE TABLE IF NOT EXISTS task_activity (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id     INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            agent       TEXT NOT NULL,
            body        TEXT NOT NULL,
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_task_activity_task ON task_activity (task_id, created_at);

        -- Connected human/CLI/TUI sessions. Each is an individually-identified
        -- IPC peer (e.g. 'human:9f3c...') so multiple sessions can coexist
        -- without cannibalizing each other's inbox. Agents are NOT sessions.
        CREATE TABLE IF NOT EXISTS sessions (
            id              TEXT PRIMARY KEY,
            kind            TEXT NOT NULL DEFAULT 'human',
            label           TEXT,
            connected_at    INTEGER NOT NULL,
            last_seen       INTEGER NOT NULL,
            disconnected_at INTEGER
        );

        -- Per-session read cursor over the shared 'human' mailbox. Lets each
        -- session see every message addressed to 'human' exactly once for
        -- itself, without marking the row delivered for other sessions.
        CREATE TABLE IF NOT EXISTS message_cursors (
            session_id       TEXT PRIMARY KEY REFERENCES sessions(id),
            last_read_msg_id INTEGER NOT NULL DEFAULT 0,
            updated_at       INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_human ON messages (to_who, id);

        -- Append-only, durable transcript of the conversation + fleet activity.
        -- `seq` (AUTOINCREMENT) is the monotonic ordering/pagination key; `ts`
        -- is seconds-granularity and only for display/retention. This is the
        -- source of truth the chat TUI backfills from (survives hub restart);
        -- the in-memory ring buffers remain only a live render scratchpad.
        CREATE TABLE IF NOT EXISTS transcript (
            seq         INTEGER PRIMARY KEY AUTOINCREMENT,
            ts          INTEGER NOT NULL,
            kind        TEXT NOT NULL,
            actor       TEXT NOT NULL,
            body        TEXT NOT NULL DEFAULT '',
            task_id     INTEGER,
            session_id  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_transcript_actor ON transcript (actor, seq);
        CREATE INDEX IF NOT EXISTS idx_transcript_kind  ON transcript (kind, seq);
        "#,
    )?;
    // Idempotent column additions for databases created before these columns existed.
    for (col, ddl) in [
        ("tags", "ALTER TABLE tasks ADD COLUMN tags TEXT NOT NULL DEFAULT ''"),
        ("pinned", "ALTER TABLE tasks ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0"),
        ("due_at", "ALTER TABLE tasks ADD COLUMN due_at INTEGER"),
        ("timeout_mins", "ALTER TABLE tasks ADD COLUMN timeout_mins INTEGER"),
        ("requires", "ALTER TABLE tasks ADD COLUMN requires TEXT NOT NULL DEFAULT ''"),
        ("is_archived", "ALTER TABLE tasks ADD COLUMN is_archived INTEGER NOT NULL DEFAULT 0"),
        ("recur", "ALTER TABLE tasks ADD COLUMN recur TEXT"),
        ("next_run_at", "ALTER TABLE tasks ADD COLUMN next_run_at INTEGER"),
        ("hook_attempts", "ALTER TABLE tasks ADD COLUMN hook_attempts INTEGER NOT NULL DEFAULT 0"),
        ("total_cost_usd", "ALTER TABLE tasks ADD COLUMN total_cost_usd REAL NOT NULL DEFAULT 0"),
        ("claimed_at", "ALTER TABLE tasks ADD COLUMN claimed_at INTEGER"),
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tasks') WHERE name=?1",
                [col],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if !exists {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}
