//! Single-pane chat rendering: a full-height scrolling transcript, a persistent
//! bottom input editor, and a one-line status bar.

use super::command::SLASH_HELP;
use super::{theme, ChatState};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

/// Min/max height (including the top separator) of the input box.
const INPUT_MIN: u16 = 3;
const INPUT_MAX: u16 = 8;

/// Working-state spinner frames.
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

pub fn draw(f: &mut Frame, st: &ChatState) {
    let editor_rows = st.input.lines().len() as u16;
    let input_h = (editor_rows + 1).clamp(INPUT_MIN, INPUT_MAX);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_transcript(f, st, chunks[0]);
    draw_input(f, st, chunks[1]);
    draw_status(f, st, chunks[2]);

    if st.show_help {
        draw_help_overlay(f, f.area());
    }
}

fn draw_transcript(f: &mut Frame, st: &ChatState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &st.transcript {
        lines.extend(item.to_lines());
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Tell the composer what you want done. It will plan, recruit, and report back.",
            Style::default().fg(theme::MUTED),
        )));
        lines.push(Line::from(Span::styled(
            "Type below and press Enter. Slash commands start with '/'; F1 for help.",
            Style::default().fg(theme::MUTED),
        )));
    }

    let total = lines.len();
    let view_h = area.height as usize;
    let max_offset = total.saturating_sub(view_h);
    // Follow mode pins to the bottom; otherwise clamp the frozen offset.
    let offset = if st.scroll.follow {
        max_offset
    } else {
        st.scroll.offset.min(max_offset)
    };

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset as u16, 0)),
        area,
    );

    // Scrollbar only when content overflows the viewport.
    if total > view_h {
        let mut sb_state = ScrollbarState::new(max_offset).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut sb_state,
        );
    }
}

fn draw_input(f: &mut Frame, st: &ChatState, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme::MUTED))
        .title(Span::styled(
            if st.confirm_quit {
                " quit and stop all agents? [y/N] "
            } else {
                " > "
            },
            if st.confirm_quit {
                Style::default()
                    .fg(theme::ERROR)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::HUMAN)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(&st.input, inner);
}

fn draw_status(f: &mut Frame, st: &ChatState, area: Rect) {
    let working = st.agents.iter().filter(|a| a.state == "working").count();
    let idle = st.agents.iter().filter(|a| a.state == "idle").count();
    let n = st.agents.len();
    let free = st
        .free_mode
        .as_deref()
        .map(|f| format!(" · {f}"))
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(
            format!(" {} ", st.project),
            Style::default().fg(theme::SYSTEM),
        ),
        theme::provider_badge(&st.model_label),
        Span::styled(
            format!(
                " · {n} agents {working}w/{idle}i · ${:.2}{free} · {} open ",
                st.total_cost, st.open_tasks
            ),
            Style::default().fg(theme::SYSTEM),
        ),
    ];
    // Compact per-agent state strip; the working glyph spins on each tick.
    for a in &st.agents {
        let glyph = if a.state == "working" {
            SPINNER[st.spin % SPINNER.len()]
        } else {
            theme::state_glyph(&a.state)
        };
        spans.push(Span::styled(format!("{glyph} "), theme::state_style(&a.state)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::STATUS_BG)),
        area,
    );
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = r.x + (r.width.saturating_sub(popup_width)) / 2;
    let y = r.y + (r.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: popup_width.min(r.width),
        height: height.min(r.height),
    }
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::with_capacity(SLASH_HELP.len() + 2);
    for (cmd, desc) in SLASH_HELP {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{cmd:<22}"),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc.to_string()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "Enter",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" send  ", Style::default().fg(theme::MUTED)),
        Span::styled(
            "Shift+Enter",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" newline  ", Style::default().fg(theme::MUTED)),
        Span::styled(
            "PgUp/PgDn",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" scroll  ", Style::default().fg(theme::MUTED)),
        Span::styled(
            "Ctrl+C",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit", Style::default().fg(theme::MUTED)),
    ]));

    let popup_height = (lines.len() as u16 + 2).min(area.height);
    let popup = centered_rect(74, popup_height, area);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" commands ")
                .title_alignment(Alignment::Center)
                .style(Style::default().bg(theme::OVERLAY_BG)),
        ),
        popup,
    );
}
