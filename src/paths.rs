//! Data-directory resolution.
//!
//! All mutable state (SQLite DB, logs, hub.json) lives under
//! `%LOCALAPPDATA%\agentcom\<project-id>\` — never inside the project root,
//! which may be synced by OneDrive (sync locks on SQLite WAL files cause
//! corruption). Only `agentcom.toml` lives in the project.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "agentcom.toml";
pub const HUB_FILE: &str = "hub.json";

/// Stable short identifier derived from the canonical project root path.
pub fn project_id(project_root: &Path) -> String {
    let canon = dunce_canonicalize(project_root);
    let mut hasher = Sha256::new();
    hasher.update(canon.to_string_lossy().to_lowercase().as_bytes());
    let digest = hasher.finalize();
    hex(&digest[..8])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Canonicalize without the `\\?\` prefix noise where possible.
fn dunce_canonicalize(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Per-project data directory; created on first use.
pub fn data_dir(project_root: &Path) -> Result<PathBuf> {
    let base = directories::ProjectDirs::from("", "", "agentcom")
        .context("could not determine local data directory")?
        .data_local_dir()
        .to_path_buf();
    let dir = base.join(project_id(project_root));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating data dir {}", dir.display()))?;
    Ok(dir)
}

pub fn db_path(project_root: &Path) -> Result<PathBuf> {
    Ok(data_dir(project_root)?.join("agentcom.db"))
}

pub fn hub_json_path(project_root: &Path) -> Result<PathBuf> {
    Ok(data_dir(project_root)?.join(HUB_FILE))
}

pub fn log_dir(project_root: &Path) -> Result<PathBuf> {
    let dir = data_dir(project_root)?.join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Stable location for the hub binary + adapters, decoupled from any project's
/// `target/` dir. The hub copies itself here and relaunches when started from
/// build output, so rebuilds can't clobber the running executables.
pub fn bin_dir() -> Result<PathBuf> {
    let base = directories::ProjectDirs::from("", "", "agentcom")
        .context("could not determine local data directory")?
        .data_local_dir()
        .to_path_buf();
    let dir = base.join("bin");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating bin dir {}", dir.display()))?;
    Ok(dir)
}

/// Walk up from `start` looking for `agentcom.toml`; returns the directory
/// containing it. Used by client-mode commands so they work from any
/// subdirectory of the project (including agent cwds).
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(dunce_canonicalize(start));
    while let Some(dir) = cur {
        if dir.join(CONFIG_FILE).is_file() {
            return Some(dir);
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_stable_and_short() {
        let a = project_id(Path::new("C:\\some\\proj"));
        let b = project_id(Path::new("C:\\some\\proj"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn find_root_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(CONFIG_FILE), "").unwrap();
        let found = find_project_root(&nested).unwrap();
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
    }
}
