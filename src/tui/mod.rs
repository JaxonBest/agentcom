//! The dashboard. Runs in the hub process as "just another client": all
//! actions go through the same `IpcMsg` channel the TCP server uses, live
//! output is read straight from the shared ring buffers, and board/feed
//! data comes from read-only store queries.

pub mod input;
pub mod ringbuf;
pub mod ui;

use crate::hub::events::{IpcMsg, UiEvent};
use crate::ipc::server::Buffers;
use crate::ipc::{AgentStatusRow, Request, Response};
use crate::store::{files::FileClaim, Message, Store, Task};
use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::Stdout;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tab {
    Chat,
    Output,
    Tasks,
    Messages,
    HubLog,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Chat,
        Tab::Output,
        Tab::Tasks,
        Tab::Messages,
        Tab::HubLog,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Chat => "1 Chat",
            Tab::Output => "2 Output",
            Tab::Tasks => "3 Tasks",
            Tab::Messages => "4 Messages",
            Tab::HubLog => "5 Hub Log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputKind {
    Message,
    Urgent,
    Broadcast,
    AddTask,
    TaskFilter,
}

pub struct InputModal {
    pub kind: InputKind,
    pub buffer: String,
}

pub struct App {
    pub project: String,
    pub agent_names: Vec<String>,
    pub selected: usize,
    pub tab: Tab,
    pub agents: Vec<AgentStatusRow>,
    pub tasks: Vec<Task>,
    pub messages: Vec<Message>,
    pub hub_log: VecDeque<String>,
    pub file_claims: Vec<FileClaim>,
    pub task_filter: String,
    pub hide_done_tasks: bool,
    /// Cursor row in the visible Tasks tab list.
    pub task_cursor: usize,
    /// When Some(id), show a full-screen detail popup for that task.
    pub task_detail_id: Option<i64>,
    pub open_tasks: u64,
    pub total_cost: f64,
    /// `None` = follow live output; `Some(n)` = scrolled up by n lines.
    pub scroll_back: Option<usize>,
    /// Always-on input line in the Chat tab (talks to the composer).
    pub chat_input: String,
    /// Animation frame counter, bumped on every tick (drives spinners).
    pub spin: usize,
    pub modal: Option<InputModal>,
    pub confirm_quit: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub flash: Option<String>,
    pub buffers: Buffers,
    ipc_tx: mpsc::Sender<IpcMsg>,
}

impl App {
    pub fn selected_agent(&self) -> Option<&str> {
        self.agent_names.get(self.selected).map(|s| s.as_str())
    }

    pub fn agent_row(&self, name: &str) -> Option<&AgentStatusRow> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Fire a request at the hub; the response lands as a flash message.
    pub fn send_request(&self, req: Request) {
        let tx = self.ipc_tx.clone();
        tokio::spawn(async move {
            let _ = request(&tx, req).await;
        });
    }
}

pub async fn request(tx: &mpsc::Sender<IpcMsg>, req: Request) -> Result<Response> {
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(IpcMsg {
        identity: "human".into(),
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
    let mut app = App {
        project,
        agent_names,
        selected: 0,
        tab: Tab::Chat,
        agents: Vec::new(),
        tasks: Vec::new(),
        messages: Vec::new(),
        hub_log: VecDeque::with_capacity(500),
        file_claims: Vec::new(),
        task_filter: String::new(),
        hide_done_tasks: false,
        task_cursor: 0,
        task_detail_id: None,
        open_tasks: 0,
        total_cost: 0.0,
        scroll_back: None,
        chat_input: String::new(),
        spin: 0,
        modal: None,
        confirm_quit: false,
        should_quit: false,
        show_help: false,
        flash: None,
        buffers,
        ipc_tx: ipc_tx.clone(),
    };

    refresh_status(&mut app, &ipc_tx).await;
    refresh_board(&mut app, &store);

    let mut ui_rx = ui_tx.subscribe();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|f| ui::draw(f, &app))?;
            dirty = false;
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        input::handle_key(&mut app, key);
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(..))) => dirty = true,
                    Some(Err(e)) => {
                        // crossterm errored reading the console — on Windows
                        // this can happen when console state is disturbed.
                        // Log it (it would otherwise be an invisible exit)
                        // before tearing down.
                        tracing::error!("terminal event stream error: {e} — shutting down TUI");
                        app.should_quit = true;
                    }
                    None => app.should_quit = true,
                    _ => {}
                }
            }
            ev = ui_rx.recv() => {
                match ev {
                    Ok(UiEvent::AgentLine { .. }) | Ok(UiEvent::AgentDelta) => dirty = true,
                    Ok(UiEvent::StateChange { agent, state, detail }) => {
                        if let Some(row) = app.agents.iter_mut().find(|a| a.name == agent) {
                            row.state = state;
                            row.detail = detail;
                        }
                        dirty = true;
                    }
                    Ok(UiEvent::TaskBoardChanged) | Ok(UiEvent::MessagesChanged) => {
                        refresh_board(&mut app, &store);
                        dirty = true;
                    }
                    Ok(UiEvent::HubLog(line)) => {
                        app.hub_log.push_back(line);
                        while app.hub_log.len() > 500 { app.hub_log.pop_front(); }
                        dirty = true;
                    }
                    Ok(UiEvent::Shutdown) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => { dirty = true; }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                app.spin = app.spin.wrapping_add(1);
                refresh_status(&mut app, &ipc_tx).await;
                dirty = true;
            }
        }

        if app.should_quit {
            // Ask the hub to shut down; the Shutdown broadcast ends the loop.
            let _ = request(&ipc_tx, Request::Stop { agent: None }).await;
            // Don't rely solely on the broadcast in case it raced.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            break;
        }
    }
    Ok(())
}

async fn refresh_status(app: &mut App, ipc_tx: &mpsc::Sender<IpcMsg>) {
    if let Ok(Response::Status {
        agents,
        open_tasks,
        total_cost_usd,
        ..
    }) = request(ipc_tx, Request::Status).await
    {
        // Agents can be hot-added at runtime (`agentcom agent add`) — keep
        // the sidebar in sync with the hub, not the startup snapshot.
        let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
        if names != app.agent_names {
            app.agent_names = names;
            app.selected = app.selected.min(app.agent_names.len().saturating_sub(1));
        }
        app.agents = agents;
        app.open_tasks = open_tasks;
        app.total_cost = total_cost_usd;
    }
}

fn refresh_board(app: &mut App, store: &Store) {
    app.tasks = store.task_list(None, None).unwrap_or_default();
    app.messages = store.msg_recent(200).unwrap_or_default();
    app.file_claims = store.files_list().unwrap_or_default();
}
