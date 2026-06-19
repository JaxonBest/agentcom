//! Slash-command parser and dispatcher. Commands map 1:1 onto existing
//! `ipc::Request` variants (no protocol change) or onto local UI actions.
//!
//! `/spawn` is intentionally omitted: there is no clean `Request` for it
//! (only `AgentAdd { config: Box<AgentConfig> }`, which needs a full config the
//! chat box can't author inline). It belongs to a later worktree/dispatch
//! workstream.

use super::transcript::TranscriptItem;
use super::ChatState;
use crate::ipc::{Request, Response};

/// A parsed slash command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// List known agents and their state (local render, no request).
    Agents,
    /// Task-board subcommand.
    Task(TaskSub),
    /// `/msg <agent> <body>` — send a message to an agent. `urgent` set by `/msg!`.
    Msg {
        agent: String,
        body: String,
        urgent: bool,
    },
    /// `/pause [agent|all]` — defaults to all.
    Pause(String),
    /// `/resume [agent|all]` — defaults to all.
    Resume(String),
    /// `/stop [agent]` — `None` stops the whole fleet (and quits).
    Stop(Option<String>),
    /// `/output <agent>` — dump the tail of an agent's output buffer.
    Output(String),
    /// Toggle the help overlay.
    Help,
    /// Clear the transcript.
    Clear,
    /// Quit (stops the fleet).
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskSub {
    Add { title: String },
    List { status: Option<String> },
    Done { id: i64 },
    Claim { id: i64 },
}

/// The slash-command set, surfaced in the help overlay.
pub const SLASH_HELP: &[(&str, &str)] = &[
    ("/agents", "list agents and their state"),
    ("/task add <title>", "add a task to the board"),
    ("/task list [status]", "list tasks (optionally by status)"),
    ("/task done <id>", "mark a task done"),
    ("/task claim <id>", "claim a task"),
    ("/msg <agent> <body>", "send a message to an agent"),
    ("/msg! <agent> <body>", "send an urgent (interrupting) message"),
    ("/pause [agent|all]", "pause an agent or the whole fleet"),
    ("/resume [agent|all]", "resume an agent or the whole fleet"),
    ("/stop [agent]", "stop an agent, or the fleet if omitted"),
    ("/output <agent>", "show recent output for an agent"),
    ("/clear", "clear the transcript"),
    ("/help", "toggle this help"),
    ("/quit", "stop the fleet and exit"),
];

/// Command names for Tab autocomplete.
const COMMAND_NAMES: &[&str] = &[
    "/agents", "/task", "/msg", "/msg!", "/pause", "/resume", "/stop", "/output", "/clear",
    "/help", "/quit",
];

/// Suggest commands whose name starts with `prefix` (which includes the `/`).
pub fn complete(prefix: &str) -> Vec<&'static str> {
    COMMAND_NAMES
        .iter()
        .filter(|c| c.starts_with(prefix))
        .copied()
        .collect()
}

/// Parse a raw input line (leading `/` included) into a [`Command`].
pub fn parse(raw: &str) -> Result<Command, String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix('/').unwrap_or(raw);
    let mut parts = raw.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match name {
        "agents" => Ok(Command::Agents),
        "help" | "?" => Ok(Command::Help),
        "clear" => Ok(Command::Clear),
        "quit" | "q" | "exit" => Ok(Command::Quit),
        "task" => parse_task(rest),
        "msg" | "msg!" => {
            let urgent = name == "msg!";
            let mut a = rest.splitn(2, char::is_whitespace);
            let agent = a.next().unwrap_or("").trim();
            let body = a.next().unwrap_or("").trim();
            if agent.is_empty() {
                return Err("usage: /msg <agent> <body>".into());
            }
            if body.is_empty() {
                return Err("usage: /msg <agent> <body> (body is empty)".into());
            }
            Ok(Command::Msg {
                agent: agent.to_string(),
                body: body.to_string(),
                urgent,
            })
        }
        "pause" => Ok(Command::Pause(default_target(rest))),
        "resume" => Ok(Command::Resume(default_target(rest))),
        "stop" => Ok(Command::Stop(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        })),
        "output" => {
            if rest.is_empty() {
                Err("usage: /output <agent>".into())
            } else {
                Ok(Command::Output(rest.to_string()))
            }
        }
        other => Err(unknown_suggestion(other)),
    }
}

fn default_target(rest: &str) -> String {
    if rest.is_empty() {
        "all".to_string()
    } else {
        rest.to_string()
    }
}

