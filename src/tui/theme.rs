//! Central palette for the chat TUI. Every color/glyph literal that used to be
//! scattered across `ui.rs` lives here so the look has a single home.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Human turns.
pub const HUMAN: Color = Color::Green;
/// Composer (lead agent) replies.
pub const COMPOSER: Color = Color::Cyan;
/// Generic worker agent output surfaced in the transcript.
pub const AGENT: Color = Color::Cyan;
/// Fleet activity stream (state changes, tool one-liners, task lifecycle).
pub const ACTIVITY: Color = Color::DarkGray;
/// System / command-acknowledgement lines.
pub const SYSTEM: Color = Color::Indexed(245);
/// Questions directed at the human — must stand out.
pub const QUESTION: Color = Color::Yellow;
/// Errors (bad command, hub failures).
pub const ERROR: Color = Color::Red;
/// Muted secondary text (hints, metadata).
pub const MUTED: Color = Color::Indexed(242);
/// Accent for keys/labels in overlays.
pub const ACCENT: Color = Color::Cyan;
/// Status-bar background.
pub const STATUS_BG: Color = Color::Indexed(236);
/// Overlay background.
pub const OVERLAY_BG: Color = Color::Indexed(235);

/// Style for an agent state string (used by the status bar / activity lines).
pub fn state_style(state: &str) -> Style {
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

/// Single-char glyph for an agent state.
pub fn state_glyph(state: &str) -> &'static str {
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

/// Provider badge span (e.g. `[claude]`), colored per provider.
pub fn provider_badge(provider: &str) -> Span<'static> {
    match provider {
        "claude" => Span::styled("[claude]", Style::default().fg(Color::Magenta)),
        "codex" => Span::styled("[codex]", Style::default().fg(Color::Blue)),
        "deepseek" => Span::styled("[deepseek]", Style::default().fg(Color::Cyan)),
        other => Span::styled(format!("[{other}]"), Style::default().fg(Color::DarkGray)),
    }
}
