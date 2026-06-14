//! Adapter that lets the hub supervise DeepSeek through the same stdin/stdout
//! protocol it already uses for Claude Code.
//!
//! Uses DeepSeek's native tool-calling API to run a real agentic loop:
//! the model calls tools (Bash, Read, Write, Edit, Glob, Grep), sees their
//! results, and keeps working until it has a final answer — matching what
//! Claude Code does.

use anyhow::{bail, Context, Result};
use regex::RegexBuilder;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(clap::Parser)]
struct Args {
    #[arg(long)]
    cwd: PathBuf,
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    system_prompt_file: PathBuf,
    #[arg(long, default_value = "deepseek-chat")]
    model: String,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long, default_value = "")]
    allowed_tools: String,
    /// Maximum tool-call rounds per hub turn (equivalent to --max-turns for Claude).
    #[arg(long, default_value = "50")]
    max_turns: u32,
}

#[derive(Default)]
struct TurnResult {
    final_message: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    failed: bool,
}

struct ApiResponse {
    choice_message: Value,
    prompt_tokens: u64,
    completion_tokens: u64,
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
            "model": &args.model,
        })
    );

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut total_cost = 0.0;
    let mut turns = 0_u64;

    while let Some(line) = lines.next_line().await? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => {
                let prompt = extract_user_text(&v).unwrap_or_default();
                let result = run_turn(&args, &system_prompt, &prompt).await;
                turns += 1;
                match result {
                    Ok(turn) => {
                        total_cost +=
                            estimate_cost(&args.model, turn.prompt_tokens, turn.completion_tokens);
                        println!(
                            "{}",
                            json!({
                                "type": "result",
                                "subtype": if turn.failed { "error" } else { "success" },
                                "is_error": turn.failed,
                                "session_id": args.resume.as_deref().unwrap_or(&args.session_id),
                                "num_turns": turns,
                                "total_cost_usd": total_cost,
                                "result": turn.final_message,
                            })
                        );
                    }
                    Err(e) => {
                        eprintln!("[deepseek-adapter] {e:#}");
                        println!(
                            "{}",
                            json!({
                                "type": "result",
                                "subtype": "error_during_execution",
                                "is_error": true,
                                "session_id": args.resume.as_deref().unwrap_or(&args.session_id),
                                "num_turns": turns,
                                "total_cost_usd": total_cost,
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

async fn run_turn(args: &Args, system_prompt: &str, prompt: &str) -> Result<TurnResult> {
    // Short-circuit for mock testing (preserves existing test behavior).
    if let Ok(text) = std::env::var("MOCK_DEEPSEEK_RESPONSE") {
        if let Ok(cmds) = std::env::var("MOCK_DEEPSEEK_RUN") {
            for cmd in cmds.split(";;").filter(|c| !c.trim().is_empty()) {
                run_shell_command(args, cmd.trim()).await?;
            }
        }
        if !text.trim().is_empty() {
            println!(
                "{}",
                json!({
                    "type": "assistant",
                    "message": { "content": [{ "type": "text", "text": &text }] }
                })
            );
        }
        return Ok(TurnResult {
            final_message: text,
            prompt_tokens: 10,
            completion_tokens: 5,
            failed: false,
        });
    }

    let platform_note = if cfg!(windows) {
        "You are running on Windows. Bash tool commands run via cmd.exe. \
         Use Windows-compatible commands (dir, type, copy, del, move, findstr) \
         rather than Unix commands. Prefer the cross-platform agentcom CLI for \
         coordination (agentcom task list, agentcom send, etc.)."
    } else {
        "You are running on a Unix-like system. Bash tool commands run via /bin/sh."
    };

    let full_system = format!(
        "{system_prompt}\n\n{platform_note}\n\nallowed_tools={}",
        args.allowed_tools
    );

    let tools = build_tool_schemas(&args.allowed_tools);
    let mut messages: Vec<Value> = vec![
        json!({"role": "system", "content": full_system}),
        json!({"role": "user", "content": prompt}),
    ];

    let mut total_prompt_tokens = 0u64;
    let mut total_completion_tokens = 0u64;
    let mut final_message = String::new();
    let mut failed = false;
    let mut tool_rounds = 0u32;

    loop {
        let resp = call_deepseek_api(args, &messages, &tools).await?;
        total_prompt_tokens += resp.prompt_tokens;
        total_completion_tokens += resp.completion_tokens;

        let msg = resp.choice_message;
        let content_str = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let tool_calls = msg
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        // Add assistant message to conversation for next round.
        messages.push(msg.clone());

        // Emit assistant text when the model includes it alongside tool calls
        // or as a final answer.
        if !content_str.trim().is_empty() {
            println!(
                "{}",
                json!({
                    "type": "assistant",
                    "message": { "content": [{ "type": "text", "text": &content_str }] }
                })
            );
            final_message = content_str.clone();
        }

        if tool_calls.is_empty() {
            break;
        }

        tool_rounds += 1;
        if tool_rounds >= args.max_turns {
            eprintln!("[deepseek-adapter] max_turns ({}) reached", args.max_turns);
            break;
        }

        for call in &tool_calls {
            let call_id = call
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("call_0");
            let fn_name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let fn_args_str = call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            let fn_args: Value = serde_json::from_str(fn_args_str).unwrap_or(json!({}));

            // Emit tool_use event so the hub/TUI can display it.
            println!(
                "{}",
                json!({
                    "type": "assistant",
                    "message": {
                        "content": [{
                            "type": "tool_use",
                            "id": call_id,
                            "name": fn_name,
                            "input": &fn_args
                        }]
                    }
                })
            );

            let (result_text, is_error) = match execute_tool(fn_name, &fn_args, args).await {
                Ok(out) => (out, false),
                Err(e) => {
                    failed = true;
                    (format!("Error: {e:#}"), true)
                }
            };

            // Emit tool_result so the hub/TUI can display it.
            println!(
                "{}",
                json!({
                    "type": "user",
                    "message": {
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": &result_text,
                            "is_error": is_error
                        }]
                    }
                })
            );

            // Feed result back into the conversation for the next API call.
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_text
            }));
        }
    }

    Ok(TurnResult {
        final_message,
        prompt_tokens: total_prompt_tokens,
        completion_tokens: total_completion_tokens,
        failed,
    })
}

async fn call_deepseek_api(
    args: &Args,
    messages: &[Value],
    tools: &[Value],
) -> Result<ApiResponse> {
    let api_key = std::env::var("DEEPSEEK_API_KEY").context("DEEPSEEK_API_KEY is not set")?;
    let base_url = std::env::var("DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let mut req = json!({
        "model": &args.model,
        "messages": messages,
    });
    if !tools.is_empty() {
        req["tools"] = json!(tools);
        req["tool_choice"] = json!("auto");
    }

    let mut child = Command::new("curl")
        .arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg(format!("Authorization: Bearer {api_key}"))
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("--data-binary")
        .arg("@-")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning curl for DeepSeek API call")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(req.to_string().as_bytes())
            .await
            .context("writing DeepSeek request to curl stdin")?;
        stdin.shutdown().await.ok();
    }

    let output = child.wait_with_output().await.context("waiting for curl")?;
    if !output.status.success() {
        bail!(
            "curl failed calling DeepSeek API: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let body: Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parsing DeepSeek response JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;

    if let Some(error) = body.get("error") {
        bail!("DeepSeek API returned error: {error}");
    }

    let choice_message = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|ch| ch.first())
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or(json!({"role": "assistant", "content": ""}));

    let usage = body.get("usage").unwrap_or(&Value::Null);
    Ok(ApiResponse {
        choice_message,
        prompt_tokens: token_field(usage, "prompt_tokens"),
        completion_tokens: token_field(usage, "completion_tokens"),
    })
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

async fn execute_tool(name: &str, tool_args: &Value, adapter: &Args) -> Result<String> {
    match name {
        "Bash" => {
            let cmd = tool_args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if !command_allowed(&adapter.allowed_tools, cmd) {
                bail!("command denied by allowed_tools: {cmd}");
            }
            run_shell_command(adapter, cmd).await
        }
        "Read" => {
            require_tool("Read", &adapter.allowed_tools)?;
            let path = tool_args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let offset = tool_args
                .get("offset")
                .and_then(|o| o.as_u64())
                .unwrap_or(0) as usize;
            let limit = tool_args
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|l| l as usize);
            tool_read(path, offset, limit, &adapter.cwd)
        }
        "Write" => {
            require_tool("Write", &adapter.allowed_tools)?;
            let path = tool_args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let content = tool_args
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            tool_write(path, content, &adapter.cwd)
        }
        "Edit" => {
            require_tool("Edit", &adapter.allowed_tools)?;
            let path = tool_args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let old_str = tool_args
                .get("old_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let new_str = tool_args
                .get("new_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let replace_all = tool_args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            tool_edit(path, old_str, new_str, replace_all, &adapter.cwd)
        }
        "Glob" => {
            require_tool("Glob", &adapter.allowed_tools)?;
            let pattern = tool_args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let search_path = tool_args.get("path").and_then(|p| p.as_str());
            tool_glob(pattern, search_path, &adapter.cwd)
        }
        "Grep" => {
            require_tool("Grep", &adapter.allowed_tools)?;
            let pattern = tool_args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let search_path = tool_args.get("path").and_then(|p| p.as_str());
            let glob_filter = tool_args.get("glob").and_then(|g| g.as_str());
            let file_type = tool_args.get("type").and_then(|t| t.as_str());
            let output_mode = tool_args
                .get("output_mode")
                .and_then(|m| m.as_str())
                .unwrap_or("files_with_matches");
            let case_insensitive = tool_args
                .get("-i")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || tool_args
                    .get("case_insensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            let context = tool_args
                .get("context")
                .or_else(|| tool_args.get("-C"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let context_before = tool_args
                .get("-B")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(context);
            let context_after = tool_args
                .get("-A")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(context);
            let head_limit = tool_args
                .get("head_limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(250) as usize;
            let offset = tool_args
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            tool_grep(
                pattern,
                search_path,
                glob_filter,
                file_type,
                output_mode,
                case_insensitive,
                context_before,
                context_after,
                head_limit,
                offset,
                &adapter.cwd,
            )
        }
        other => bail!("unknown tool: {other}"),
    }
}

// ── Tool implementations ──────────────────────────────────────────────────────

fn tool_read(path: &str, offset: usize, limit: Option<usize>, cwd: &Path) -> Result<String> {
    let full = resolve_path(path, cwd);
    let text = std::fs::read_to_string(&full)
        .with_context(|| format!("reading {}", full.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = offset.min(lines.len());
    let end = match limit {
        Some(n) => (start + n).min(lines.len()),
        None => lines.len(),
    };
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(out, "{}\t{}", start + i + 1, line);
    }
    Ok(out)
}

fn tool_write(path: &str, content: &str, cwd: &Path) -> Result<String> {
    let full = resolve_path(path, cwd);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directories for {}", full.display()))?;
    }
    std::fs::write(&full, content)
        .with_context(|| format!("writing {}", full.display()))?;
    Ok(format!("Wrote {} bytes to {}", content.len(), full.display()))
}

fn tool_edit(path: &str, old_str: &str, new_str: &str, replace_all: bool, cwd: &Path) -> Result<String> {
    let full = resolve_path(path, cwd);
    let original = std::fs::read_to_string(&full)
        .with_context(|| format!("reading {}", full.display()))?;
    if !original.contains(old_str) {
        bail!(
            "old_string not found in {} — no changes made",
            full.display()
        );
    }
    let updated = if replace_all {
        original.replace(old_str, new_str)
    } else {
        let count = original.matches(old_str).count();
        if count > 1 {
            bail!(
                "old_string appears {count} times in {} — provide more context to make it unique, or use replace_all=true",
                full.display()
            );
        }
        original.replacen(old_str, new_str, 1)
    };
    std::fs::write(&full, &updated)
        .with_context(|| format!("writing {}", full.display()))?;
    Ok(format!("Edited {}", full.display()))
}

fn tool_glob(pattern: &str, search_path: Option<&str>, cwd: &Path) -> Result<String> {
    let base = match search_path {
        Some(p) => resolve_path(p, cwd),
        None => cwd.to_path_buf(),
    };
    let mut matches: Vec<String> = Vec::new();
    walk_glob(&base, &base, pattern, &mut matches);
    matches.sort();
    if matches.is_empty() {
        return Ok("No files matched.".to_string());
    }
    Ok(matches.join("\n"))
}

fn walk_glob(root: &Path, dir: &Path, pattern: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_glob(root, &path, pattern, out);
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if glob_matches(pattern, &rel_str) {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
}

fn tool_grep(
    pattern: &str,
    search_path: Option<&str>,
    glob_filter: Option<&str>,
    file_type: Option<&str>,
    output_mode: &str,
    case_insensitive: bool,
    context_before: usize,
    context_after: usize,
    head_limit: usize,
    offset: usize,
    cwd: &Path,
) -> Result<String> {
    let re = RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .with_context(|| format!("invalid regex: {pattern}"))?;

    let base = match search_path {
        Some(p) => resolve_path(p, cwd),
        None => cwd.to_path_buf(),
    };

    // 'type' maps to a recursive glob (e.g. "rs" -> "**/*.rs")
    let type_glob = file_type.map(|t| format!("**/*.{t}"));
    let effective_glob: Option<&str> = glob_filter.or_else(|| type_glob.as_deref());

    let mut file_paths: Vec<PathBuf> = Vec::new();
    if base.is_file() {
        file_paths.push(base.clone());
    } else {
        collect_files(&base, &base, effective_glob, &mut file_paths);
    }
    file_paths.sort();

    match output_mode {
        "count" => {
            let mut entries: Vec<String> = Vec::new();
            for path in &file_paths {
                let Ok(text) = std::fs::read_to_string(path) else { continue };
                let count = text.lines().filter(|line| re.is_match(line)).count();
                if count > 0 {
                    let rel = path.strip_prefix(&base).unwrap_or(path);
                    entries.push(format!(
                        "{}:{}",
                        rel.to_string_lossy().replace('\\', "/"),
                        count
                    ));
                }
            }
            if entries.is_empty() {
                return Ok("No matches found.".to_string());
            }
            let result: Vec<String> = entries.into_iter().skip(offset).take(head_limit).collect();
            Ok(result.join("\n"))
        }
        "content" => {
            let mut output_lines: Vec<String> = Vec::new();
            for path in &file_paths {
                let Ok(text) = std::fs::read_to_string(path) else { continue };
                let lines: Vec<&str> = text.lines().collect();
                let rel = path.strip_prefix(&base).unwrap_or(path);
                let rel_str = rel.to_string_lossy().replace('\\', "/");

                let match_indices: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| re.is_match(line))
                    .map(|(i, _)| i)
                    .collect();

                if match_indices.is_empty() {
                    continue;
                }

                // Compute which lines to include (match lines + context).
                let mut included = vec![false; lines.len()];
                let mut is_match_line = vec![false; lines.len()];
                for &m in &match_indices {
                    is_match_line[m] = true;
                    let start = m.saturating_sub(context_before);
                    let end = (m + context_after + 1).min(lines.len());
                    for i in start..end {
                        included[i] = true;
                    }
                }

                let mut prev_included = false;
                for (i, line) in lines.iter().enumerate() {
                    if included[i] {
                        // Emit `--` separator between non-contiguous match groups.
                        if !prev_included && !output_lines.is_empty() {
                            output_lines.push("--".to_string());
                        }
                        if is_match_line[i] {
                            output_lines.push(format!("{}:{}:{}", rel_str, i + 1, line));
                        } else {
                            output_lines.push(format!("{}-{}-{}", rel_str, i + 1, line));
                        }
                        prev_included = true;
                    } else {
                        prev_included = false;
                    }
                }
            }
            if output_lines.is_empty() {
                return Ok("No matches found.".to_string());
            }
            let result: Vec<String> =
                output_lines.into_iter().skip(offset).take(head_limit).collect();
            Ok(result.join("\n"))
        }
        _ => {
            // "files_with_matches" (default)
            let mut matched_files: Vec<String> = Vec::new();
            for path in &file_paths {
                let Ok(text) = std::fs::read_to_string(path) else { continue };
                if text.lines().any(|line| re.is_match(line)) {
                    let rel = path.strip_prefix(&base).unwrap_or(path);
                    matched_files.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
            if matched_files.is_empty() {
                return Ok("No matches found.".to_string());
            }
            let result: Vec<String> =
                matched_files.into_iter().skip(offset).take(head_limit).collect();
            Ok(result.join("\n"))
        }
    }
}

/// Recursively collect files under `dir`, optionally filtered by a glob pattern.
fn collect_files(root: &Path, dir: &Path, glob_filter: Option<&str>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, glob_filter, out);
        } else if let Some(gf) = glob_filter {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if glob_matches(gf, &rel_str) {
                out.push(path);
            }
        } else {
            out.push(path);
        }
    }
}

// ── Glob pattern matching ─────────────────────────────────────────────────────

/// Returns true if `rel_path` (forward-slashed, relative) matches the glob `pattern`.
/// Supports `*` (any chars within one path segment) and `**` (any path segments).
fn glob_matches(pattern: &str, rel_path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = rel_path.split('/').collect();
    glob_match_segments(&pat, &path)
}

fn glob_match_segments(pat: &[&str], path: &[&str]) -> bool {
    match (pat.first(), path.first()) {
        (None, None) => true,
        (None, _) | (_, None) => false,
        (Some(&"**"), _) => {
            // ** can consume zero or more path segments
            glob_match_segments(&pat[1..], path)
                || glob_match_segments(pat, &path[1..])
        }
        (Some(p), Some(s)) => {
            segment_matches(p, s) && glob_match_segments(&pat[1..], &path[1..])
        }
    }
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == segment;
    }
    if !segment.starts_with(parts[0]) {
        return false;
    }
    if !segment.ends_with(parts[parts.len() - 1]) {
        return false;
    }
    let mut pos = parts[0].len();
    let end_skip = if parts[parts.len() - 1].is_empty() { 0 } else { parts[parts.len() - 1].len() };
    let search_in = &segment[..segment.len().saturating_sub(end_skip)];
    for mid in &parts[1..parts.len() - 1] {
        if mid.is_empty() {
            continue;
        }
        match search_in[pos..].find(mid) {
            Some(idx) => pos += idx + mid.len(),
            None => return false,
        }
    }
    true
}

// ── Shell execution ───────────────────────────────────────────────────────────

async fn run_shell_command(args: &Args, command: &str) -> Result<String> {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    let output = cmd
        .current_dir(&args.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("running command {command:?}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        let combined = format!("{stdout}{stderr}").trim().to_string();
        bail!(
            "command failed (exit {:?}): {command}\n{combined}",
            output.status.code()
        );
    }
    Ok(format!("{stdout}{stderr}"))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_path(path: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn require_tool(name: &str, allowed_tools: &str) -> Result<()> {
    if !tool_allowed(name, allowed_tools) {
        bail!("{name} is not in allowed_tools");
    }
    Ok(())
}

fn tool_allowed(name: &str, allowed_tools: &str) -> bool {
    allowed_tools
        .split(',')
        .map(str::trim)
        .any(|t| t == name)
}

fn command_allowed(allowed_tools: &str, command: &str) -> bool {
    let command = command.trim();
    for tool in allowed_tools
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if tool == "Bash" {
            return true;
        }
        let Some(rule) = tool.strip_prefix("Bash(").and_then(|t| t.strip_suffix(')')) else {
            continue;
        };
        if let Some(prefix) = rule.strip_suffix(":*") {
            if command == prefix || command.starts_with(&format!("{prefix} ")) {
                return true;
            }
        } else if command == rule {
            return true;
        }
    }
    false
}

fn token_field(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
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

fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (default_in, default_out) = if model.contains("reasoner") {
        (0.55, 2.19)
    } else {
        (0.27, 1.10)
    };
    let input_per_mtok = std::env::var("AGENTCOM_DEEPSEEK_INPUT_PER_MTOK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default_in);
    let output_per_mtok = std::env::var("AGENTCOM_DEEPSEEK_OUTPUT_PER_MTOK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default_out);
    (input_tokens as f64 / 1_000_000.0) * input_per_mtok
        + (output_tokens as f64 / 1_000_000.0) * output_per_mtok
}

// ── Tool schemas ──────────────────────────────────────────────────────────────

fn build_tool_schemas(allowed_tools: &str) -> Vec<Value> {
    let mut tools = vec![];

    let has_bash = allowed_tools.split(',').any(|t| {
        let t = t.trim();
        t == "Bash" || t.starts_with("Bash(")
    });
    if has_bash {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Bash",
                "description": "Execute a shell command and return its stdout+stderr. \
                    On Windows commands run via cmd.exe; on Unix via /bin/sh. \
                    Use agentcom CLI commands (agentcom task list, agentcom send, etc.) \
                    for hub coordination.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute."
                        },
                        "description": {
                            "type": "string",
                            "description": "Short description of what this command does."
                        }
                    },
                    "required": ["command"]
                }
            }
        }));
    }

    if tool_allowed("Read", allowed_tools) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Read",
                "description": "Read a file and return its contents with line numbers. \
                    Use offset and limit to read a slice of a large file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute or cwd-relative path to the file."
                        },
                        "offset": {
                            "type": "integer",
                            "description": "0-based line number to start reading from."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of lines to return."
                        }
                    },
                    "required": ["file_path"]
                }
            }
        }));
    }

    if tool_allowed("Write", allowed_tools) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Write",
                "description": "Write content to a file, creating it or overwriting it entirely. \
                    Parent directories are created automatically.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute or cwd-relative path to the file."
                        },
                        "content": {
                            "type": "string",
                            "description": "The complete content to write."
                        }
                    },
                    "required": ["file_path", "content"]
                }
            }
        }));
    }

    if tool_allowed("Edit", allowed_tools) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Edit",
                "description": "Replace a string in a file. By default old_string must appear \
                    exactly once — provide enough surrounding context to make it unique. \
                    Set replace_all=true to replace every occurrence. \
                    Prefer Edit over Write for targeted changes to existing files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute or cwd-relative path to the file."
                        },
                        "old_string": {
                            "type": "string",
                            "description": "The exact text to find."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "The text to replace it with."
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "If true, replace all occurrences instead of requiring uniqueness (default false)."
                        }
                    },
                    "required": ["file_path", "old_string", "new_string"]
                }
            }
        }));
    }

    if tool_allowed("Glob", allowed_tools) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Glob",
                "description": "Find files matching a glob pattern. Supports * (within a \
                    path segment) and ** (any number of segments). Returns absolute paths \
                    sorted by name.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob pattern, e.g. \"**/*.rs\" or \"src/**/*.ts\"."
                        },
                        "path": {
                            "type": "string",
                            "description": "Directory to search in (defaults to cwd)."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        }));
    }

    if tool_allowed("Grep", allowed_tools) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "Grep",
                "description": "Search file contents using a regular expression. \
                    output_mode controls the format: 'files_with_matches' (default) lists \
                    matching file paths; 'content' shows matching lines (with optional context \
                    via -A/-B/context); 'count' shows match count per file. \
                    Results are capped at head_limit (default 250) after skipping offset entries.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regular expression to search for (full regex syntax supported)."
                        },
                        "path": {
                            "type": "string",
                            "description": "File or directory to search in (defaults to cwd)."
                        },
                        "glob": {
                            "type": "string",
                            "description": "Only search files matching this glob, e.g. \"**/*.rs\"."
                        },
                        "type": {
                            "type": "string",
                            "description": "File type shorthand, e.g. 'rs', 'ts', 'py'. Equivalent to glob '**/*.{type}'."
                        },
                        "output_mode": {
                            "type": "string",
                            "enum": ["files_with_matches", "content", "count"],
                            "description": "Output format: 'files_with_matches' (default), 'content', or 'count'."
                        },
                        "-i": {
                            "type": "boolean",
                            "description": "Case-insensitive matching (default false)."
                        },
                        "context": {
                            "type": "integer",
                            "description": "Lines of context before and after each match in content mode (equivalent to -C)."
                        },
                        "-A": {
                            "type": "integer",
                            "description": "Lines of context after each match in content mode."
                        },
                        "-B": {
                            "type": "integer",
                            "description": "Lines of context before each match in content mode."
                        },
                        "head_limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default 250)."
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Skip this many results before applying head_limit (default 0)."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        }));
    }

    tools
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_star_ext() {
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("**/*.rs", "src/agent/mod.rs"));
        assert!(!glob_matches("**/*.rs", "src/main.ts"));
    }

    #[test]
    fn glob_prefixed_star_star() {
        assert!(glob_matches("src/**/*.ts", "src/foo/bar.ts"));
        assert!(!glob_matches("src/**/*.ts", "lib/foo/bar.ts"));
    }

    #[test]
    fn glob_simple_star() {
        assert!(glob_matches("*.json", "package.json"));
        assert!(!glob_matches("*.json", "src/package.json"));
    }

    #[test]
    fn bash_allowed_tool_rules() {
        assert!(command_allowed("Bash(agentcom:*)", "agentcom task list"));
        assert!(!command_allowed("Bash(agentcom:*)", "cargo test"));
        assert!(command_allowed("Bash", "cargo test"));
    }

    #[test]
    fn tool_schemas_filtered_by_allowed() {
        let schemas = build_tool_schemas("Bash,Read");
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("function")?.get("name")?.as_str())
            .collect();
        assert_eq!(names, vec!["Bash", "Read"]);
    }

    #[test]
    fn tool_edit_rejects_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "foo\nfoo\n").unwrap();
        let rel = path.file_name().unwrap().to_str().unwrap();
        assert!(tool_edit(rel, "foo", "bar", false, dir.path()).is_err());
    }

    #[test]
    fn tool_edit_applies_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        let rel = path.file_name().unwrap().to_str().unwrap();
        tool_edit(rel, "world", "rust", false, dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust\n");
    }

    #[test]
    fn tool_edit_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "foo\nfoo\nbar\n").unwrap();
        let rel = path.file_name().unwrap().to_str().unwrap();
        tool_edit(rel, "foo", "baz", true, dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "baz\nbaz\nbar\n");
    }

    #[test]
    fn tool_grep_regex_files_with_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "struct Foo;\n").unwrap();
        let result = tool_grep("fn ", None, None, None, "files_with_matches", false, 0, 0, 250, 0, dir.path()).unwrap();
        assert!(result.contains("a.rs"));
        assert!(!result.contains("b.rs"));
    }

    #[test]
    fn tool_grep_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "Hello World\n").unwrap();
        let sensitive = tool_grep("hello", None, None, None, "files_with_matches", false, 0, 0, 250, 0, dir.path()).unwrap();
        let insensitive = tool_grep("hello", None, None, None, "files_with_matches", true, 0, 0, 250, 0, dir.path()).unwrap();
        assert_eq!(sensitive, "No matches found.");
        assert!(insensitive.contains("a.txt"));
    }

    #[test]
    fn tool_grep_count_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "foo\nfoo\nbar\n").unwrap();
        let result = tool_grep("foo", None, None, None, "count", false, 0, 0, 250, 0, dir.path()).unwrap();
        assert!(result.contains(":2"));
    }

    #[test]
    fn tool_grep_content_mode_with_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line1\nMATCH\nline3\n").unwrap();
        let result = tool_grep("MATCH", None, None, None, "content", false, 1, 1, 250, 0, dir.path()).unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("MATCH"));
        assert!(result.contains("line3"));
    }

    #[test]
    fn tool_grep_type_filter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "fn foo() {}\n").unwrap();
        let result = tool_grep("fn foo", None, None, Some("rs"), "files_with_matches", false, 0, 0, 250, 0, dir.path()).unwrap();
        assert!(result.contains("a.rs"));
        assert!(!result.contains("b.txt"));
    }

    #[test]
    fn tool_grep_head_limit_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10u8 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "match\n").unwrap();
        }
        let all = tool_grep("match", None, None, None, "files_with_matches", false, 0, 0, 250, 0, dir.path()).unwrap();
        let limited = tool_grep("match", None, None, None, "files_with_matches", false, 0, 0, 3, 0, dir.path()).unwrap();
        let offset = tool_grep("match", None, None, None, "files_with_matches", false, 0, 0, 3, 2, dir.path()).unwrap();
        assert_eq!(all.lines().count(), 10);
        assert_eq!(limited.lines().count(), 3);
        assert_eq!(offset.lines().count(), 3);
        assert_ne!(limited, offset);
    }
}
