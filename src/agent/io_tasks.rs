//! The four tokio tasks attached to every child process:
//! stdout reader, stderr reader, stdin writer, and exit waiter.
//!
//! Readers are always draining (preventing stdio deadlock) and double as the
//! TUI feed: every displayable event lands in the agent's ring buffer before
//! being forwarded to the hub loop.

use super::WriterCmd;
use crate::hub::events::{HubEvent, UiEvent};
use crate::protocol::event::{
    parse_line, stream_block_end, stream_text_delta, CliEvent, ContentBlock, ParsedLine,
};
use crate::tui::ringbuf::SharedRingBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc};

pub struct IoHandles {
    pub stdin_tx: mpsc::Sender<WriterCmd>,
    pub pid: Option<u32>,
}

pub fn attach(
    agent: String,
    mut child: Child,
    buf: SharedRingBuf,
    bus_tx: mpsc::Sender<HubEvent>,
    ui_tx: broadcast::Sender<UiEvent>,
) -> IoHandles {
    let pid = child.id();
    let stdout = child.stdout.take().expect("child stdout piped");
    let stderr = child.stderr.take().expect("child stderr piped");
    let mut stdin = child.stdin.take().expect("child stdin piped");

    // --- stdin writer (sole owner of ChildStdin) ---
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<WriterCmd>(64);
    {
        let agent = agent.clone();
        tokio::spawn(async move {
            while let Some(cmd) = stdin_rx.recv().await {
                match cmd {
                    WriterCmd::Line(mut line) => {
                        line.push('\n');
                        if let Err(e) = stdin.write_all(line.as_bytes()).await {
                            tracing::warn!(agent = %agent, error = %e, "stdin write failed");
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                    WriterCmd::Close => break,
                }
            }
            // Dropping stdin closes the pipe — the child's graceful-exit signal.
        });
    }

    // --- stdout reader ---
    {
        let agent = agent.clone();
        let buf = buf.clone();
        let bus_tx = bus_tx.clone();
        let ui_tx = ui_tx.clone();
        // AGENTCOM_CAPTURE_RAW=<dir>: tee every raw stdout line to
        // <dir>/<agent>.ndjson (protocol fixture capture / debugging).
        let mut capture = std::env::var("AGENTCOM_CAPTURE_RAW").ok().and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::path::Path::new(&dir).join(format!("{agent}.ndjson")))
                .ok()
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if let Some(f) = capture.as_mut() {
                            use std::io::Write;
                            let _ = writeln!(f, "{line}");
                        }
                        handle_stdout_line(&agent, &line, &buf, &bus_tx, &ui_tx).await;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(agent = %agent, error = %e, "stdout read error");
                        break;
                    }
                }
            }
        });
    }

    // --- stderr reader ---
    {
        let agent = agent.clone();
        let buf = buf.clone();
        let bus_tx = bus_tx.clone();
        let ui_tx = ui_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let display = format!("[stderr] {line}");
                buf.write().unwrap().push_line(display.clone());
                let _ = ui_tx.send(UiEvent::AgentLine {
                    agent: agent.clone(),
                    line: display,
                });
                let _ = bus_tx
                    .send(HubEvent::Stderr {
                        agent: agent.clone(),
                        line,
                    })
                    .await;
            }
        });
    }

    // --- exit waiter ---
    {
        let agent = agent.clone();
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(status) => status.code(),
                Err(e) => {
                    tracing::warn!(agent = %agent, error = %e, "wait() failed");
                    None
                }
            };
            let _ = bus_tx.send(HubEvent::Exited { agent, code }).await;
        });
    }

    IoHandles { stdin_tx, pid }
}

async fn handle_stdout_line(
    agent: &str,
    line: &str,
    buf: &SharedRingBuf,
    bus_tx: &mpsc::Sender<HubEvent>,
    ui_tx: &broadcast::Sender<UiEvent>,
) {
    match parse_line(line) {
        ParsedLine::Event(event) => {
            render_event(agent, &event, buf, ui_tx);
            let _ = bus_tx
                .send(HubEvent::Cli {
                    agent: agent.to_string(),
                    event,
                })
                .await;
        }
        ParsedLine::Raw(raw) => {
            buf.write().unwrap().push_line(format!("[raw] {raw}"));
            let _ = bus_tx
                .send(HubEvent::CliRaw {
                    agent: agent.to_string(),
                    line: raw,
                })
                .await;
        }
    }
}

/// Turn a CLI event into human-readable output-pane lines.
fn render_event(
    agent: &str,
    event: &CliEvent,
    buf: &SharedRingBuf,
    ui_tx: &broadcast::Sender<UiEvent>,
) {
    let push = |line: String| {
        buf.write().unwrap().push_line(line.clone());
        let _ = ui_tx.send(UiEvent::AgentLine {
            agent: agent.to_string(),
            line,
        });
    };

    match event {
        CliEvent::System { subtype, model, .. } => {
            push(format!(
                "[session] {subtype}{}",
                model
                    .as_deref()
                    .map(|m| format!(" · model {m}"))
                    .unwrap_or_default()
            ));
        }
        CliEvent::Assistant { message } => {
            for block in &message.content {
                match block {
                    // With partial messages enabled, text streams in via
                    // deltas; the trailing full text block then duplicates it,
                    // so we seal the streamed line instead of reprinting.
                    // Providers that never stream (DeepSeek, Codex) leave
                    // `saw_delta` false, so their full text always renders.
                    ContentBlock::Text { text } => {
                        if buf.write().unwrap().take_saw_delta() {
                            buf.write().unwrap().close_line();
                        } else {
                            for l in text.lines() {
                                push(l.to_string());
                            }
                        }
                    }
                    ContentBlock::ToolUse { name, input } => {
                        let summary = tool_summary(name, input);
                        push(format!("[tool] {summary}"));
                    }
                    ContentBlock::Thinking { .. } | ContentBlock::Other => {}
                }
            }
        }
        CliEvent::Result {
            subtype,
            total_cost_usd,
            is_error,
            ..
        } => {
            push(format!(
                "[turn end] {subtype}{}{}",
                if *is_error { " (error)" } else { "" },
                total_cost_usd
                    .map(|c| format!(" · ${c:.4}"))
                    .unwrap_or_default()
            ));
        }
        CliEvent::StreamEvent { event } => {
            if let Some(text) = stream_text_delta(event) {
                buf.write().unwrap().push_delta(text);
                let _ = ui_tx.send(UiEvent::AgentDelta);
            } else if stream_block_end(event) {
                buf.write().unwrap().close_line();
            }
        }
        CliEvent::ControlResponse { .. }
        | CliEvent::User { .. }
        | CliEvent::RateLimitEvent { .. }
        | CliEvent::Unknown => {}
    }
}

fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    let detail = match name {
        "Bash" => input.get("command").and_then(|v| v.as_str()),
        "Read" | "Write" | "Edit" => input.get("file_path").and_then(|v| v.as_str()),
        "Glob" | "Grep" => input.get("pattern").and_then(|v| v.as_str()),
        _ => None,
    };
    match detail {
        Some(d) => {
            let d: String = d.chars().take(120).collect();
            format!("{name}: {d}")
        }
        None => name.to_string(),
    }
}
