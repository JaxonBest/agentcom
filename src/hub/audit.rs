//! Append-only security audit log written to `.agentcom/audit.log`.
//!
//! Each event is one JSON line:
//! `{"ts":1234567890,"event":"task_claim","agent":"builder","task_id":42}`
//!
//! The log is separate from hub.log and is never truncated. Wire it up in
//! hub/mod.rs by calling `self.audit.write(...)` at the key event points.

use std::io::Write;
use std::path::{Path, PathBuf};

pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("audit.log"),
        }
    }

    /// Rotate the log when it exceeds 10 MiB. Renames `audit.log` →
    /// `audit.log.1`, overwriting any previous backup. Silently ignores
    /// rotation errors so audit failures never crash the hub.
    fn rotate_if_needed(&self) {
        const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return; // log doesn't exist yet
        };
        if meta.len() < MAX_LOG_BYTES {
            return;
        }
        let backup = self.path.with_extension("log.1");
        let _ = std::fs::rename(&self.path, &backup);
    }

    /// Append one JSON event line. Silently ignores I/O errors — audit
    /// failures must never crash the hub.
    pub fn write(&self, event: &str, agent: &str, extra: serde_json::Value) {
        self.rotate_if_needed();
        let mut opts = std::fs::OpenOptions::new();
        opts.append(true).create(true);
        // Restrict new files to owner-only so audit events stay private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let Ok(mut f) = opts.open(&self.path) else {
            return;
        };
        let ts = crate::store::now_ts();
        let mut record =
            serde_json::json!({"ts": ts, "event": event, "agent": agent});
        if let (Some(obj), serde_json::Value::Object(extra_map)) =
            (record.as_object_mut(), extra)
        {
            obj.extend(extra_map);
        }
        if let Ok(line) = serde_json::to_string(&record) {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn writes_json_lines() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::new(dir.path());

        log.write("task_claim", "builder", serde_json::json!({"task_id": 42}));
        log.write(
            "file_claim",
            "builder",
            serde_json::json!({"paths": ["src/main.rs"]}),
        );

        let content = fs::read_to_string(dir.path().join("audit.log")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["event"], "task_claim");
        assert_eq!(first["agent"], "builder");
        assert_eq!(first["task_id"], 42);

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["event"], "file_claim");
        assert_eq!(second["paths"][0], "src/main.rs");
    }

    #[test]
    fn write_to_unwritable_dir_does_not_panic() {
        let log = AuditLog::new(Path::new("/nonexistent/path"));
        // Must not panic — audit errors are silently swallowed.
        log.write("agent_spawn", "builder", serde_json::json!({}));
    }

    #[test]
    fn audit_log_rotates_at_10mb() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        // Pre-create a file just over 10 MiB to trigger rotation.
        let big = vec![b'x'; 10 * 1024 * 1024 + 1];
        fs::write(&log_path, &big).unwrap();

        let log = AuditLog::new(dir.path());
        log.write("task_claim", "builder", serde_json::json!({"task_id": 99}));

        // Old oversized log should now be the backup.
        let backup = dir.path().join("audit.log.1");
        assert!(backup.exists(), "backup audit.log.1 must exist after rotation");
        assert_eq!(
            fs::metadata(&backup).unwrap().len(),
            big.len() as u64,
            "backup should contain the original oversized content"
        );

        // Fresh log should be small (just the one new event).
        let new_len = fs::metadata(&log_path).unwrap().len();
        assert!(
            new_len < 1024,
            "rotated audit.log should be small, got {new_len} bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_log_created_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let log = AuditLog::new(dir.path());
        log.write("task_claim", "builder", serde_json::json!({"task_id": 1}));
        let mode = fs::metadata(dir.path().join("audit.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "audit.log must be owner-read/write only");
    }
}
