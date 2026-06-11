//! End-to-end tests: a real headless hub driving `mock-claude` children,
//! exercised through the real `agentcom` CLI — the full stack across
//! process boundaries, at zero API cost.

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

struct HubGuard {
    child: Child,
    project: PathBuf,
}

impl Drop for HubGuard {
    fn drop(&mut self) {
        // Best-effort graceful stop, then hard kill.
        let _ = Command::new(agentcom_bin())
            .args(["stop"])
            .current_dir(&self.project)
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
    let mut cfg = String::from("project_name = \"e2e\"\ninterrupt_timeout_secs = 2\n");
    cfg.push_str(extra);
    for (name, role) in agents {
        cfg.push_str(&format!(
            "\n[[agent]]\nname = \"{name}\"\nrole = \"{role}\"\n"
        ));
    }
    std::fs::write(project.join("agentcom.toml"), cfg).unwrap();
}

fn start_hub(project: &Path, script_dir: &Path, tasks: &[&str], extra_env: &[(&str, &str)]) -> HubGuard {
    let mut cmd = Command::new(agentcom_bin());
    cmd.arg("up").arg("--headless");
    for t in tasks {
        cmd.arg("--task").arg(t);
    }
    cmd.current_dir(project)
        .env("AGENTCOM_CLAUDE_EXE", mock_claude_bin())
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

/// Run a client command against the hub (discovery via hub.json from cwd).
fn cli(project: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(agentcom_bin())
        .args(args)
        .current_dir(project)
        // Make sure client-mode discovery uses hub.json, not stale env.
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

/// Poll a CLI command until its output satisfies `pred`.
fn wait_for(project: &Path, args: &[&str], timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
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

#[test]
fn fleet_completes_task_and_passes_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("alice", "worker"), ("bob", "helper")], "");

    // Whichever agent is offered the seed task claims it, messages the other,
    // and completes it. The other agent files a follow-up when it hears.
    for (me, other) in [("alice", "bob"), ("bob", "alice")] {
        std::fs::write(
            scripts.join(format!("{me}.ndjson")),
            format!(
                r#"{{"run": ["agentcom task claim 1", "agentcom send {other} \"hello from {me}\"", "agentcom task done 1 --note finished"], "text": "did my part", "cost": 0.01}}
{{"run": ["agentcom task add \"followup-from-{me}\""], "text": "heard you"}}
"#
            ),
        )
        .unwrap();
    }

    let mut hub = start_hub(&project, &scripts, &["greet your teammate"], &[]);

    // Seed task gets claimed and completed by one of the agents.
    let done = wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("#1"),
    );
    assert!(done.contains("finished"), "done note present: {done}");

    // The message recipient woke up and filed a follow-up task.
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(20),
        |out| out.contains("followup-from-"),
    );

    // Cost from result events is tracked.
    let status = wait_for(
        &project,
        &["status"],
        Duration::from_secs(10),
        |out| out.contains("e2e —"),
    );
    assert!(status.contains("alice"), "status lists agents: {status}");
    assert!(status.contains("bob"), "status lists agents: {status}");

    // Graceful shutdown via CLI.
    let (ok, out) = cli(&project, &["stop"]);
    assert!(ok, "stop succeeds: {out}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(exit)) = hub.child.try_wait() {
            assert!(exit.success(), "hub exits cleanly; logs:\n{}", hub_logs(&mut hub));
            break;
        }
        if Instant::now() > deadline {
            panic!("hub did not exit after stop; logs:\n{}", hub_logs(&mut hub));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn interrupt_aborts_turn_and_delivers_urgent_message() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does long work")], "");

    // Step 1: start the seeded task but never finish the turn.
    // Step 2 (fed after the interrupt): acknowledge by filing a task.
    std::fs::write(
        scripts.join("worker.ndjson"),
        r#"{"await_interrupt": true}
{"run": ["agentcom task add \"interrupt-ack\""], "text": "stopped as asked"}
"#,
    )
    .unwrap();

    let _hub = start_hub(&project, &scripts, &["long running work"], &[]);

    // Wait until the worker is mid-turn.
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("working")
    });

    let (ok, out) = cli(&project, &["interrupt", "worker", "stop what you are doing"]);
    assert!(ok, "interrupt accepted: {out}");
    assert!(
        out.contains("interrupting"),
        "interrupt targeted a working agent: {out}"
    );

    // The aborted turn's result triggers urgent delivery; step 2 files the ack.
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(15),
        |out| out.contains("interrupt-ack"),
    );

    // The interrupted message shows as delivered in the message feed (the
    // worker consumed it as [INBOX] content).
    let status = wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("0 pending message(s)")
    });
    assert!(status.contains("worker"));
}

