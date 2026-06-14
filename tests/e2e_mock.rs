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
        // Best-effort graceful stop, then hard kill.
        // Strip the hub's IPC env vars so discover() uses hub.json for this
        // test project instead of routing to the outer hub (if tests run inside
        // an agentcom agent that has AGENTCOM_PORT set in its environment).
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
    let mut cfg = String::from("project_name = \"e2e\"\ninterrupt_timeout_secs = 2\n");
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
    start_hub_args(project, script_dir, tasks, extra_env, &[])
}

fn start_hub_args(
    project: &Path,
    script_dir: &Path,
    tasks: &[&str],
    extra_env: &[(&str, &str)],
    extra_args: &[&str],
) -> HubGuard {
    let mut cmd = Command::new(agentcom_bin());
    cmd.arg("up").arg("--headless");
    for t in tasks {
        cmd.arg("--task").arg(t);
    }
    cmd.args(extra_args);
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

/// True when the status output shows `agent` in `state` (the injected
/// composer means a bare "working" substring check is ambiguous).
fn agent_in_state(status_out: &str, agent: &str, state: &str) -> bool {
    status_out
        .lines()
        .any(|l| l.contains(agent) && l.contains(state))
}

/// Run a CLI command and return (success, stdout-only, stderr-only).
fn cli_split(project: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(agentcom_bin())
        .args(args)
        .current_dir(project)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
        .env_remove("AGENTCOM_AGENT")
        .output()
        .expect("cli runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Poll a CLI command until its output satisfies `pred`.
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
    let status = wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("e2e —")
    });
    assert!(status.contains("alice"), "status lists agents: {status}");
    assert!(status.contains("bob"), "status lists agents: {status}");

    // Graceful shutdown via CLI.
    let (ok, out) = cli(&project, &["stop"]);
    assert!(ok, "stop succeeds: {out}");
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

#[test]
fn codex_provider_uses_adapter_and_completes_task() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[("coder", "codex-backed worker")],
        "default_provider = \"codex\"\n",
    );

    let _hub = start_hub(
        &project,
        &scripts,
        &["prove codex provider works"],
        &[(
            "MOCK_CODEX_RUN",
            "agentcom task claim 1;;agentcom task done 1 --note codex-finished",
        )],
    );

    let done = wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("codex-finished"),
    );
    assert!(done.contains("#1"), "codex-backed task completed: {done}");

    let status = wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("coder")
    });
    assert!(status.contains("idle"), "agent returned idle: {status}");
}

#[test]
fn deepseek_provider_uses_adapter_and_completes_task() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(
        &project,
        &[("seeker", "deepseek-backed worker")],
        "default_provider = \"deepseek\"\n",
    );

    let _hub = start_hub(
        &project,
        &scripts,
        &["prove deepseek provider works"],
        &[
            (
                "MOCK_DEEPSEEK_RUN",
                "agentcom task claim 1;;agentcom task done 1 --note deepseek-finished",
            ),
            ("MOCK_DEEPSEEK_RESPONSE", "mock deepseek done"),
        ],
    );

    let done = wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("deepseek-finished"),
    );
    assert!(
        done.contains("#1"),
        "deepseek-backed task completed: {done}"
    );

    let status = wait_for(&project, &["status"], Duration::from_secs(10), |out| {
        out.contains("seeker")
    });
    assert!(
        status.contains("deepseek"),
        "status shows provider badge/name: {status}"
    );
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

    // Wait until the worker (specifically) is mid-turn.
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        agent_in_state(out, "worker", "working")
    });

    let (ok, out) = cli(
        &project,
        &["interrupt", "worker", "stop what you are doing"],
    );
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
        agent_in_state(out, "stubborn", "working")
    });

    let (ok, _) = cli(&project, &["interrupt", "stubborn", "please stop"]);
    assert!(ok);

    // interrupt_timeout_secs = 2 → tree kill → auto-restart with --resume.
    // After restart the agent is fed the urgent message; the fresh mock
    // process replays step 1 (await_interrupt) so it ends up working again.
    wait_for(&project, &["status"], Duration::from_secs(30), |out| {
        agent_in_state(out, "stubborn", "working")
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
fn agent_can_recruit_a_teammate_and_cap_is_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    // Cap of 2: the starter may recruit exactly one teammate.
    write_config(&project, &[("lead", "decomposes work")], "max_agents = 2\n");

    // The lead recruits a helper on its first turn, then tries to recruit a
    // second one (which must be rejected by the cap) and records the result.
    std::fs::write(
        scripts.join("lead.ndjson"),
        r#"{"run": ["agentcom agent add helper --role \"recruited helper\"", "agentcom send helper \"welcome\""], "text": "recruited"}
"#,
    )
    .unwrap();
    // The recruit proves it's alive by filing a task when the welcome lands.
    std::fs::write(
        scripts.join("helper.ndjson"),
        r#"{"run": ["agentcom task add \"recruit-reporting\"", "agentcom agent add third --role \"one too many\""], "text": "hi"}
"#,
    )
    .unwrap();

    let _hub = start_hub(&project, &scripts, &["kick off"], &[]);

    // The recruit went live and did work.
    wait_for(&project, &["status"], Duration::from_secs(20), |out| {
        out.contains("helper")
    });
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(20),
        |out| out.contains("recruit-reporting"),
    );

    // Recruit persisted to config.
    let cfg = std::fs::read_to_string(project.join("agentcom.toml")).unwrap();
    assert!(cfg.contains("helper"), "recruit persisted:\n{cfg}");

    // The cap held: "third" must not exist anywhere.
    let (_, status) = cli(&project, &["status"]);
    assert!(!status.contains("third"), "cap enforced: {status}");
    let cfg = std::fs::read_to_string(project.join("agentcom.toml")).unwrap();
    assert!(
        !cfg.contains("one too many"),
        "cap rejection not persisted:\n{cfg}"
    );
}

