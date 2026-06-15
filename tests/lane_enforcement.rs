//! Integration tests for Workstream B: typed lane enforcement.
//!
//! Validates:
//! 1. FilesClaim outside declared lane globs is hard-rejected.
//! 2. FilesClaim inside declared lane globs succeeds.
//! 3. `agentcom check` warns on a config with a typo lane.
//!
//! Uses a real hub with a simple agentcom.toml; agents are mocked
//! but we interact via `agentcom files claim` with AGENTCOM_AGENT set.

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

fn write_config(project: &Path, extra: &str) {
    let cfg = format!(
        "project_name = \"lane-test\"\n\
         interrupt_timeout_secs = 2\n\
         max_agents = 5\n\
         {extra}"
    );
    std::fs::write(project.join("agentcom.toml"), cfg).unwrap();
}

fn start_hub(project: &Path, tasks: &[&str]) -> HubGuard {
    let mut cmd = Command::new(agentcom_bin());
    cmd.arg("up").arg("--headless");
    for t in tasks {
        cmd.arg("--task").arg(t);
    }
    // Set all adapter env vars to mock paths so the hub starts without real adapters.
    cmd.current_dir(project)
        .env("AGENTCOM_CLAUDE_EXE", mock_claude_bin())
        .env("AGENTCOM_CODEX_ADAPTER_EXE", "/nonexistent/codex-adapter")
        .env("AGENTCOM_CODEX_EXE", "/nonexistent/codex")
        .env("AGENTCOM_DEEPSEEK_ADAPTER_EXE", "/nonexistent/deepseek-adapter")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
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
    // Capture hub stderr for debugging
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

/// Claim files via `agentcom files claim` with a specific agent identity.
fn files_claim(project: &Path, agent: &str, paths: &[&str]) -> (bool, String) {
    let mut args = vec!["files", "claim"];
    for p in paths {
        args.push(p);
    }
    let out = Command::new(agentcom_bin())
        .args(&args)
        .current_dir(project)
        .env("AGENTCOM_AGENT", agent)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
        .output()
        .expect("files claim runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// Run `agentcom check` and return (exit_success, stderr contents).
fn run_check(project: &Path) -> (bool, String) {
    let out = Command::new(agentcom_bin())
        .args(["check"])
        .current_dir(project)
        .env_remove("AGENTCOM_PORT")
        .env_remove("AGENTCOM_TOKEN")
        .env_remove("AGENTCOM_AGENT")
        .output()
        .expect("check runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}\n{stderr}"))
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[test]
fn builder_claims_out_of_lane_is_rejected() {
    let project = PathBuf::from(std::env!("CARGO_TARGET_TMPDIR"));
    let project = project.join("lane_test_reject");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(project.join("tests/foo.py"), "def test_foo(): pass").unwrap();

    write_config(
        &project,
        "\n\
         [[agent]]\n\
         name = \"builder\"\n\
         role = \"builder\"\n\
         provider = \"claude\"\n\
         lanes = [\"src/**\"]\n",
    );

    let hub = start_hub(&project, &["test task"]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Claim a path OUTSIDE the lane (tests/foo.py is not in src/**)
    let (ok, out) = files_claim(&project, "builder", &["tests/foo.py"]);
    assert!(
        !ok,
        "claim of out-of-lane path should be rejected; got output:\n{out}"
    );
    assert!(
        out.contains("lane violation"),
        "expected 'lane violation' in error message; got:\n{out}"
    );

    drop(hub);
}

#[test]
fn builder_claims_in_lane_succeeds() {
    let project = PathBuf::from(std::env!("CARGO_TARGET_TMPDIR"));
    let project = project.join("lane_test_accept");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}").unwrap();

    write_config(
        &project,
        "\n\
         [[agent]]\n\
         name = \"builder\"\n\
         role = \"builder\"\n\
         provider = \"claude\"\n\
         lanes = [\"src/**\"]\n",
    );

    let hub = start_hub(&project, &["test task"]);
    wait_for_hub(&project, Duration::from_secs(20));

    // Claim a path INSIDE the lane
    let (ok, out) = files_claim(&project, "builder", &["src/main.rs"]);
    assert!(
        ok,
        "claim of in-lane path should succeed; got output:\n{out}"
    );
    assert!(
        out.contains("claimed"),
        "expected 'claimed' in success message; got:\n{out}"
    );

    drop(hub);
}

#[test]
fn agentcom_check_warns_on_typo_lane() {
    let project = PathBuf::from(std::env!("CARGO_TARGET_TMPDIR"));
    let project = project.join("lane_check_typo");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}").unwrap();

    // Deliberately typo the lane glob: "src/buidler/**" instead of "src/builder/**"
    write_config(
        &project,
        "\n\
         [[agent]]\n\
         name = \"builder\"\n\
         role = \"builder\"\n\
         provider = \"claude\"\n\
         lanes = [\"src/buidler/**\"]\n",
    );

    let (ok, out) = run_check(&project);
    if !ok {
        eprintln!("agentcom check failed. Full output:\n{out}");
    }
    assert!(ok, "agentcom check should exit 0 even with warnings; output:\n{out}");
    assert!(
        out.contains("no files in project match any glob pattern"),
        "expected warning about no matching files; got:\n{out}"
    );
}
