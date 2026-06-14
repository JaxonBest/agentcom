//! Adapter that lets the hub supervise Codex through the same stdin/stdout
//! protocol it already uses for Claude Code.
//!
//! The adapter is persistent, but each fed prompt becomes one `codex exec
//! --json` child process. If the hub sends a control interrupt, the adapter
//! kills the current child and emits a turn-ending result.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

#[derive(clap::Parser)]
struct Args {
    #[arg(long)]
    codex_exe: PathBuf,
    #[arg(long)]
    cwd: PathBuf,
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    system_prompt_file: PathBuf,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    resume: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    use clap::Parser;
    let args = Args::parse();
    let system_prompt = std::fs::read_to_string(&args.system_prompt_file)
        .with_context(|| format!("reading {}", args.system_prompt_file.display()))?;

    println!(
        "{}",
        json!({
            "type": "system",
            "subtype": "init",
            "session_id": args.resume.as_deref().unwrap_or(&args.session_id),
            "model": args.model.as_deref().unwrap_or("codex"),
        })
    );

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut resume_id = args.resume.clone();

    while let Some(line) = lines.next_line().await? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => {
                let prompt = extract_user_text(&v).unwrap_or_default();
                let full_prompt = format!("{system_prompt}\n\n{prompt}");
                let result = run_codex_turn(&args, resume_id.as_deref(), &full_prompt).await;
                match result {
                    Ok(turn) => {
                        if let Some(id) = turn.thread_id {
                            resume_id = Some(id.clone());
                        }
                        println!(
                            "{}",
                            json!({
                                "type": "result",
                                "subtype": if turn.failed { "error" } else { "success" },
                                "is_error": turn.failed,
                                "session_id": resume_id.as_deref().unwrap_or(&args.session_id),
                                "num_turns": 1,
                                "result": turn.final_message.unwrap_or_default(),
                            })
                        );
                    }
                    Err(e) => {
                        eprintln!("[codex-adapter] {e:#}");
                        println!(
                            "{}",
                            json!({
                                "type": "result",
                                "subtype": "error_during_execution",
                                "is_error": true,
                                "session_id": resume_id.as_deref().unwrap_or(&args.session_id),
                                "num_turns": 1,
                                "result": e.to_string(),
                            })
                        );
                    }
                }
            }
            Some("control_request") => {
                println!(
                    "{}",
                    json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": v.get("request_id").and_then(|r| r.as_str()).unwrap_or("")
                        }
                    })
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn extract_user_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?.as_array()?;
    let mut out = String::new();
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    Some(out)
}

#[derive(Default)]
struct TurnResult {
    thread_id: Option<String>,
    final_message: Option<String>,
    failed: bool,
}

async fn run_codex_turn(args: &Args, resume: Option<&str>, prompt: &str) -> Result<TurnResult> {
    let mut cmd = Command::new(&args.codex_exe);
    cmd.arg("exec").arg("--json");
    if let Some(model) = &args.model {
        cmd.arg("--model").arg(model);
    }
    cmd.arg("--cd")
        .arg(&args.cwd)
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--ask-for-approval")
        .arg("never");
    if let Some(session) = resume {
        cmd.arg("resume").arg(session).arg(prompt);
    } else {
        cmd.arg(prompt);
    }
    cmd.current_dir(&args.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().context("spawning codex exec")?;
    let stdout = child.stdout.take().expect("codex stdout piped");
    let stderr = child.stderr.take().expect("codex stderr piped");

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("{line}");
        }
    });

    let mut result = TurnResult::default();
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        handle_codex_event(&line, &mut result).await?;
    }

    let status = wait_child(&mut child).await;
    let _ = stderr_task.await;
    if !status {
        result.failed = true;
    }
    Ok(result)
}

async fn handle_codex_event(line: &str, result: &mut TurnResult) -> Result<()> {
    let v: Value = serde_json::from_str(line)
        .with_context(|| format!("parsing codex jsonl event: {line}"))?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("thread.started") => {
            result.thread_id = v
                .get("thread_id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
        }
        Some("item.started") | Some("item.completed") => {
            if let Some(item) = v.get("item") {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("agent_message") => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            result.final_message = Some(text.to_string());
                            println!(
                                "{}",
                                json!({
                                    "type": "assistant",
                                    "message": {
                                        "content": [{ "type": "text", "text": text }]
                                    }
                                })
                            );
                        }
                    }
                    Some("reasoning") => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            println!(
                                "{}",
                                json!({
                                    "type": "assistant",
                                    "message": {
                                        "content": [{ "type": "thinking", "thinking": text }]
                                    }
                                })
                            );
                        }
                    }
                    Some("command_execution") => {
                        let command = item
                            .get("command")
                            .and_then(|c| c.as_str())
                            .unwrap_or("command");
                        println!(
                            "{}",
                            json!({
                                "type": "assistant",
                                "message": {
                                    "content": [{
                                        "type": "tool_use",
                                        "name": "Bash",
                                        "input": { "command": command }
                                    }]
                                }
                            })
                        );
                    }
                    _ => {}
                }
            }
        }
        Some("turn.failed") | Some("error") => {
            result.failed = true;
        }
        Some("turn.completed") => {
            if let Some(usage) = v.get("usage") {
                eprintln!("[usage] {usage}");
            }
        }
        _ => {}
    }
    let _ = tokio::io::stdout().flush().await;
    Ok(())
}

async fn wait_child(child: &mut Child) -> bool {
    match child.wait().await {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}