fn parse_task(rest: &str) -> Result<Command, String> {
    let mut parts = rest.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();
    match sub {
        "add" => {
            if arg.is_empty() {
                Err("usage: /task add <title>".into())
            } else {
                Ok(Command::Task(TaskSub::Add {
                    title: arg.to_string(),
                }))
            }
        }
        "list" | "" => Ok(Command::Task(TaskSub::List {
            status: if arg.is_empty() {
                None
            } else {
                Some(arg.to_string())
            },
        })),
        "done" => parse_id(arg).map(|id| Command::Task(TaskSub::Done { id })),
        "claim" => parse_id(arg).map(|id| Command::Task(TaskSub::Claim { id })),
        other => Err(format!(
            "unknown task subcommand '{other}' — try add/list/done/claim"
        )),
    }
}

fn parse_id(arg: &str) -> Result<i64, String> {
    let arg = arg.trim().trim_start_matches('#');
    arg.parse::<i64>()
        .map_err(|_| format!("expected a task id, got '{arg}'"))
}

fn unknown_suggestion(name: &str) -> String {
    let candidate = format!("/{name}");
    match complete(&candidate).first() {
        Some(s) => format!("unknown command '/{name}' — did you mean {s}? (/help for list)"),
        None => format!("unknown command '/{name}' (/help for list)"),
    }
}

/// Map a command to the `ipc::Request` it sends, if any. Local-only commands
/// (Agents, Output, Help, Clear) return `None`. Kept pure so the parser tests
/// can assert the request shape without a running hub.
pub fn cmd_to_request(cmd: &Command) -> Option<Request> {
    match cmd {
        Command::Agents | Command::Output(_) | Command::Help | Command::Clear => None,
        Command::Task(TaskSub::Add { title }) => Some(Request::TaskAdd {
            title: title.clone(),
            description: String::new(),
            priority: 2,
            depends_on: vec![],
            timeout_mins: None,
            requires: vec![],
            recur: None,
        }),
        Command::Task(TaskSub::List { status }) => Some(Request::TaskList {
            status: status.clone(),
            search: None,
            tag: None,
        }),
        Command::Task(TaskSub::Done { id }) => Some(Request::TaskDone {
            id: *id,
            note: None,
        }),
        Command::Task(TaskSub::Claim { id }) => Some(Request::TaskClaim { id: *id }),
        Command::Msg {
            agent,
            body,
            urgent,
        } => Some(Request::Send {
            to: agent.clone(),
            body: body.clone(),
            urgent: *urgent,
        }),
        Command::Pause(agent) => Some(Request::Pause {
            agent: agent.clone(),
        }),
        Command::Resume(agent) => Some(Request::Resume {
            agent: agent.clone(),
        }),
        Command::Stop(agent) => Some(Request::Stop {
            agent: agent.clone(),
        }),
        // /quit reuses the existing should_quit -> Stop{None} path in the loop.
        Command::Quit => None,
    }
}

/// Execute a command against the chat state: either fire an `ipc::Request`
/// (with the reply routed back into the transcript as a System line) or perform
/// a local action.
pub fn exec(cmd: Command, st: &mut ChatState) {
    match cmd {
        Command::Help => st.show_help = !st.show_help,
        Command::Quit => st.should_quit = true,
        Command::Clear => {
            st.transcript.clear();
            st.scroll.follow = true;
            st.scroll.offset = 0;
        }
        Command::Agents => {
            if st.agents.is_empty() {
                st.push_item(TranscriptItem::System {
                    body: "no agents".into(),
                });
            } else {
                for a in &st.agents {
                    st.cmd_result_tx
                        .send(TranscriptItem::System {
                            body: format!(
                                "{:<12} {:<10} [{}] ${:.2} {}t{}",
                                a.name,
                                a.state,
                                a.provider,
                                a.spent_usd,
                                a.turns,
                                a.detail
                                    .as_deref()
                                    .map(|d| format!("  {d}"))
                                    .unwrap_or_default(),
                            ),
                        })
                        .ok();
                }
            }
        }
        Command::Output(agent) => {
            let lines: Vec<String> = {
                let map = st.buffers.read().unwrap();
                match map.get(&agent) {
                    Some(buf) => buf.read().unwrap().tail(40),
                    None => vec![],
                }
            };
            if lines.is_empty() {
                st.push_item(TranscriptItem::System {
                    body: format!("no output for '{agent}'"),
                });
            } else {
                st.push_item(TranscriptItem::System {
                    body: format!("── output: {agent} (last {} lines) ──", lines.len()),
                });
                for l in lines {
                    st.push_item(TranscriptItem::Agent {
                        name: agent.clone(),
                        body: l,
                    });
                }
            }
        }
        ref c @ Command::Task(TaskSub::List { .. }) => {
            // List needs the reply rendered, so route the Response back.
            if let Some(req) = cmd_to_request(c) {
                dispatch_with_reply(st, req, render_task_list);
            }
        }
        other => {
            // Fire-and-acknowledge commands: send the request, surface Ok/Err.
            if let Some(req) = cmd_to_request(&other) {
                let label = ack_label(&other);
                dispatch_with_reply(st, req, move |resp| ack_lines(&label, resp));
            }
        }
    }
}

