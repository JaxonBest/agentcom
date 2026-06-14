//! Test double for the `claude` CLI: speaks just enough of the stream-json
//! protocol to exercise the hub's full loop (scheduler, messaging,
//! interrupts, crash handling) in `cargo test` at zero API cost.
//!
//! Behavior is scripted through environment variables:
//! - `MOCK_SCRIPT` — path to an NDJSON file; line N describes the
//!   response to the Nth user message:
//!   `{"text": "...", "run": ["cmd", ...], "sleep_ms": 100, "cost": 0.01,
//!   "await_interrupt": true, "exit": true}`
//!   When the script runs out, every further message gets a plain text + result response.
//! - `MOCK_SCRIPT_DIR` — directory of per-agent scripts; the file
//!   `<dir>/<AGENTCOM_AGENT>.ndjson` takes precedence over MOCK_SCRIPT.
//! - `MOCK_IGNORE_INTERRUPT=1` — swallow control_requests without aborting
//!   (exercises the hub's kill/escalation path).
//! - `MOCK_SESSION_ID` — session id for the init event (default: random-ish).

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn emit(v: Value) {
    let mut line = v.to_string();
    line.push('\n');
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes()).unwrap();
    out.flush().unwrap();
}

struct Step {
    text: String,
    run: Vec<String>,
    sleep_ms: u64,
    cost: f64,
    await_interrupt: bool,
    exit: bool,
}

fn script_path() -> Option<String> {
    if let (Ok(dir), Ok(agent)) = (
        std::env::var("MOCK_SCRIPT_DIR"),
        std::env::var("AGENTCOM_AGENT"),
    ) {
        let p = std::path::Path::new(&dir).join(format!("{agent}.ndjson"));
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    std::env::var("MOCK_SCRIPT").ok()
}

fn load_script() -> Vec<Step> {
    let Some(path) = script_path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| Step {
            text: v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("ok")
                .to_string(),
            run: v
                .get("run")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            sleep_ms: v.get("sleep_ms").and_then(|s| s.as_u64()).unwrap_or(0),
            cost: v.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.001),
            await_interrupt: v
                .get("await_interrupt")
                .and_then(|a| a.as_bool())
                .unwrap_or(false),
            exit: v.get("exit").and_then(|e| e.as_bool()).unwrap_or(false),
        })
        .collect()
}

fn run_command(cmdline: &str) -> String {
    // Run through the platform shell so PATH (with the agentcom dir
    // prepended by the hub) applies, mirroring an agent's Bash tool.
    #[cfg(windows)]
    let output = {
        // raw_arg: cmd.exe does its own parsing; Rust's default quoting
        // would turn embedded quotes into literal characters.
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(cmdline)
            .output()
    };
    #[cfg(not(windows))]
    let output = std::process::Command::new("sh")
        .args(["-c", cmdline])
        .output();
    match output {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("error: {e}"),
    }
}

fn main() {
    let session_id =
        std::env::var("MOCK_SESSION_ID").unwrap_or_else(|_| format!("mock-{}", std::process::id()));
    let ignore_interrupt = std::env::var("MOCK_IGNORE_INTERRUPT").is_ok();
    let script = load_script();
    let mut step_idx = 0usize;
    let mut total_cost = 0.0f64;
    let mut num_turns = 0u64;

    emit(json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
        "model": "mock-model",
        "tools": ["Bash"],
    }));

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("control_request") => {
                let request_id = v
                    .get("request_id")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                if ignore_interrupt {
                    // Pretend we never saw it; the hub must escalate.
                    continue;
                }
                emit(json!({
                    "type": "control_response",
                    "response": { "subtype": "success", "request_id": request_id },
                }));
                num_turns += 1;
                emit(json!({
                    "type": "result",
                    "subtype": "error_interrupted",
                    "is_error": true,
                    "total_cost_usd": total_cost,
                    "num_turns": num_turns,
                    "session_id": session_id,
                }));
            }
            Some("user") => {
                let step = script.get(step_idx);
                step_idx += 1;

                if let Some(s) = step {
                    if s.exit {
                        std::process::exit(7);
                    }
                    if s.await_interrupt {
                        // Turn "in progress" forever — only a control_request
                        // (or stdin close) ends it. Skip the result entirely.
                        continue;
                    }
                    if s.sleep_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(s.sleep_ms));
                    }
                    for cmd in &s.run {
                        let out = run_command(cmd);
                        emit(json!({
                            "type": "assistant",
                            "message": {
                                "role": "assistant",
                                "content": [{ "type": "tool_use", "id": "t1", "name": "Bash",
                                              "input": { "command": cmd } }],
                            },
                        }));
                        emit(json!({
                            "type": "user",
                            "message": { "role": "user",
                                         "content": [{ "type": "tool_result", "content": out }] },
                        }));
                    }
                    total_cost += s.cost;
                    emit(json!({
                        "type": "assistant",
                        "message": {
                            "role": "assistant",
                            "content": [{ "type": "text", "text": s.text }],
                        },
                    }));
                } else {
                    total_cost += 0.001;
                    emit(json!({
                        "type": "assistant",
                        "message": {
                            "role": "assistant",
                            "content": [{ "type": "text", "text": "(script exhausted — idling)" }],
                        },
                    }));
                }
                num_turns += 1;
                emit(json!({
                    "type": "result",
                    "subtype": "success",
                    "is_error": false,
                    "total_cost_usd": total_cost,
                    "num_turns": num_turns,
                    "session_id": session_id,
                    "result": "turn done",
                }));
            }
            _ => {}
        }
    }
}
