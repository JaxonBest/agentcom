//! TUI key handling.

use super::{App, InputKind, InputModal, Tab};
use crate::ipc::Request;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Modal input line takes priority.
    if app.modal.is_some() {
        handle_modal_key(app, key);
        return;
    }

    if app.confirm_quit {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => app.should_quit = true,
            _ => app.confirm_quit = false,
        }
        return;
    }

    // ? toggles help overlay from anywhere (except modals handled above).
    if key.code == KeyCode::Char('?') {
        app.show_help = !app.show_help;
        return;
    }
    // Esc closes help overlay if open.
    if app.show_help && key.code == KeyCode::Esc {
        app.show_help = false;
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.confirm_quit = true;
        return;
    }

    // Chat tab: the keyboard belongs to the input line (so you can type
    // freely); Tab still switches panes, Ctrl+C still quits.
    if app.tab == Tab::Chat {
        handle_chat_key(app, key);
        return;
    }

    // Task detail popup: Esc closes, 'r' reopens, 'i' toggles pin, 0-4 sets priority.
    if let Some(id) = app.task_detail_id {
        match key.code {
            KeyCode::Esc => app.task_detail_id = None,
            KeyCode::Char('r') => {
                app.send_request(Request::TaskReopen { id });
                app.flash = Some(format!("reopened task #{id}"));
                app.task_detail_id = None;
            }
            KeyCode::Char('i') => {
                let pinned = app.tasks.iter().find(|t| t.id == id).map(|t| t.pinned).unwrap_or(false);
                if pinned {
                    app.send_request(Request::TaskUnpin { id });
                    app.flash = Some(format!("unpinned task #{id}"));
                } else {
                    app.send_request(Request::TaskPin { id });
                    app.flash = Some(format!("pinned task #{id}"));
                }
                app.task_detail_id = None;
            }
            KeyCode::Char(c @ '0'..='4') => {
                let priority = (c as i64) - ('0' as i64);
                app.send_request(Request::TaskEdit {
                    id,
                    title: None,
                    description: None,
                    priority: Some(priority),
                });
                app.flash = Some(format!("task #{id} priority → p{priority}"));
                app.task_detail_id = None;
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.confirm_quit = true,
        KeyCode::Tab => {
            let i = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
            app.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
        }
        KeyCode::Char('1') => app.tab = Tab::Chat,
        KeyCode::Char('2') => app.tab = Tab::Output,
        KeyCode::Char('3') => app.tab = Tab::Tasks,
        KeyCode::Char('4') => app.tab = Tab::Messages,
        KeyCode::Char('5') => app.tab = Tab::HubLog,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.tab == Tab::Tasks {
                if app.task_cursor > 0 {
                    app.task_cursor -= 1;
                }
            } else {
                if app.selected > 0 {
                    app.selected -= 1;
                    app.scroll_back = None;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.tab == Tab::Tasks {
                let n = visible_task_count(app);
                if app.task_cursor + 1 < n {
                    app.task_cursor += 1;
                }
            } else {
                if app.selected + 1 < app.agent_names.len() {
                    app.selected += 1;
                    app.scroll_back = None;
                }
            }
        }
        KeyCode::Enter if app.tab == Tab::Tasks => {
            let filter = app.task_filter.to_lowercase();
            let visible: Vec<i64> = app
                .tasks
                .iter()
                .filter(|t| {
                    if app.hide_done_tasks && t.status == crate::store::TaskStatus::Done {
                        return false;
                    }
                    if filter.is_empty() {
                        return true;
                    }
                    t.title.to_lowercase().contains(&filter)
                        || t.description.to_lowercase().contains(&filter)
                        || t.status.as_str().contains(&filter)
                        || t.claimed_by.as_deref().map(|s| s.to_lowercase().contains(&filter)).unwrap_or(false)
                })
                .map(|t| t.id)
                .collect();
            if let Some(&id) = visible.get(app.task_cursor) {
                app.task_detail_id = Some(id);
            }
        }
        KeyCode::PageUp => {
            let cur = app.scroll_back.unwrap_or(0);
            app.scroll_back = Some(cur + 20);
        }
        KeyCode::PageDown => {
            app.scroll_back = match app.scroll_back {
                Some(n) if n > 20 => Some(n - 20),
                _ => None,
            };
        }
        KeyCode::End => app.scroll_back = None,
        KeyCode::Char('m') => open_modal(app, InputKind::Message),
        KeyCode::Char('u') => open_modal(app, InputKind::Urgent),
        KeyCode::Char('M') => {
            app.modal = Some(InputModal {
                kind: InputKind::Broadcast,
                buffer: String::new(),
            })
        }
        KeyCode::Char('a') => {
            app.modal = Some(InputModal {
                kind: InputKind::AddTask,
                buffer: String::new(),
            })
        }
        KeyCode::Char('/') => {
            app.modal = Some(InputModal {
                kind: InputKind::TaskFilter,
                buffer: app.task_filter.clone(),
            });
        }
        KeyCode::Char('F') => {
            app.task_filter.clear();
            app.flash = Some("filter cleared".into());
        }
        KeyCode::Char('d') => {
            app.hide_done_tasks = !app.hide_done_tasks;
            app.flash = Some(if app.hide_done_tasks {
                "hiding done tasks".into()
            } else {
                "showing all tasks".into()
            });
        }
        KeyCode::Char('p') => {
            if let Some(name) = app.selected_agent().map(str::to_string) {
                let paused = app
                    .agent_row(&name)
                    .map(|r| r.state == "paused")
                    .unwrap_or(false);
                let req = if paused {
                    Request::Resume {
                        agent: name.clone(),
                    }
                } else {
                    Request::Pause {
                        agent: name.clone(),
                    }
                };
                app.send_request(req);
                app.flash = Some(format!(
                    "{} {name}",
                    if paused { "resuming" } else { "pausing" }
                ));
            }
        }
        KeyCode::Char('s') => {
            if let Some(name) = app.selected_agent().map(str::to_string) {
                app.send_request(Request::Stop {
                    agent: Some(name.clone()),
                });
                app.flash = Some(format!("stopping {name}"));
            }
        }
        // P — fleet-wide pause (all agents); p already handles single-agent pause/resume.
        KeyCode::Char('P') => {
            app.send_request(Request::Pause { agent: "all".to_string() });
            app.flash = Some("pausing all agents".into());
        }
        // R — fleet-wide resume.
        KeyCode::Char('R') => {
            app.send_request(Request::Resume { agent: "all".to_string() });
            app.flash = Some("resuming all agents".into());
        }
        _ => {}
    }
}

fn handle_chat_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Tab => {
            app.tab = Tab::Output;
        }
        KeyCode::Enter => {
            let text = app.chat_input.trim().to_string();
            app.chat_input.clear();
            if text.is_empty() {
                return;
            }
            app.send_request(Request::Send {
                to: crate::config::COMPOSER_NAME.to_string(),
                body: text,
                urgent: false,
            });
        }
        KeyCode::Esc => app.chat_input.clear(),
        KeyCode::Backspace => {
            app.chat_input.pop();
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.chat_input.push(c);
        }
        _ => {}
    }
}

