//! The chat TUI. Runs in the hub process as "just another client": all
//! actions go through the same `IpcMsg` channel the TCP server uses, live
//! output is read straight from the shared ring buffers, and board/feed
//! data comes from read-only store queries.
//!
//! A single full-height transcript interleaves human turns, composer replies,
//! and a compact fleet-activity stream. A persistent bottom input box (a real
//! line editor) sends to the composer or runs slash commands.

pub mod command;
pub mod input;
pub mod ringbuf;
pub mod theme;
pub mod transcript;
pub mod ui;

use crate::hub::events::{IpcMsg, UiEvent};
use crate::ipc::server::Buffers;
use crate::ipc::{AgentStatusRow, Request, Response};
use crate::store::Store;
use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use transcript::TranscriptItem;
use tui_textarea::TextArea;

/// Transcript scroll position. `follow` pins to the bottom (live mode); any
/// scroll-up clears it and freezes `offset` (lines from the top of the window).
#[derive(Debug, Default, Clone, Copy)]
pub struct ScrollState {
    /// First visible transcript line (top of the viewport). Only meaningful
    /// when `!follow`.
    pub offset: usize,
    /// When true, the viewport tracks the newest line.
    pub follow: bool,
}

/// All TUI state. Replaces the old multi-tab `App` god-struct.
pub struct ChatState {
    pub project: String,
    /// Model label shown in the status bar (provider of the composer, or the
    /// dominant provider in the fleet).
    pub model_label: String,
    pub transcript: Vec<TranscriptItem>,
    pub scroll: ScrollState,
    /// The bottom input editor.
    pub input: TextArea<'static>,
    /// Input history (most recent last) and the current recall cursor.
    pub history: Vec<String>,
    pub hist_idx: Option<usize>,
    pub agents: Vec<AgentStatusRow>,
    pub open_tasks: u64,
    pub total_cost: f64,
    /// Free-mode summary (e.g. "95m left") from hub status, or None.
    pub free_mode: Option<String>,
    /// Animation frame counter, bumped on every tick (drives the working glyph).
    pub spin: usize,
    pub should_quit: bool,
    pub confirm_quit: bool,
    pub show_help: bool,
    /// Shared per-agent output ring buffers (read for `/output`).
    pub buffers: Buffers,
    ipc_tx: mpsc::Sender<IpcMsg>,
    /// Background command replies land here and are drained into the transcript.
    cmd_result_tx: mpsc::UnboundedSender<TranscriptItem>,
}

impl ChatState {
    /// Fire a request at the hub, discarding the reply (for the quit path and
    /// optimistic sends where the round-trip shows up via `MessagesChanged`).
    pub fn send_request(&self, req: Request) {
        let tx = self.ipc_tx.clone();
        tokio::spawn(async move {
            let _ = request(&tx, req).await;
        });
    }

    /// Append an item to the transcript (trimming the ring) and keep the
    /// viewport pinned to the bottom when following.
    pub fn push_item(&mut self, item: TranscriptItem) {
        transcript::push(&mut self.transcript, item);
    }

    fn fresh_input() -> TextArea<'static> {
        let mut ta = TextArea::default();
        ta.set_cursor_line_style(ratatui::style::Style::default());
        ta.set_placeholder_text("Message the composer, or /help for commands");
        ta
    }

    /// Minimal state for unit tests (no live hub). The dropped receivers mean
    /// background sends silently no-op, which is fine for key-handling tests.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        use std::collections::HashMap;
        use std::sync::RwLock;
        let (ipc_tx, _ipc_rx) = mpsc::channel(8);
        let (cmd_result_tx, _cmd_rx) = mpsc::unbounded_channel();
        ChatState {
            project: "test".into(),
            model_label: "test".into(),
            transcript: Vec::new(),
            scroll: ScrollState {
                offset: 0,
                follow: true,
            },
            input: ChatState::fresh_input(),
            history: Vec::new(),
            hist_idx: None,
            agents: Vec::new(),
            open_tasks: 0,
            total_cost: 0.0,
            free_mode: None,
            spin: 0,
            should_quit: false,
            confirm_quit: false,
            show_help: false,
            buffers: Arc::new(RwLock::new(HashMap::new())),
            ipc_tx,
            cmd_result_tx,
        }
    }
}