/// A short human label for the acknowledgement of a fire-and-forget command.
fn ack_label(cmd: &Command) -> String {
    match cmd {
        Command::Task(TaskSub::Add { title }) => format!("task added: {title}"),
        Command::Task(TaskSub::Done { id }) => format!("task #{id} done"),
        Command::Task(TaskSub::Claim { id }) => format!("claimed task #{id}"),
        Command::Msg { agent, urgent, .. } => {
            if *urgent {
                format!("interrupted {agent}")
            } else {
                format!("messaged {agent}")
            }
        }
        Command::Pause(a) => format!("paused {a}"),
        Command::Resume(a) => format!("resumed {a}"),
        Command::Stop(Some(a)) => format!("stopping {a}"),
        Command::Stop(None) => "stopping fleet".into(),
        _ => "ok".into(),
    }
}

/// Turn an `Ok`/`Err` response into transcript lines, preferring the success
/// label but always surfacing an error message.
fn ack_lines(label: &str, resp: Response) -> Vec<TranscriptItem> {
    match resp {
        Response::Ok { message } => vec![TranscriptItem::System {
            body: message.unwrap_or_else(|| label.to_string()),
        }],
        Response::Err { message } => vec![TranscriptItem::System {
            body: format!("error: {message}"),
        }],
        // Any other response to a fire-and-forget command is unexpected; show
        // the optimistic label rather than nothing.
        _ => vec![TranscriptItem::System {
            body: label.to_string(),
        }],
    }
}

/// Render a `Response::Tasks` reply into a compact System listing.
fn render_task_list(resp: Response) -> Vec<TranscriptItem> {
    match resp {
        Response::Tasks { tasks } => {
            if tasks.is_empty() {
                return vec![TranscriptItem::System {
                    body: "no tasks".into(),
                }];
            }
            let mut out = vec![TranscriptItem::System {
                body: format!("── tasks ({}) ──", tasks.len()),
            }];
            for t in tasks {
                out.push(TranscriptItem::System {
                    body: format!(
                        "#{:<4} p{} {:<14} {:<10} {}",
                        t.id,
                        t.priority,
                        t.status.as_str(),
                        t.claimed_by.unwrap_or_default(),
                        t.title,
                    ),
                });
            }
            out
        }
        Response::Err { message } => vec![TranscriptItem::System {
            body: format!("error: {message}"),
        }],
        _ => vec![TranscriptItem::System {
            body: "unexpected response to /task list".into(),
        }],
    }
}

