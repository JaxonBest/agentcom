//! TUI rendering.

use super::{App, InputKind, Tab};
use crate::store::{Message, TaskStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Tabs};
use ratatui::Frame;

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(20)])
        .split(chunks[1]);
    draw_sidebar(f, app, body[0]);
    draw_main(f, app, body[1]);

    draw_footer(f, app, chunks[2]);
}

fn state_style(state: &str) -> Style {
    match state {
        "working" => Style::default().fg(Color::Green),
        "interrupting" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "idle" => Style::default().fg(Color::DarkGray),
        "paused" => Style::default().fg(Color::Yellow),
        "crashed" => Style::default().fg(Color::Red),
        "starting" => Style::default().fg(Color::Cyan),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn state_glyph(state: &str) -> &'static str {
    match state {
        "working" => ">",
        "interrupting" => "!",
        "idle" => ".",
        "paused" => "||",
        "crashed" => "x",
        "starting" => "...",
        _ => "-",
    }
}

fn provider_badge(provider: &str) -> Span<'static> {
    match provider {
        "claude" => Span::styled("[claude]", Style::default().fg(Color::Magenta)),
        "codex" => Span::styled("[codex]", Style::default().fg(Color::Blue)),
        "deepseek" => Span::styled("[deepseek]", Style::default().fg(Color::Cyan)),
        other => Span::styled(format!("[{other}]"), Style::default().fg(Color::DarkGray)),
    }
}

fn provider_usage(app: &App) -> Vec<(String, f64, u64)> {
    let mut usage = std::collections::BTreeMap::<String, (f64, u64)>::new();
    for agent in &app.agents {
        let entry = usage.entry(agent.provider.clone()).or_default();
        entry.0 += agent.spent_usd;
        entry.1 += agent.turns;
    }
    usage
        .into_iter()
        .map(|(provider, (cost, turns))| (provider, cost, turns))
        .collect()
}

fn looks_like_question(body: &str) -> bool {
    let b = body.trim();
    b.contains('?')
}