#[test]
fn composer_chat_and_file_claims() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    // Note: NO composer in the config — `up` must inject one.
    write_config(&project, &[("worker", "does the work")], "");

    // The composer reacts to the human's chat message: claims a file (so the
    // worker will collide), files a task, and reports back to the human.
    std::fs::write(
        scripts.join("composer.ndjson"),
        r#"{"run": ["agentcom files claim src/app.js", "agentcom task add \"update src/app.js\"", "agentcom send human \"plan is in motion\""], "text": "delegated"}
"#,
    )
    .unwrap();
    // The worker tries to claim the same file (must be rejected), records the
    // rejection, then reports to the human too.
    std::fs::write(
        scripts.join("worker.ndjson"),
        r#"{"run": ["agentcom files claim src/app.js || agentcom task add \"claim-was-rejected\"", "agentcom send human \"worker checking in\""], "text": "tried"}
"#,
    )
    .unwrap();

    let _hub = start_hub(&project, &scripts, &[], &[]);

    // Composer was injected and is visible.
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("composer") && out.contains("worker")
    });

    // Human chats with the composer (this is what the TUI chat input sends).
    let (ok, out) = cli(&project, &["send", "composer", "please update app.js"]);
    assert!(ok, "{out}");

    // Composer replied to the human; the reply is readable as the human.
    wait_for(&project, &["inbox"], Duration::from_secs(20), |out| {
        out.contains("plan is in motion")
    });

    // The composer's file claim is on record...
    let files = wait_for(
        &project,
        &["files", "list"],
        Duration::from_secs(10),
        |out| out.contains("src/app.js"),
    );
    assert!(
        files.contains("composer"),
        "composer holds the claim: {files}"
    );

    // ...and the worker's conflicting claim was rejected (it filed the marker
    // task from the `||` fallback branch).
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(20),
        |out| out.contains("claim-was-rejected"),
    );
}

#[test]
fn free_mode_nudges_composer_when_fleet_idles() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does the work")], "");
    // The composer reacts to the free-mode nudge by queuing real work.
    std::fs::write(
        scripts.join("composer.ndjson"),
        r#"{"run": ["agentcom task add \"free-work-1\"", "agentcom send human \"queued the next round\""], "text": "planned"}
"#,
    )
    .unwrap();

    let _hub = start_hub_args(
        &project,
        &scripts,
        &[],
        &[("AGENTCOM_FREE_NUDGE_SECS", "1")],
        &[
            "--free",
            "continuously improve this project",
            "--for",
            "10m",
        ],
    );

    // No seed tasks, everyone idles -> the hub nudges the composer, which
    // files work toward the standing goal.
    wait_for(
        &project,
        &["task", "list"],
        Duration::from_secs(20),
        |out| out.contains("free-work-1"),
    );
    let (_, status) = cli(&project, &["status"]);
    assert!(status.contains("FREE MODE"), "status shows mode: {status}");
    assert!(
        status.contains("continuously improve"),
        "status shows goal: {status}"
    );
}

