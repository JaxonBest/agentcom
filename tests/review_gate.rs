//! Integration tests for Workstream A: review gate.
//!
//! Validates:
//! 1. An agent with default_review_required=true transitions to awaiting_review, not done.
//! 2. The hub auto-files a review task when a task enters awaiting_review.
//! 3. Reviewer approval transitions awaiting_review → done.
//! 4. Reviewer rejection reopens the task.
//! 5. The original closing agent cannot self-review.
//! 6. When default_review_required=false, task done goes straight to done.
//!
//! Follows the e2e_mock.rs & hooks_integration.rs pattern: mock-claude via
//! MOCK_SCRIPT_DIR ndjson, real hub, CLI interactions via `agentcom task <sub>`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn agentcom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom")
}
fn mock_claude_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock-claude")
}
fn mock_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock-codex")
}
fn codex_adapter_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom-codex-adapter")
}
fn deepseek_adapter_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentcom-deepseek-adapter")
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

fn write_config(project: &Path, agents: &[(&str, &str, bool, &[&str])], extra: &str) {
    let mut cfg = String::from(
        "project_name = \"review-test\"\ninterrupt_timeout_secs = 2\nmax_agents = 5\n",
    );
    cfg.push_str(extra);
    for (name, role, review_required, caps) in agents {
        cfg.push_str(&format!(
            "\n[[agent]]\nname = \"{name}\"\nrole = \"{role}\"\nprovider = \"claude\"\n"
        ));
        if *review_required {
            cfg.push_str("default_review_required = true\n");
        }
        if !caps.is_empty() {
            let caps_str = caps
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            cfg.push_str(&format!("capabilities = [{caps_str}]\n"));
        }
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
    // Small delay to let the hub start
    std::thread::sleep(Duration::from_millis(500));
    HubGuard {
        child,
        project: project.to_path_buf(),
    }
}

/// Wait for the hub to be ready (status command succeeds).
fn wait_for_hub(project: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let out = Command::new(agentcom_bin())
            .args(["status"])
            .current_dir(project)
            .output();
        if let Ok(out) = out {
            if out.status.success() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let out = Command::new(agentcom_bin())
        .args(["status"])
        .current_dir(project)
        .output()
        .unwrap();
    panic!(
        "hub did not become ready in {:?}. status stdout: {} status stderr: {}",
        timeout,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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

fn cli_as(project: &Path, agent: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(agentcom_bin())
        .args(args)
        .current_dir(project)
        .env("AGENTCOM_AGENT", agent)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
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
        use std::io::Read;
        let _ = stderr.read_to_string(&mut out);
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: agent with default_review_required cannot bypass the review gate.
// task done transitions to awaiting_review.
// Calling task done again while awaiting_review is blocked.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn no_bypass_when_review_required() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[("builder", "builder", true, &[])],
        "",
    );

    // Agent script: claim and complete the task.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["bypass test task"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the agent to claim and attempt done.
    // The task should be in awaiting_review (not done).
    let review_out = wait_for(
        &project,
        &["task", "list", "--status", "awaiting_review"],
        Duration::from_secs(20),
        |out| out.contains("bypass test task"),
    );
    assert!(
        review_out.contains("bypass test task"),
        "task should be in awaiting_review list: {review_out}"
    );

    // Verify the task is NOT in done list.
    let (_, done_out) = cli(&project, &["task", "list", "--status", "done"]);
    assert!(
        !done_out.contains("bypass test task"),
        "task should NOT be done: {done_out}"
    );

    // Attempt to bypass via task done as the same agent — should be rejected.
    let (ok, bypass_out) = cli_as(&project, "builder", &["task", "done", "1", "--note", "bypass"]);
    assert!(
        !ok,
        "task done should be rejected while awaiting_review: {bypass_out}"
    );
    assert!(
        bypass_out.contains("awaiting_review") || bypass_out.contains("cannot"),
        "error should mention awaiting_review: {bypass_out}"
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
// Test 2: hub auto-files a review task when a task enters awaiting_review.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn hub_auto_files_review_task() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    // Builder with review required + reviewer with "review" capability
    write_config(
        &project,
        &[
            ("builder", "builder", true, &[]),
            ("reviewer", "reviewer", false, &["review"]),
        ],
        "",
    );

    // Agent script: claim and complete the task.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    // Reviewer script: just idle (no-op)
    std::fs::write(
        scripts.join("reviewer.ndjson"),
        r#"{"run": [], "text": "idle", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["feature XYZ"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the review task to appear.
    let review_task_out = wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(20),
        |out| out.contains("Review: feature XYZ"),
    );
    assert!(
        review_task_out.contains("Review: feature XYZ"),
        "hub should auto-file a review task: {review_task_out}"
    );

    // The review task should be claimed by the reviewer.
    let (_, show_out) = cli(&project, &["task", "list", "--status", "claimed"]);
    assert!(
        show_out.contains("Review: feature XYZ") || show_out.contains("reviewer"),
        "review task should be claimed: {show_out}"
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
// Test 3: reviewer approval transitions awaiting_review → done.
// The original task is the one in awaiting_review. Approving it via
// `agentcom task review 1 --approve` (as a different agent) marks it Done.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn review_approve_transitions_to_done() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[
            ("builder", "builder", true, &[]),
            ("reviewer", "reviewer", false, &["review"]),
        ],
        "",
    );

    // Builder script: claim and complete.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    // Reviewer script: idle (we'll approve manually via CLI)
    std::fs::write(
        scripts.join("reviewer.ndjson"),
        r#"{"run": [], "text": "idle", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["approve me"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the original task (#1) to reach awaiting_review.
    wait_for(
        &project,
        &["task", "list", "--status", "awaiting_review"],
        Duration::from_secs(20),
        |out| out.contains("approve me"),
    );

    // Approve original task #1 as the reviewer (not the builder).
    let (ok, approve_out) = cli_as(
        &project,
        "reviewer",
        &["task", "review", "1", "--approve"],
    );
    assert!(
        ok,
        "review approve should succeed: {approve_out}"
    );

    // The original task should now be Done.
    let done_out = wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(10),
        |out| out.contains("approve me"),
    );
    assert!(
        done_out.contains("approve me"),
        "approved task should be done: {done_out}"
    );

    // It should no longer be in awaiting_review.
    let (_, awaiting_out) = cli(&project, &["task", "list", "--status", "awaiting_review"]);
    assert!(
        !awaiting_out.contains("approve me"),
        "task should not still be awaiting_review: {awaiting_out}"
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
// Test 4: reviewer rejection reopens the task.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn review_reject_reopens_task() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[
            ("builder", "builder", true, &[]),
            ("reviewer", "reviewer", false, &["review"]),
        ],
        "",
    );

    // Builder script: claim and complete.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    // Reviewer script: idle (we'll reject manually)
    std::fs::write(
        scripts.join("reviewer.ndjson"),
        r#"{"run": [], "text": "idle", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["fix bug #42"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the task to reach awaiting_review.
    wait_for(
        &project,
        &["task", "list", "--status", "awaiting_review"],
        Duration::from_secs(20),
        |out| out.contains("fix bug #42"),
    );

    // Reject task #1 as reviewer with a note
    let (ok, reject_out) = cli_as(
        &project,
        "reviewer",
        &["task", "review", "1", "--reject", "--note", "needs more tests"],
    );
    assert!(
        ok,
        "review reject should succeed: {reject_out}"
    );

    // The task should now be open again.
    let open_out = wait_for(
        &project,
        &["task", "list", "--status", "open"],
        Duration::from_secs(10),
        |out| out.contains("fix bug #42"),
    );
    assert!(
        open_out.contains("fix bug #42"),
        "rejected task should be open: {open_out}"
    );

    // Verify the rejection note is visible in task show.
    let (ok, show_out) = cli(&project, &["task", "show", "1"]);
    assert!(ok, "task show succeeds: {show_out}");
    assert!(
        show_out.contains("rejected") || show_out.contains("needs more tests"),
        "show should mention rejection: {show_out}"
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
// Test 5: original closing agent cannot self-review.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn self_review_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[("builder", "builder", true, &[])],
        "",
    );

    // Builder script: claim and complete.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["self review test"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the task to reach awaiting_review.
    wait_for(
        &project,
        &["task", "list", "--status", "awaiting_review"],
        Duration::from_secs(20),
        |out| out.contains("self review test"),
    );

    // Try to approve as the same agent (builder) — should be rejected.
    let (ok, self_review_out) = cli_as(
        &project,
        "builder",
        &["task", "review", "1", "--approve"],
    );
    assert!(
        !ok,
        "self-review should be rejected: {self_review_out}"
    );
    assert!(
        self_review_out.contains("cannot") || self_review_out.contains("self") || self_review_out.contains("own"),
        "error should mention self-review is not allowed: {self_review_out}"
    );

    // The task should remain awaiting_review.
    let (_, awaiting_out) = cli(&project, &["task", "list", "--status", "awaiting_review"]);
    assert!(
        awaiting_out.contains("self review test"),
        "task should still be awaiting_review: {awaiting_out}"
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
// Test 6: when default_review_required=false (default), task done goes
// straight to done with no review gate.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn no_review_gate_when_flag_false() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[("builder", "builder", false, &[])],
        "",
    );

    // Agent script: claim and complete the task.
    std::fs::write(
        scripts.join("builder.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done-no-review"], "text": "done", "cost": 0.01}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["no review needed"], &[]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Wait for the task to become done.
    let done_out = wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("no review needed"),
    );
    assert!(
        done_out.contains("no review needed"),
        "task should be done: {done_out}"
    );

    // Verify it's NOT in awaiting_review.
    let (_, awaiting_out) = cli(&project, &["task", "list", "--status", "awaiting_review"]);
    assert!(
        !awaiting_out.contains("no review needed"),
        "task should NOT be awaiting_review: {awaiting_out}"
    );

    // Verify no review task was created.
    let (_, all_out) = cli(&project, &["task", "list"]);
    assert!(
        !all_out.contains("Review: no review needed"),
        "no review task should be created: {all_out}"
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
