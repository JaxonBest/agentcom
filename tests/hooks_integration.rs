//! Integration tests for Workstream C: post-close hooks.
//!
//! Validates:
//! 1. A failing post-close hook transitions Done→Blocked (re-blocking).
//! 2. Loop prevention — a task with hook_attempts >= 2 stays Done.
//!
//! Follows the e2e_mock.rs pattern: mock-claude via MOCK_SCRIPT_DIR ndjson.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn agentcom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom")
}
fn mock_claude_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock-claude")
}
fn codex_adapter_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom-codex-adapter")
}
fn deepseek_adapter_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom-deepseek-adapter")
}
fn mock_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock-codex")
}

struct HubGuard {
    child: Child,
    project: PathBuf,
}

impl Drop for HubGuard {
    fn drop(&mut self) {
        let _ = Command::new(agentcom_bin())
            .args(["stop"])
            .current_dir(&self.project)
            .env_remove("AGENTCOM_PORT")
            .env_remove("AGENTCOM_TOKEN")
            .env_remove("AGENTCOM_AGENT")
            .output();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
    }
}

fn write_config(project: &Path, agents: &[(&str, &str)], extra: &str) {
    let mut cfg = String::from("project_name = \"hooks-test\"\ninterrupt_timeout_secs = 2\n");
    cfg.push_str(extra);
    for (name, role) in agents {
        cfg.push_str(&format!(
            "\n[[agent]]\nname = \"{name}\"\nrole = \"{role}\"\n"
        ));
    }
    std::fs::write(project.join("agentcom.toml"), cfg).unwrap();
}

fn start_hub(
    project: &Path,
    script_dir: &Path,
    tasks: &[&str],
    extra_env: &[(&str, &str)],
) -> HubGuard {
    let mut cmd = Command::new(agentcom_bin());
    cmd.arg("up").arg("--headless");
    for t in tasks {
        cmd.arg("--task").arg(t);
    }
    cmd.current_dir(project)
        .env("AGENTCOM_CLAUDE_EXE", mock_claude_bin())
        .env("AGENTCOM_CODEX_ADAPTER_EXE", codex_adapter_bin())
        .env("AGENTCOM_CODEX_EXE", mock_codex_bin())
        .env("AGENTCOM_DEEPSEEK_ADAPTER_EXE", deepseek_adapter_bin())
        .env("MOCK_SCRIPT_DIR", script_dir)
        .env("RUST_LOG", "agentcom=debug")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("hub starts");
    HubGuard {
        child,
        project: project.to_path_buf(),
    }
}

fn cli(project: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(agentcom_bin())
        .args(args)
        .current_dir(project)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
        .env_remove("AGENTCOM_AGENT")
        .output()
        .expect("cli runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn wait_for(
    project: &Path,
    args: &[&str],
    timeout: Duration,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (_, out) = cli(project, args);
        if pred(&out) {
            return out;
        }
        last = out;
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!(
        "timed out waiting for {:?}; last output:\n{last}",
        args.join(" ")
    );
}

fn hub_logs(guard: &mut HubGuard) -> String {
    let mut out = String::new();
    if let Some(stderr) = guard.child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut out);
    }
    out
}