/// Fire `req` at the hub on a background task and route the rendered reply back
/// into the transcript via the command-result channel. Keeps `exec` free of any
/// `&mut ChatState` borrow held across `.await`.
fn dispatch_with_reply<F>(st: &ChatState, req: Request, render: F)
where
    F: FnOnce(Response) -> Vec<TranscriptItem> + Send + 'static,
{
    let tx = st.ipc_tx.clone();
    let result_tx = st.cmd_result_tx.clone();
    tokio::spawn(async move {
        match super::request(&tx, req).await {
            Ok(resp) => {
                for item in render(resp) {
                    let _ = result_tx.send(item);
                }
            }
            Err(e) => {
                let _ = result_tx.send(TranscriptItem::System {
                    body: format!("error: {e}"),
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_add() {
        assert_eq!(
            parse("/task add fix bug"),
            Ok(Command::Task(TaskSub::Add {
                title: "fix bug".into()
            }))
        );
        // `Request` doesn't derive PartialEq (it's a cross-module wire type),
        // so assert the mapped request's shape by pattern instead.
        match cmd_to_request(&parse("/task add fix bug").unwrap()) {
            Some(Request::TaskAdd {
                title,
                description,
                priority,
                depends_on,
                timeout_mins,
                requires,
                recur,
            }) => {
                assert_eq!(title, "fix bug");
                assert_eq!(description, "");
                assert_eq!(priority, 2);
                assert!(depends_on.is_empty());
                assert!(timeout_mins.is_none());
                assert!(requires.is_empty());
                assert!(recur.is_none());
            }
            other => panic!("expected TaskAdd, got {other:?}"),
        }
    }

    #[test]
    fn parse_task_list_with_and_without_status() {
        assert_eq!(
            parse("/task list"),
            Ok(Command::Task(TaskSub::List { status: None }))
        );
        assert_eq!(
            parse("/task list open"),
            Ok(Command::Task(TaskSub::List {
                status: Some("open".into())
            }))
        );
        // bare /task defaults to list
        assert_eq!(
            parse("/task"),
            Ok(Command::Task(TaskSub::List { status: None }))
        );
    }

    #[test]
    fn parse_task_done_and_claim_accept_hash() {
        assert_eq!(
            parse("/task done #7"),
            Ok(Command::Task(TaskSub::Done { id: 7 }))
        );
        assert_eq!(
            parse("/task claim 3"),
            Ok(Command::Task(TaskSub::Claim { id: 3 }))
        );
        assert!(parse("/task done abc").is_err());
    }

    #[test]
    fn parse_msg_and_urgent() {
        assert_eq!(
            parse("/msg worker hello there"),
            Ok(Command::Msg {
                agent: "worker".into(),
                body: "hello there".into(),
                urgent: false,
            })
        );
        assert_eq!(
            parse("/msg! worker stop now"),
            Ok(Command::Msg {
                agent: "worker".into(),
                body: "stop now".into(),
                urgent: true,
            })
        );
        assert!(parse("/msg worker").is_err());
    }

    #[test]
    fn parse_pause_resume_default_all() {
        assert_eq!(parse("/pause"), Ok(Command::Pause("all".into())));
        assert_eq!(parse("/pause worker"), Ok(Command::Pause("worker".into())));
        assert_eq!(parse("/resume"), Ok(Command::Resume("all".into())));
        match cmd_to_request(&parse("/pause").unwrap()) {
            Some(Request::Pause { agent }) => assert_eq!(agent, "all"),
            other => panic!("expected Pause, got {other:?}"),
        }
    }

    #[test]
    fn parse_stop() {
        assert_eq!(parse("/stop"), Ok(Command::Stop(None)));
        assert_eq!(parse("/stop worker"), Ok(Command::Stop(Some("worker".into()))));
        match cmd_to_request(&parse("/stop worker").unwrap()) {
            Some(Request::Stop { agent }) => assert_eq!(agent.as_deref(), Some("worker")),
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn parse_locals() {
        assert_eq!(parse("/agents"), Ok(Command::Agents));
        assert_eq!(parse("/help"), Ok(Command::Help));
        assert_eq!(parse("/clear"), Ok(Command::Clear));
        assert_eq!(parse("/quit"), Ok(Command::Quit));
        assert_eq!(parse("/output worker"), Ok(Command::Output("worker".into())));
        assert!(parse("/output").is_err());
    }

    #[test]
    fn parse_unknown_suggests() {
        let err = parse("/bogus").unwrap_err();
        assert!(err.contains("unknown command"), "got: {err}");
        // Near-miss returns a suggestion.
        let err = parse("/quti").unwrap_err();
        assert!(err.contains("unknown command"), "got: {err}");
        let err = parse("/qu" ).unwrap_err();
        assert!(err.contains("/quit"), "expected /quit suggestion, got: {err}");
    }

    #[test]
    fn complete_prefixes() {
        assert!(complete("/q").contains(&"/quit"));
        assert!(complete("/ms").contains(&"/msg"));
        assert!(complete("/ms").contains(&"/msg!"));
        assert!(complete("/zzz").is_empty());
    }

    #[test]
    fn quit_maps_to_no_request() {
        // /quit drives should_quit, which the loop turns into Stop{None}.
        assert!(cmd_to_request(&Command::Quit).is_none());
    }

    #[test]
    fn msg_maps_to_send() {
        let cmd = parse("/msg! worker stop").unwrap();
        match cmd_to_request(&cmd) {
            Some(Request::Send { to, body, urgent }) => {
                assert_eq!(to, "worker");
                assert_eq!(body, "stop");
                assert!(urgent);
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}
