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
        "#,
    )?;
    Ok(())
}
