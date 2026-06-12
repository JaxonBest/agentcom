//! Advisory file claims — the mechanism that keeps agents from editing the
//! same files at once. An agent claims paths before touching them; claiming
//! a path someone else holds is rejected with the holder's name, and the
//! composer/teammates coordinate from there. Claims are advisory (nothing
//! blocks the filesystem) but agents are instructed to honor them, and
//! crashes release them automatically.

use super::{now_ts, Store};
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileClaim {
    pub path: String,
    pub agent: String,
    pub claimed_at: i64,
}

/// Normalize for comparison: forward slashes, lowercase (Windows paths are
/// case-insensitive), no leading "./".
fn normalize(path: &str) -> String {
    let p = path.trim().replace('\\', "/").to_lowercase();
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

impl Store {
    /// Claim paths for an agent. All-or-nothing: if any path is held by a
    /// different agent, nothing is claimed and the conflicts are returned
    /// as `Err`. Re-claiming your own paths is a no-op.
    pub fn files_claim(&self, agent: &str, paths: &[String]) -> Result<(), Vec<FileClaim>> {
        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction().map_err(|_| Vec::new())?;
        let mut conflicts = Vec::new();
        for path in paths {
            let norm = normalize(path);
            let existing: Option<(String, i64)> = tx
                .query_row(
                    "SELECT agent, claimed_at FROM file_claims WHERE path = ?1",
                    [&norm],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            if let Some((holder, at)) = existing {
                if holder != agent {
                    conflicts.push(FileClaim {
                        path: norm.clone(),
                        agent: holder,
                        claimed_at: at,
                    });
                }
            }
        }
        if !conflicts.is_empty() {
            return Err(conflicts);
        }
        let now = now_ts();
        for path in paths {
            let _ = tx.execute(
                "INSERT OR REPLACE INTO file_claims (path, agent, claimed_at) VALUES (?1, ?2, ?3)",
                params![normalize(path), agent, now],
            );
        }
        tx.commit().map_err(|_| Vec::new())?;
        Ok(())
    }

    /// Release specific paths (only your own) or all of an agent's claims.
    pub fn files_release(&self, agent: &str, paths: &[String], all: bool) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        if all {
            return Ok(conn.execute("DELETE FROM file_claims WHERE agent = ?1", [agent])?);
        }
        let mut released = 0;
        for path in paths {
            released += conn.execute(
                "DELETE FROM file_claims WHERE path = ?1 AND agent = ?2",
                params![normalize(path), agent],
            )?;
        }
        Ok(released)
    }

    pub fn files_list(&self) -> Result<Vec<FileClaim>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT path, agent, claimed_at FROM file_claims ORDER BY agent, path")?;
        let rows = stmt.query_map([], |r| {
            Ok(FileClaim {
                path: r.get(0)?,
                agent: r.get(1)?,
                claimed_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicting_claim_rejected_atomically() {
        let s = Store::open_in_memory().unwrap();
        s.files_claim("alice", &["src/a.rs".into(), "src/b.rs".into()])
            .unwrap();

        // bob wants b.rs (held) and c.rs (free) — nothing gets claimed.
        let err = s
            .files_claim("bob", &["src/c.rs".into(), "src\\B.RS".into()])
            .unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].agent, "alice");
        assert_eq!(err[0].path, "src/b.rs");
        let claims = s.files_list().unwrap();
        assert!(!claims.iter().any(|c| c.agent == "bob"));

        // re-claiming your own path is fine
        s.files_claim("alice", &["./src/a.rs".into()]).unwrap();

        // release frees it for bob
        s.files_release("alice", &[], true).unwrap();
        s.files_claim("bob", &["src/b.rs".into()]).unwrap();
    }

    #[test]
    fn release_specific_paths_only_own() {
        let s = Store::open_in_memory().unwrap();
        s.files_claim("alice", &["x.js".into()]).unwrap();
        assert_eq!(s.files_release("bob", &["x.js".into()], false).unwrap(), 0);
        assert_eq!(s.files_release("alice", &["x.js".into()], false).unwrap(), 1);
    }
}