fn human_attention(app: &App) -> (usize, usize) {
    let pending = app
        .messages
        .iter()
        .filter(|m| m.to_who == "human" && !m.delivered)
        .count();
    let questions = app
        .messages
        .iter()
        .filter(|m| m.to_who == "human" && !m.delivered && looks_like_question(&m.body))
        .count();
    (pending, questions)
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let usage = provider_usage(app)
        .into_iter()
        .map(|(provider, cost, turns)| format!("{provider} ${cost:.2}/{turns}t"))
        .collect::<Vec<_>>()
        .join(" | ");
    let usage = if usage.is_empty() {
        String::new()
    } else {
        format!(" | {usage}")
    };
    let (human_pending, human_questions) = human_attention(app);
    let mut spans = vec![
        Span::styled(
            format!(" agentcom - {} ", app.project),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "| ${:.2} total{usage} | {} open",
            app.total_cost, app.open_tasks
        )),
    ];
    if human_questions > 0 {
        spans.push(Span::styled(
            format!(" | {human_questions} QUESTION(S)"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if human_pending > 0 {
        spans.push(Span::styled(
            format!(" | {human_pending} message(s) to you"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Indexed(236))),
        area,
    );
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .agent_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let row = app.agent_row(name);
            let state = row.map(|r| r.state.as_str()).unwrap_or("stopped");
            let provider = row.map(|r| r.provider.as_str()).unwrap_or("?");
            let cost = row.map(|r| r.spent_usd).unwrap_or(0.0);
            let marker = if i == app.selected { ">" } else { " " };
            let glyph = if state == "working" {
                SPINNER[app.spin % SPINNER.len()]
            } else {
                state_glyph(state)
            };
            let line = Line::from(vec![
                Span::raw(format!("{marker} ")),
                Span::styled(format!("{glyph:<2} "), state_style(state)),
                Span::raw(format!("{name:<10} ")),
                provider_badge(provider),
                Span::styled(format!(" ${cost:.2}"), Style::default().fg(Color::DarkGray)),
            ]);
            let item = ListItem::new(line);
            if i == app.selected {
                item.style(Style::default().bg(Color::Indexed(237)))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(
        List::new(items).block(Block::default().borders(Borders::RIGHT).title(" agents ")),
        area,
    );
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    f.render_widget(
        Tabs::new(Tab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
            .select(idx)
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        chunks[0],
    );

    match app.tab {
        Tab::Chat => draw_chat(f, app, chunks[1]),
        Tab::Output => draw_output(f, app, chunks[1]),
        Tab::Tasks => draw_tasks(f, app, chunks[1]),
        Tab::Messages => draw_messages(f, app, chunks[1]),
        Tab::HubLog => draw_hub_log(f, app, chunks[1]),
    }
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(8)])
        .split(area);
    draw_conversation(f, app, chunks[0]);
    draw_activity(f, app, chunks[1]);
}

fn draw_conversation(f: &mut Frame, app: &App, area: Rect) {
    let inner = area.height.saturating_sub(2) as usize;
    let chat: Vec<&Message> = app
        .messages
        .iter()
        .filter(|m| m.from_who == "human" || m.to_who == "human")
        .collect();
    let question_count = chat
        .iter()
        .filter(|m| m.to_who == "human" && !m.delivered && looks_like_question(&m.body))
        .count();
    let title = if question_count > 0 {
        format!(" composer - {question_count} question(s) need your reply ")
    } else {
        " composer ".to_string()
    };
    let lines: Vec<Line> = chat
        .iter()
        .rev()
        .take(inner.max(1))
        .rev()
        .map(|m| conversation_line(m))
        .collect();
    let lines = if lines.is_empty() {
        vec![
            Line::from(Span::styled(
                "Tell the composer what you want done. It will plan, recruit, and report back.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "(Type below, Enter to send. Tab for agent output panes.)",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        lines
    };
    let block_style = if question_count > 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(block_style),
            ),
        area,
    );
}

fn conversation_line(m: &Message) -> Line<'static> {
    if m.from_who == "human" {
        return Line::from(vec![
            Span::styled(
                format!("you -> {}: ", m.to_who),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(m.body.clone()),
        ]);
    }

    let is_question = m.to_who == "human" && !m.delivered && looks_like_question(&m.body);
    let is_pending = m.to_who == "human" && !m.delivered;
    if is_question {
        Line::from(vec![
            Span::styled(
                format!("QUESTION from {}: ", m.from_who),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(m.body.clone()),
        ])
    } else if is_pending {
        Line::from(vec![
            Span::styled(
                format!("MESSAGE from {}: ", m.from_who),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(m.body.clone()),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!("{}: ", m.from_who),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(m.body.clone()),
        ])
    }
}

fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
    let spinner = SPINNER[app.spin % SPINNER.len()];
    let mut lines: Vec<Line> = Vec::new();

    for row in &app.agents {
        let busy = matches!(row.state.as_str(), "working" | "interrupting");
        if !busy {
            continue;
        }
        let last_action: String = {
            let map = app.buffers.read().unwrap();
            map.get(&row.name)
                .map(|b| b.read().unwrap().tail(1))
                .unwrap_or_default()
                .pop()
                .unwrap_or_default()
        };
        let last_action: String = last_action.chars().take(120).collect();
        let style = if row.state == "interrupting" {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{spinner} "), style),
            Span::styled(format!("{} ", row.name), style.add_modifier(Modifier::BOLD)),
            provider_badge(&row.provider),
            Span::styled(format!(" {}", row.state), style),
            Span::styled(
                if last_action.is_empty() {
                    String::new()
                } else {
                    format!("  {last_action}")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "all agents idle",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let remaining = (area.height.saturating_sub(2) as usize).saturating_sub(lines.len());
    for event in app.hub_log.iter().rev().take(remaining).rev() {
        lines.push(Line::from(Span::styled(
            format!("- {event}"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" activity ")),
        area,
    );
}

fn draw_output(f: &mut Frame, app: &App, area: Rect) {
    let Some(name) = app.selected_agent() else {
        f.render_widget(Paragraph::new("no agent selected"), area);
        return;
    };
    let row = app.agent_row(name);
    let provider = row.map(|r| r.provider.as_str()).unwrap_or("?");
    let title = format!(
        " {name} [{provider}] - {} - {} turns - ${:.2}{} ",
        row.map(|r| r.state.clone()).unwrap_or_default(),
        row.map(|r| r.turns).unwrap_or(0),
        row.map(|r| r.spent_usd).unwrap_or(0.0),
        row.and_then(|r| r.detail.clone())
            .map(|d| format!(" - {d}"))
            .unwrap_or_default(),
    );

    let inner_height = area.height.saturating_sub(2) as usize;
    let lines: Vec<String> = {
        let map = app.buffers.read().unwrap();
        match map.get(name) {
            Some(buf) => {
                let buf = buf.read().unwrap();
                let want = inner_height + app.scroll_back.unwrap_or(0);
                let mut tail = buf.tail(want);
                if let Some(back) = app.scroll_back {
                    let keep = tail.len().saturating_sub(back);
                    tail.truncate(keep.max(inner_height.min(tail.len())));
                    let start = tail.len().saturating_sub(inner_height);
                    tail = tail.split_off(start);
                } else {
                    let start = tail.len().saturating_sub(inner_height);
                    tail = tail.split_off(start);
                }
                tail
            }
            None => vec![],
        }
    };

    let text: Vec<Line> = lines
        .iter()
        .map(|l| {
            if l.starts_with("[stderr]") {
                Line::from(Span::styled(
                    l.clone(),
                    Style::default().fg(Color::Red).dim(),
                ))
            } else if l.starts_with("[tool]") {
                Line::from(Span::styled(l.clone(), Style::default().fg(Color::Cyan)))
            } else if l.starts_with("[turn end]") || l.starts_with("[session]") {
                Line::from(Span::styled(
                    l.clone(),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(l.clone())
            }
        })
        .collect();

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if app.scroll_back.is_some() {
        block = block.title_bottom(" SCROLLED (End to follow) ");
    }
    f.render_widget(
        Paragraph::new(text)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(block),
        area,
    );
}

fn draw_tasks(f: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .tasks
        .iter()
        .map(|t| {
            let style = match t.status {
                TaskStatus::Claimed => Style::default().fg(Color::Green),
                TaskStatus::Open => Style::default(),
                TaskStatus::Blocked => Style::default().fg(Color::Yellow),
                TaskStatus::Done => Style::default().fg(Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(format!("#{}", t.id)),
                Cell::from(format!("p{}", t.priority)),
                Cell::from(t.status.as_str()),
                Cell::from(t.claimed_by.clone().unwrap_or_default()),
                Cell::from(t.title.clone()),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["id", "pr", "status", "agent", "title"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(" task board "));
    f.render_widget(table, area);
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let inner = area.height.saturating_sub(2) as usize;
    let msgs = app.messages.iter().rev().take(inner).rev();
    let lines: Vec<Line> = msgs
        .map(|m| {
            let mut spans = vec![Span::styled(
                format!("{} -> {} ", m.from_who, m.to_who),
                Style::default().fg(Color::Cyan),
            )];
            if m.to_who == "human" && !m.delivered && looks_like_question(&m.body) {
                spans.push(Span::styled(
                    "QUESTION ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            } else if m.to_who == "human" && !m.delivered {
                spans.push(Span::styled(
                    "TO YOU ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if m.urgent {
                spans.push(Span::styled(
                    "URGENT ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
            if !m.delivered {
                spans.push(Span::styled(
                    "(queued) ",
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::raw(m.body.clone()));
            Line::from(spans)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" messages ")),
        area,
    );
}

fn draw_hub_log(f: &mut Frame, app: &App, area: Rect) {
    let inner = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .hub_log
        .iter()
        .rev()
        .take(inner)
        .rev()
        .map(|l| Line::from(l.clone()))
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" hub log ")),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let (_, human_questions) = human_attention(app);
    let content: Line = if let Some(modal) = &app.modal {
        let label = match modal.kind {
            InputKind::Message => format!("message -> {}: ", app.selected_agent().unwrap_or("?")),
            InputKind::Urgent => format!("INTERRUPT -> {}: ", app.selected_agent().unwrap_or("?")),
            InputKind::Broadcast => "broadcast -> all: ".to_string(),
            InputKind::AddTask => "new task title: ".to_string(),
        };
        Line::from(vec![
            Span::styled(label, Style::default().fg(Color::Yellow)),
            Span::raw(modal.buffer.clone()),
            Span::styled("|", Style::default().fg(Color::Yellow)),
        ])
    } else if app.confirm_quit {
        Line::from(Span::styled(
            "quit and stop all agents? [y/N]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
    } else if app.tab == Tab::Chat {
        let mut spans = vec![
            Span::styled("> ", Style::default().fg(Color::Green)),
            Span::raw(app.chat_input.clone()),
            Span::styled("|", Style::default().fg(Color::Green)),
            Span::styled(
                "  (Enter send - Tab panes - Ctrl+C quit)",
                Style::default().fg(Color::DarkGray),
            ),
        ];
        if human_questions > 0 {
            spans.push(Span::styled(
                format!("  {human_questions} question(s) waiting"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    } else if let Some(flash) = &app.flash {
        Line::from(vec![
            Span::styled(format!(" {flash} "), Style::default().fg(Color::Green)),
            Span::styled(
                " [m]sg [u]rgent [M]broadcast [a]dd-task [p]ause [s]top [Tab]pane [q]uit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " [m]sg [u]rgent [M]broadcast [a]dd-task [p]ause [s]top [Up/Down]agent [PgUp/PgDn]scroll [Tab]pane [q]uit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(
        Paragraph::new(content).style(Style::default().bg(Color::Indexed(236))),
        area,
    );
}