/// Per-process session identity for the embedded TUI. Set once at startup so
/// this TUI registers as its own `human:tui-<uuid>` session, distinct from any
/// other terminal — which is what keeps multi-session inbox delivery correct
/// (no two sessions cannibalize each other's messages).
static TUI_SESSION_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub async fn request(tx: &mpsc::Sender<IpcMsg>, req: Request) -> Result<Response> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let identity = TUI_SESSION_ID
        .get()
        .cloned()
        .unwrap_or_else(|| "human".to_string());
    tx.send(IpcMsg {
        identity,
        request: req,
        reply: reply_tx,
    })
    .await
    .map_err(|_| anyhow::anyhow!("hub gone"))?;
    Ok(reply_rx.await?)
}

pub async fn run(
    project: String,
    agent_names: Vec<String>,
    ipc_tx: mpsc::Sender<IpcMsg>,
    ui_tx: broadcast::Sender<UiEvent>,
    buffers: Buffers,
    store: Arc<Store>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(
        &mut terminal,
        project,
        agent_names,
        ipc_tx,
        ui_tx,
        buffers,
        store,
    )
    .await;
    restore_terminal();
    // In TUI mode stderr is the alternate screen, so an error returned from
    // the loop (e.g. a failed `terminal.draw`) would otherwise vanish — log
    // it to the hub file so the exit reason survives.
    if let Err(e) = &result {
        tracing::error!("TUI loop exited with error: {e:#}");
    }
    result
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> Result<Term> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    // Both the panic hook and explicit restore put the terminal back —
    // raw mode left enabled is the classic ratatui-on-Windows footgun.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore first, so the panic text isn't drawn into the alternate
        // screen and lost on exit.
        restore_terminal();
        // Persist the panic to the hub log. Without this a render-path panic
        // dies to the terminal with no trace (stderr is the alt-screen).
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(location = %location, "TUI panicked: {payload}\nbacktrace:\n{backtrace}");
        original_hook(info);
    }));
    Ok(Terminal::new(CrosstermBackend::new(std::io::stdout()))?)
}

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
}

