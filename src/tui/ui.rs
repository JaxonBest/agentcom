//! TUI rendering.

use super::{App, InputKind, Tab};
use crate::store::TaskStatus;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Tabs};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(5),    // body
            Constraint::Length(1), // footer / input
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(20)])
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
        "working" => "▶",
        "interrupting" => "!",
        "idle" => "·",
        "paused" => "⏸",
        "crashed" => "✗",
        "starting" => "…",
        _ => "□",
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let text = Line::from(vec![
        Span::styled(
            format!(" agentcom — {} ", app.project),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "· ${:.2} · {} open task(s) ",
            app.total_cost, app.open_tasks
        )),
    ]);
    f.render_widget(
        Paragraph::new(text).style(Style::default().bg(Color::Indexed(236))),
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
            let cost = row.map(|r| r.spent_usd).unwrap_or(0.0);
            let marker = if i == app.selected { ">" } else { " " };
            let line = Line::from(vec![
                Span::raw(format!("{marker} ")),
                Span::styled(format!("{} ", state_glyph(state)), state_style(state)),
                Span::raw(format!("{name:<10}")),
                Span::styled(format!("${cost:.2}"), Style::default().fg(Color::DarkGray)),
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
        List::new(items).block(Block::default().borders(Borders::RIGHT).title("agents")),
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
            .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    match app.tab {
        Tab::Output => draw_output(f, app, chunks[1]),
        Tab::Tasks => draw_tasks(f, app, chunks[1]),
        Tab::Messages => draw_messages(f, app, chunks[1]),
        Tab::HubLog => draw_hub_log(f, app, chunks[1]),
    }
}

fn draw_output(f: &mut Frame, app: &App, area: Rect) {
    let Some(name) = app.selected_agent() else {
        f.render_widget(Paragraph::new("no agent selected"), area);
        return;
    };
    let row = app.agent_row(name);
    let title = format!(
        " {name} · {} · {} turns · ${:.2}{} ",
        row.map(|r| r.state.clone()).unwrap_or_default(),
        row.map(|r| r.turns).unwrap_or(0),
        row.map(|r| r.spent_usd).unwrap_or(0.0),
        row.and_then(|r| r.detail.clone())
            .map(|d| format!(" · {d}"))
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
                Line::from(Span::styled(l.clone(), Style::default().fg(Color::Red).dim()))
            } else if l.starts_with("[tool]") {
                Line::from(Span::styled(l.clone(), Style::default().fg(Color::Cyan)))
            } else if l.starts_with("[turn end]") || l.starts_with("[session]") {
                Line::from(Span::styled(l.clone(), Style::default().fg(Color::DarkGray)))
            } else {
                Line::from(l.clone())
            }
        })
        .collect();

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if app.scroll_back.is_some() {
        block = block.title_bottom(" SCROLLED (End to follow) ");
    }
    f.render_widget(Paragraph::new(text).block(block), area);
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
    let msgs = app
        .messages
        .iter()
        .rev()
        .take(inner)
        .rev();
    let lines: Vec<Line> = msgs
        .map(|m| {
            let mut spans = vec![Span::styled(
                format!("{} → {} ", m.from_who, m.to_who),
                Style::default().fg(Color::Cyan),
            )];
            if m.urgent {
                spans.push(Span::styled(
                    "URGENT ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
            if !m.delivered {
                spans.push(Span::styled("(queued) ", Style::default().fg(Color::Yellow)));
            }
            spans.push(Span::raw(m.body.clone()));
            Line::from(spans)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" messages ")),
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
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" hub log ")),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let content: Line = if let Some(modal) = &app.modal {
        let label = match modal.kind {
            InputKind::Message => format!(
                "message → {}: ",
                app.selected_agent().unwrap_or("?")
            ),
            InputKind::Urgent => format!(
                "INTERRUPT → {}: ",
                app.selected_agent().unwrap_or("?")
            ),
            InputKind::Broadcast => "broadcast → all: ".to_string(),
            InputKind::AddTask => "new task title: ".to_string(),
        };
        Line::from(vec![
            Span::styled(label, Style::default().fg(Color::Yellow)),
            Span::raw(modal.buffer.clone()),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
        ])
    } else if app.confirm_quit {
        Line::from(Span::styled(
            "quit and stop all agents? [y/N]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
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
            " [m]sg [u]rgent [M]broadcast [a]dd-task [p]ause [s]top [↑↓]agent [PgUp/PgDn]scroll [Tab]pane [q]uit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(
        Paragraph::new(content).style(Style::default().bg(Color::Indexed(236))),
        area,
    );
}