#[test]
fn free_mode_time_limit_shuts_down() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does the work")], "");

    let mut hub = start_hub_args(
        &project,
        &scripts,
        &[],
        // Keep nudges out of the way so shutdown is the only event.
        &[("AGENTCOM_FREE_NUDGE_SECS", "600")],
        &["--free", "anything", "--for", "2s"],
    );

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Ok(Some(exit)) = hub.child.try_wait() {
            assert!(
                exit.success(),
                "hub exits cleanly at the time limit; logs:\n{}",
                hub_logs(&mut hub)
            );
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "hub did not stop at the 2s time limit; logs:\n{}",
                hub_logs(&mut hub)
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
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

#[test]
fn task_edit_and_show() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does the work")], "");
    let _hub = start_hub(&project, &scripts, &[], &[]);

    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("idle") || out.contains("working")
    });

    // Add a task via CLI.
    let (ok, out) = cli(&project, &["task", "add", "original title", "-d", "original desc"]);
    assert!(ok, "task add: {out}");

    // Edit just the title; description should be preserved.
    let (ok, out) = cli(&project, &["task", "edit", "1", "--title", "updated title"]);
    assert!(ok, "task edit: {out}");
    assert!(out.contains("updated title"), "edit returns new title: {out}");

    // task show returns the single-task view with the updated title.
    let (ok, out) = cli(&project, &["task", "show", "1"]);
    assert!(ok, "task show: {out}");
    assert!(out.contains("updated title"), "show reflects edit: {out}");
    assert!(out.contains("original desc"), "show keeps original desc: {out}");

    // Editing with no fields errors gracefully.
    let (ok, out) = cli(&project, &["task", "edit", "1"]);
    assert!(!ok, "edit with no fields should fail: {out}");
    assert!(out.contains("nothing to update"), "proper error message: {out}");

    // show on a non-existent task errors gracefully.
    let (ok, out) = cli(&project, &["task", "show", "999"]);
    assert!(!ok, "show nonexistent task should fail: {out}");
    assert!(out.contains("not found"), "proper error message: {out}");
}

#[test]
fn json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does the work")], "");
    let _hub = start_hub(&project, &scripts, &["do something"], &[]);

    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("idle") || out.contains("working")
    });

    // agentcom status --json should produce a valid JSON object on stdout.
    let (ok, stdout, stderr) = cli_split(&project, &["status", "--json"]);
    assert!(ok, "status --json: {stdout}{stderr}");
    let val: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("status --json is valid JSON");
    assert!(val["agents"].is_array(), "has agents array: {stdout}");
    assert!(val["open_tasks"].is_number(), "has open_tasks: {stdout}");
    assert!(val["total_cost_usd"].is_number(), "has total_cost_usd: {stdout}");

    // agentcom task list --json should produce a valid JSON array on stdout.
    let (ok, stdout, stderr) = cli_split(&project, &["task", "list", "--json"]);
    assert!(ok, "task list --json: {stdout}{stderr}");
    let arr: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("task list --json is valid JSON array");
    assert!(arr.is_array(), "task list --json is an array: {stdout}");
    // Seed task should be present.
    let tasks = arr.as_array().unwrap();
    assert!(!tasks.is_empty(), "seed task present: {stdout}");
    assert_eq!(tasks[0]["id"], 1, "first task has id 1: {stdout}");
}

#[test]
fn logs_command() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does the work")], "");
    let _hub = start_hub(&project, &scripts, &[], &[]);

    // Give the hub a moment to emit logs.
    wait_for(&project, &["status"], Duration::from_secs(15), |out| {
        out.contains("idle") || out.contains("working")
    });

    // agentcom logs reads hub log files without a running hub connection.
    let (ok, out) = cli(&project, &["logs"]);
    assert!(ok, "logs command succeeds: {out}");

    // -n limits output lines; should still succeed even if log is small.
    let (ok, out) = cli(&project, &["logs", "-n", "5"]);
    assert!(ok, "logs -n 5 succeeds: {out}");

    // --agent filter is case-insensitive; hub log contains "hub" entries.
    let (ok, out) = cli(&project, &["logs", "--agent", "HUB"]);
    assert!(ok, "logs --agent filter succeeds: {out}");
    let _ = out;
}