async fn run_loop(
    terminal: &mut Term,
    project: String,
    agent_names: Vec<String>,
    ipc_tx: mpsc::Sender<IpcMsg>,
    ui_tx: broadcast::Sender<UiEvent>,
    buffers: Buffers,
    store: Arc<Store>,
) -> Result<()> {
    let (cmd_result_tx, mut cmd_result_rx) = mpsc::unbounded_channel::<TranscriptItem>();

    // This TUI registers as its own session so concurrent terminals/TUIs each
    // read the shared `human` mailbox independently (set once; ignore re-set).
    let _ = TUI_SESSION_ID.set(format!("human:tui-{}", uuid::Uuid::new_v4()));

    // Hydrate scrollback from the DURABLE transcript so the conversation and
    // fleet activity survive a hub restart (the ring buffers do not). Live
    // updates after attach arrive via the UiEvent stream below.
    let initial = store
        .transcript_tail(transcript::TRANSCRIPT_CAP)
        .unwrap_or_default();
    let transcript = transcript::hydrate_from_transcript(&initial);
    // Baseline for the live `MessagesChanged` path: only surface messages
    // newer than what the hydrated transcript already shows.
    let mut last_msg_id = store
        .msg_recent(1)
        .ok()
        .and_then(|v| v.last().map(|m| m.id))
        .unwrap_or(0);

    let mut st = ChatState {
        project,
        model_label: provider_summary(&[]),
        transcript,
        scroll: ScrollState {
            offset: 0,
            follow: true,
        },
        input: ChatState::fresh_input(),
        history: Vec::new(),
        hist_idx: None,
        agents: Vec::new(),
        open_tasks: 0,
        total_cost: 0.0,
        free_mode: None,
        spin: 0,
        should_quit: false,
        confirm_quit: false,
        show_help: false,
        buffers,
        ipc_tx: ipc_tx.clone(),
        cmd_result_tx,
    };
    // Seed agent names from the startup snapshot until the first status refresh.
    st.agents = agent_names
        .iter()
        .map(|name| AgentStatusRow {
            name: name.clone(),
            provider: "?".into(),
            state: "starting".into(),
            detail: None,
            session_id: None,
            spent_usd: 0.0,
            turns: 0,
        })
        .collect();

    refresh_status(&mut st, &ipc_tx).await;

    let mut ui_rx = ui_tx.subscribe();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut dirty = true;

    loop {
        // Drain background command replies into the transcript before drawing.
        while let Ok(item) = cmd_result_rx.try_recv() {
            st.push_item(item);
            dirty = true;
        }

        if dirty {
            terminal.draw(|f| ui::draw(f, &st))?;
            dirty = false;
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        input::handle_key(&mut st, key);
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(..))) => dirty = true,
                    Some(Err(e)) => {
                        // crossterm errored reading the console — on Windows
                        // this can happen when console state is disturbed.
                        // Log it (it would otherwise be an invisible exit)
                        // before tearing down.
                        tracing::error!("terminal event stream error: {e} — shutting down TUI");
                        st.should_quit = true;
                    }
                    None => st.should_quit = true,
                    _ => {}
                }
            }
            ev = ui_rx.recv() => {
                match ev {
                    Ok(UiEvent::AgentLine { agent, line }) => {
                        // Only tool one-liners are inlined as Activity; the rest
                        // of an agent's chatter stays behind `/output`. Coalesce
                        // identical consecutive lines to keep the feed readable.
                        if let Some(rest) = line.strip_prefix("[tool] ") {
                            let new_line = format!("{agent}: {rest}");
                            let dup = matches!(
                                st.transcript.last(),
                                Some(TranscriptItem::Activity { line: l }) if *l == new_line
                            );
                            if !dup {
                                st.push_item(TranscriptItem::Activity { line: new_line });
                                dirty = true;
                            }
                        }
                    }
                    Ok(UiEvent::AgentDelta) => {
                        // Streaming worker text isn't inlined; ignore to cut redraws.
                    }
                    Ok(UiEvent::StateChange { agent, state, detail }) => {
                        if let Some(row) = st.agents.iter_mut().find(|a| a.name == agent) {
                            row.state = state.clone();
                            row.detail = detail.clone();
                        }
                        let detail_str = detail
                            .as_deref()
                            .map(|d| format!(": {d}"))
                            .unwrap_or_default();
                        st.push_item(TranscriptItem::Activity {
                            line: format!("{agent} → {state}{detail_str}"),
                        });
                        dirty = true;
                    }
                    Ok(UiEvent::MessagesChanged) => {
                        // Re-hydrate only the tail of new human/composer turns.
                        let recent = store.msg_recent(200).unwrap_or_default();
                        for m in recent.iter().filter(|m| m.id > last_msg_id) {
                            st.push_item(transcript::item_from_message(m));
                        }
                        if let Some(max) = recent.last().map(|m| m.id) {
                            last_msg_id = last_msg_id.max(max);
                        }
                        dirty = true;
                    }
                    Ok(UiEvent::TaskBoardChanged) => {
                        // Refresh counts; task lifecycle detail is on `/task list`.
                        st.open_tasks = store
                            .task_list(Some(crate::store::TaskStatus::Open), None)
                            .map(|t| t.len() as u64)
                            .unwrap_or(st.open_tasks);
                        dirty = true;
                    }
                    Ok(UiEvent::HubLog(line)) => {
                        // Hub log is no longer auto-inlined into the chat (it
                        // would drown the conversation); keep it in the hub
                        // file so the detail survives for `agentcom logs`.
                        tracing::debug!(target: "hub_log", "{line}");
                    }
                    Ok(UiEvent::Shutdown) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => { dirty = true; }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                st.spin = st.spin.wrapping_add(1);
                refresh_status(&mut st, &ipc_tx).await;
                dirty = true;
            }
        }

        if st.should_quit {
            // Ask the hub to shut down; the Shutdown broadcast ends the loop.
            let _ = request(&ipc_tx, Request::Stop { agent: None }).await;
            // Don't rely solely on the broadcast in case it raced.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            break;
        }
    }
    Ok(())
}

async fn refresh_status(st: &mut ChatState, ipc_tx: &mpsc::Sender<IpcMsg>) {
    if let Ok(Response::Status {
        agents,
        open_tasks,
        total_cost_usd,
        free,
        ..
    }) = request(ipc_tx, Request::Status).await
    {
        st.model_label = provider_summary(&agents);
        st.agents = agents;
        st.open_tasks = open_tasks;
        st.total_cost = total_cost_usd;
        st.free_mode = free;
    }
}

/// A short model/provider label for the status bar: the composer's provider if
/// present, else the most common provider across the fleet.
fn provider_summary(agents: &[AgentStatusRow]) -> String {
    if let Some(composer) = agents
        .iter()
        .find(|a| a.name == crate::config::COMPOSER_NAME)
    {
        return composer.provider.clone();
    }
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for a in agents {
        *counts.entry(a.provider.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| "—".to_string())
}