fn visible_task_count(app: &App) -> usize {
    let filter = app.task_filter.to_lowercase();
    app.tasks
        .iter()
        .filter(|t| {
            if app.hide_done_tasks && t.status == crate::store::TaskStatus::Done {
                return false;
            }
            if filter.is_empty() {
                return true;
            }
            t.title.to_lowercase().contains(&filter)
                || t.description.to_lowercase().contains(&filter)
                || t.status.as_str().contains(&filter)
                || t.claimed_by
                    .as_deref()
                    .map(|s| s.to_lowercase().contains(&filter))
                    .unwrap_or(false)
        })
        .count()
}

fn open_modal(app: &mut App, kind: InputKind) {
    if app.selected_agent().is_some() {
        app.modal = Some(InputModal {
            kind,
            buffer: String::new(),
        });
    }
}

fn handle_modal_key(app: &mut App, key: KeyEvent) {
    let Some(modal) = app.modal.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => app.modal = None,
        KeyCode::Enter => {
            let kind = modal.kind;
            let text = modal.buffer.trim().to_string();
            app.modal = None;
            // TaskFilter allows empty text (clears the filter).
            if text.is_empty() && kind != InputKind::TaskFilter {
                return;
            }
            match kind {
                InputKind::Message | InputKind::Urgent => {
                    if let Some(name) = app.selected_agent().map(str::to_string) {
                        app.send_request(Request::Send {
                            to: name.clone(),
                            body: text,
                            urgent: kind == InputKind::Urgent,
                        });
                        app.flash = Some(format!(
                            "{} {name}",
                            if kind == InputKind::Urgent {
                                "interrupting"
                            } else {
                                "messaged"
                            }
                        ));
                    }
                }
                InputKind::Broadcast => {
                    app.send_request(Request::Send {
                        to: "all".into(),
                        body: text,
                        urgent: false,
                    });
                    app.flash = Some("broadcast sent".into());
                }
                InputKind::AddTask => {
                    app.send_request(Request::TaskAdd {
                        title: text,
                        description: String::new(),
                        priority: 2,
                        depends_on: vec![],
                        timeout_mins: None,
                        requires: vec![],
                        recur: None,
                    });
                    app.flash = Some("task added".into());
                }
                InputKind::TaskFilter => {
                    app.task_filter = text.clone();
                    if text.is_empty() {
                        app.flash = Some("filter cleared".into());
                    } else {
                        app.flash = Some(format!("filter: {text}"));
                    }
                }
            }
        }
        KeyCode::Backspace => {
            modal.buffer.pop();
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            modal.buffer.push(c);
        }
        _ => {}
    }
}