#[test]
fn shell_completions() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("agentcom.toml"), "project_name = \"test\"\n[[agent]]\nname = \"w\"\nrole = \"worker\"\n").unwrap();

    // bash completions should produce non-empty output without a hub.
    let (_, stdout, stderr) = cli_split(&project, &["completions", "bash"]);
    assert!(!stdout.is_empty(), "bash completions non-empty: stderr={stderr}");
    assert!(stdout.contains("agentcom"), "completions mention the binary: {stdout}");

    // zsh completions also work.
    let (_, stdout, _) = cli_split(&project, &["completions", "zsh"]);
    assert!(!stdout.is_empty(), "zsh completions non-empty");
}

#[test]
fn budget_command() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("spender", "does work and accrues cost")], "");
    // Agent completes a task, creating a runs entry with cost.
    std::fs::write(
        scripts.join("spender.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note spent"], "text": "done", "cost": 0.05}
"#,
    )
    .unwrap();

    let mut hub = start_hub(&project, &scripts, &["cost-tracking task"], &[]);
    wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("spent"),
    );

    // Graceful stop so runs are finalized in the DB.
    let _ = Command::new(agentcom_bin())
        .args(["stop"])
        .current_dir(&project)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
        .env_remove("AGENTCOM_AGENT")
        .output();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if matches!(hub.child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // agentcom budget reads the DB without a running hub.
    let (_, stdout, stderr) = cli_split(&project, &["budget"]);
    assert!(!stdout.is_empty(), "budget produces output: stderr={stderr}");
    assert!(stdout.contains("spender"), "budget shows agent name: {stdout}");
    assert!(
        stdout.contains("COST") || stdout.contains("$"),
        "budget shows cost column: {stdout}"
    );
}

#[test]
fn config_show() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("agentcom.toml"),
        "project_name = \"myproj\"\n[[agent]]\nname = \"w\"\nrole = \"worker\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = cli_split(&project, &["config", "show"]);
    assert!(ok, "config show exits 0: stderr={stderr}");
    assert!(stdout.contains("myproj"), "config show includes project name: {stdout}");
    // Output must be valid JSON.
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("config show output is valid JSON: {e}\n{stdout}"));
    assert_eq!(v["project_name"], "myproj");
    assert!(v["agent"].is_array(), "agent field is present");
}

#[test]
fn task_export() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("w", "worker")], "");
    std::fs::write(
        scripts.join("w.ndjson"),
        r#"{"run": ["agentcom task add \"open task\"", "agentcom task claim 1", "agentcom task done 1 --note finished"], "text": "done"}
"#,
    )
    .unwrap();

    let hub = start_hub(&project, &scripts, &["seed task"], &[]);
    wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("finished"),
    );
    drop(hub);

    // agentcom task export reads the DB without a hub.
    let (ok, stdout, stderr) = cli_split(&project, &["task", "export"]);
    assert!(ok, "task export exits 0: stderr={stderr}");
    assert!(stdout.contains("##"), "export has markdown sections: {stdout}");
    assert!(stdout.contains("- ["), "export has checklist items: {stdout}");
}

#[test]
fn task_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();

    write_config(&project, &[("worker", "does tasks")], "");
    std::fs::write(
        scripts.join("worker.ndjson"),
        r#"{"run": ["agentcom task claim 1", "agentcom task done 1 --note done"], "text": "done"}
"#,
    )
    .unwrap();

    let hub = start_hub(&project, &scripts, &["a task to complete"], &[]);
    wait_for(
        &project,
        &["task", "list", "--status", "done"],
        Duration::from_secs(20),
        |out| out.contains("done"),
    );
    drop(hub);

    // agentcom task stats reads the DB without a hub.
    let (ok, stdout, stderr) = cli_split(&project, &["task", "stats"]);
    assert!(ok, "task stats exits 0: stderr={stderr}");
    assert!(stdout.contains("total"), "stats shows total: {stdout}");
    assert!(stdout.contains("done"), "stats shows done count: {stdout}");

    // --json flag returns valid JSON with expected fields.
    let (ok, stdout, stderr) = cli_split(&project, &["task", "stats", "--json"]);
    assert!(ok, "task stats --json exits 0: stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("task stats --json is valid JSON: {e}\n{stdout}"));
    assert!(v["total"].as_u64().unwrap_or(0) >= 1, "total >= 1: {stdout}");
    assert_eq!(v["done"], 1, "one done task: {stdout}");
    assert!(v["top_claimants"].is_array(), "top_claimants is array: {stdout}");
    let claimants = v["top_claimants"].as_array().unwrap();
    assert!(!claimants.is_empty(), "worker appears in top claimants: {stdout}");
    assert_eq!(claimants[0]["agent"], "worker");
    assert_eq!(claimants[0]["tasks_done"], 1);
}
