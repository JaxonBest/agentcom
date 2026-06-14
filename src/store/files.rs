//! Advisory file claims — the mechanism that keeps agents from editing the
//! same files at once. An agent claims paths before touching them; claiming
//! a path someone else holds is rejected with the holder's name, and the
//! composer/teammates coordinate from there. Claims are advisory (nothing
//! blocks the filesystem) but agents are instructed to honor them, and
//! crashes release them automatically.

use super::{now_ts, Store};
use anyhow::{bail, Result};
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

/// Reject paths that could escape the project root or reference sensitive locations.
/// Only relative paths without traversal are allowed.
fn validate_claim_path(path: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("file claim path must not be empty");
    }
    // Reject absolute paths on any platform.
    if trimmed.starts_with('/') || trimmed.starts_with('~') {
        bail!("file claim path must be relative, not absolute: {path:?}");
    }
    // Reject Windows absolute paths (C:\...) and UNC paths (\\...).
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        bail!("file claim path must be relative, not absolute: {path:?}");
    }
    if trimmed.starts_with("\\\\") {
        bail!("file claim path must be relative, not absolute: {path:?}");
    }
    // Reject directory traversal segments.
    let normalized = trimmed.replace('\\', "/");
    if normalized.split('/').any(|seg| seg == "..") {
        bail!("file claim path must not contain '..': {path:?}");
    }
    Ok(())
}

impl Store {
    /// Validate paths before claiming — rejects traversal (`..`), absolute paths,
    /// home-dir paths (`~`), and empty strings. Call this before `files_claim` to
    /// surface a clear error message; hub handler updated separately.
    pub fn validate_claim_paths(paths: &[String]) -> Result<()> {
        for path in paths {
            validate_claim_path(path)?;
        }
        Ok(())
    }

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

    /// Clear all file claims from all agents — called at session start.
    pub fn files_release_all_agents(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM file_claims", [])?)
    }

    pub fn files_list(&self) -> Result<Vec<FileClaim>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT path, agent, claimed_at FROM file_claims ORDER BY agent, path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FileClaim {
                path: r.get(0)?,
                agent: r.get(1)?,
                claimed_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// List all file claims held by a specific agent.
    pub fn files_list_for_agent(&self, agent: &str) -> Result<Vec<FileClaim>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT path, agent, claimed_at FROM file_claims WHERE agent = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map([agent], |r| {
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
        assert_eq!(
            s.files_release("alice", &["x.js".into()], false).unwrap(),
            1
        );
    }

    #[test]
    fn validate_claim_paths_rejects_traversal() {
        assert!(Store::validate_claim_paths(&["../secret".into()]).is_err());
        assert!(Store::validate_claim_paths(&["src/../../etc/passwd".into()]).is_err());
        assert!(Store::validate_claim_paths(&["a/b/../../../c".into()]).is_err());
    }

    #[test]
    fn validate_claim_paths_rejects_absolute() {
        assert!(Store::validate_claim_paths(&["/etc/passwd".into()]).is_err());
        assert!(Store::validate_claim_paths(&["~/secret".into()]).is_err());
        assert!(Store::validate_claim_paths(&["C:\\Windows\\System32".into()]).is_err());
    }

    #[test]
    fn validate_claim_paths_accepts_relative() {
        assert!(Store::validate_claim_paths(&["src/main.rs".into()]).is_ok());
        assert!(Store::validate_claim_paths(&["tests/e2e.rs".into()]).is_ok());
        assert!(Store::validate_claim_paths(&["Cargo.toml".into()]).is_ok());
    }

    #[test]
    fn list_for_agent_returns_only_own_claims() {
        let s = Store::open_in_memory().unwrap();
        s.files_claim("alice", &["src/a.rs".into(), "src/b.rs".into()])
            .unwrap();
        s.files_claim("bob", &["src/c.rs".into()]).unwrap();

        let alice_claims = s.files_list_for_agent("alice").unwrap();
        assert_eq!(alice_claims.len(), 2);
        assert!(alice_claims.iter().all(|c| c.agent == "alice"));
        let paths: Vec<&str> = alice_claims.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"src/a.rs"));
        assert!(paths.contains(&"src/b.rs"));

        let bob_claims = s.files_list_for_agent("bob").unwrap();
        assert_eq!(bob_claims.len(), 1);
        assert_eq!(bob_claims[0].path, "src/c.rs");

        // agent with no claims returns empty
        let empty = s.files_list_for_agent("carol").unwrap();
        assert!(empty.is_empty());
    }
}