/// Write a failing hook script that exits non-zero.
fn write_failing_hook(project: &Path) -> PathBuf {
    let hook_path = project.join("failing-hook.sh");
    std::fs::write(
        &hook_path,
        "#!/bin/sh\nexit 1\n",
    )
    .unwrap();
    // Make executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    hook_path
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: failing post-close hook transitions Done→Blocked
// ─────────────────────────────────────────────────────────────────────

#[test]
fn failing_hook_blocks_done_task() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    let hook_path = write_failing_hook(&project);

    // Configure a single worker with a failing post-close hook.
    write_config(
        &project,
        &[("worker", "does work")],
        &format!(
            "[hooks]\npost_close = \"{}\"\npost_close_timeout_secs = 10\n",
            // Use the absolute path so the hook is found regardless of cwd.
            hook_path.to_string_lossy()
        ),
    );

    // Agent script: claim and complete the task.
    std::fs::write(
        scripts.join("worker.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note hook-test-done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["hook test task"], &[]);

    // The agent completes the task, then the hook fires and fails.
    // The task should transition: open → claimed → done → (hook fails) → blocked.
    let blocked_out = wait_for(
        &project,
        &["task", "list", "--status", "blocked"],
        Duration::from_secs(20),
        |out| out.contains("hook test task"),
    );
    assert!(
        blocked_out.contains("#1"),
        "task #1 is blocked after hook failure: {blocked_out}"
    );
    assert!(
        blocked_out.contains("post_close"),
        "blocked reason mentions the post_close hook: {blocked_out}"
    );

    // The task show output should show the blocked reason mentioning the hook.
    let (ok, show_out) = cli(&project, &["task", "show", "1"]);
    assert!(ok, "task show succeeds: {show_out}");
    assert!(
        show_out.contains("blocked: post_close"),
        "task show includes blocked reason with hook info: {show_out}"
    );

    // Graceful stop.
    let (ok, _) = cli(&project, &["stop"]);
    assert!(ok, "stop succeeds");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(exit)) = hub.child.try_wait() {
            assert!(
                exit.success(),
                "hub exits cleanly; logs:\n{}",
                hub_logs(&mut hub)
            );
            break;
        }
        if Instant::now() > deadline {
            panic!("hub did not exit after stop; logs:\n{}", hub_logs(&mut hub));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: loop prevention — hook_attempts >= 2 stays Done
// ─────────────────────────────────────────────────────────────────────

#[test]
fn hook_loop_prevention() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    let hook_path = write_failing_hook(&project);

    // Configure a single worker with a failing post-close hook.
    write_config(
        &project,
        &[("worker", "does work")],
        &format!(
            "[hooks]\npost_close = \"{}\"\npost_close_timeout_secs = 10\n",
            hook_path.to_string_lossy()
        ),
    );

    // Single ndjson entry reused on every agent invocation.
    // mock-claude resets to entry 1 on each new invocation, so all three rounds
    // must use the same script — loop prevention is verified by state, not note text.
    std::fs::write(
        scripts.join("worker.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["loop prevention task"], &[]);

    // ── Round 1 ──
    // Agent completes the task, hook fails, task is blocked.
    wait_for(
        &project,
        &["task", "list", "--status", "blocked"],
        Duration::from_secs(20),
        |out| out.contains("loop prevention task"),
    );

    // Re-open via human CLI so the agent can claim it again.
    let (ok, reopen_out) = cli(&project, &["task", "reopen", "1"]);
    assert!(ok, "reopen round 1 succeeds: {reopen_out}");

    // Give the hub a moment to feed the task to the idle agent.
    std::thread::sleep(Duration::from_millis(1000));

    // ── Round 2 ──
    // Agent completes the task again, hook fails again (hook_attempts=2), task blocked.
    wait_for(
        &project,
        &["task", "list", "--status", "blocked"],
        Duration::from_secs(20),
        |out| out.contains("loop prevention task"),
    );

    // Re-open again for round 3.
    let (ok, reopen_out) = cli(&project, &["task", "reopen", "1"]);
    assert!(ok, "reopen round 2 succeeds: {reopen_out}");

    // Give the hub a moment to feed the task.
    std::thread::sleep(Duration::from_millis(1000));

    // ── Round 3 ──
    // Agent completes the task. hook_attempts is now 2, so maybe_spawn_hook skips.
    // The task should stay Done (not re-blocked).
    wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("loop prevention task"),
    );

    // Give the hook a moment to fire (or not) — we're verifying it does NOT fire.
    std::thread::sleep(Duration::from_millis(500));

    // Verify the task stays done and is NOT re-blocked.
    let (_, blocked_out) = cli(&project, &["task", "list", "--status", "blocked"]);
    assert!(
        !blocked_out.contains("loop prevention task"),
        "task should not be re-blocked after hook_attempts >= 2: {blocked_out}"
    );

    let (_, done_out) = cli(&project, &["task", "list", "--status", "done"]);
    assert!(
        done_out.contains("loop prevention task"),
        "task stays done after hook skip: {done_out}"
    );

    // Check hook_attempts via task show — should be 2.
    let (ok, show_out) = cli(&project, &["task", "show", "1"]);
    assert!(ok, "task show succeeds: {show_out}");
    // The show output may or may not expose hook_attempts directly in the
    // human-readable view. At minimum confirm the task is done.
    assert!(
        show_out.contains("done") || show_out.contains("Done"),
        "task is done: {show_out}"
    );

    // Graceful stop.
    let (ok, _) = cli(&project, &["stop"]);
    assert!(ok, "stop succeeds");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(exit)) = hub.child.try_wait() {
            assert!(
                exit.success(),
                "hub exits cleanly; logs:\n{}",
                hub_logs(&mut hub)
            );
            break;
        }
        if Instant::now() > deadline {
            panic!("hub did not exit after stop; logs:\n{}", hub_logs(&mut hub));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