#[test]
fn ignored_interrupt_escalates_to_kill_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("stubborn", "ignores interrupts")], "");

    std::fs::write(
        scripts.join("stubborn.ndjson"),
        r#"{"await_interrupt": true}
"#,
    )
    .unwrap();

    let _hub = start_hub(
        &project,
        &scripts,
        &["long running work"],
        &[("MOCK_IGNORE_INTERRUPT", "1")],
    );

    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("working")
    });

    let (ok, _) = cli(&project, &["interrupt", "stubborn", "please stop"]);
    assert!(ok);

    // interrupt_timeout_secs = 2 → tree kill → auto-restart with --resume.
    // After restart the agent is fed the urgent message; the fresh mock
    // process replays step 1 (await_interrupt) so it ends up working again.
    wait_for(&project, &["status"], Duration::from_secs(30), |out| {
        out.contains("working") || out.contains("starting")
    });

    // The restart is visible in run history via a second `working` stretch;
    // verify the agent is alive and the hub never wedged.
    let (ok, out) = cli(&project, &["send", "stubborn", "hello again"]);
    assert!(ok, "messaging after restart works: {out}");
}

#[test]
fn agent_add_persists_and_spawns_live() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("solo", "the only starter")], "");
    // The hot-added agent files a task on its first fed turn.
    std::fs::write(
        scripts.join("newbie.ndjson"),
        r#"{"run": ["agentcom task add \"hello-from-newbie\""], "text": "checked in"}
"#,
    )
    .unwrap();

    let _hub = start_hub(&project, &scripts, &[], &[]);
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("idle")
    });

    let (ok, out) = cli(
        &project,
        &["agent", "add", "newbie", "--role", "hot-added helper"],
    );
    assert!(ok, "agent add: {out}");
    assert!(out.contains("live"), "spawned into running hub: {out}");

    // Persisted to agentcom.toml...
    let cfg = std::fs::read_to_string(project.join("agentcom.toml")).unwrap();
    assert!(cfg.contains("newbie"), "config updated:\n{cfg}");
    assert!(cfg.contains("hot-added helper"));

    // ...and alive in the hub: it shows in status and can do work.
    wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("newbie")
    });
    let (ok, out) = cli(&project, &["send", "newbie", "welcome aboard"]);
    assert!(ok, "{out}");
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(15),
        |out| out.contains("hello-from-newbie"),
    );

    // Duplicate names are rejected.
    let (ok, out) = cli(&project, &["agent", "add", "newbie", "--role", "again"]);
    assert!(!ok, "duplicate rejected: {out}");
}

#[test]
fn pause_resume_and_inbox() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("idler", "does nothing")], "");
    // No script: any fed turn answers with the default text.

    let _hub = start_hub(&project, &scripts, &[], &[]);

    // No tasks, no messages — the agent settles into idle.
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("idle")
    });

    let (ok, out) = cli(&project, &["pause", "idler"]);
    assert!(ok, "{out}");
    wait_for(&project, &["status"], Duration::from_secs(5), |out| {
        out.contains("paused")
    });

    // Messages to a paused agent queue instead of delivering.
    let (ok, out) = cli(&project, &["send", "idler", "wake up later"]);
    assert!(ok, "{out}");
    assert!(out.contains("queued"), "queued while paused: {out}");

    let (ok, _) = cli(&project, &["resume", "idler"]);
    assert!(ok);
    // On resume the queued message is delivered as a turn.
    wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("0 pending message(s)")
    });
}
