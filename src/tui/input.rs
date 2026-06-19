//! TUI key handling. Global keys (quit, scroll, help) are handled first;
//! everything else flows into the bottom line editor. Enter sends the buffer to
//! the composer or dispatches a slash command.

use super::transcript::TranscriptItem;
use super::{command, ChatState};
use crate::config::COMPOSER_NAME;
use crate::ipc::Request;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tui_textarea::{CursorMove, TextArea};

/// How many transcript lines a PageUp/PageDown moves.
const PAGE: usize = 10;

pub fn handle_key(st: &mut ChatState, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Quit confirmation gate takes precedence over everything.
    if st.confirm_quit {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => st.should_quit = true,
            _ => st.confirm_quit = false,
        }
        return;
    }

    // Ctrl+C arms the quit confirmation.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        st.confirm_quit = true;
        return;
    }

    // Help overlay toggle (F1) / close (Esc).
    if key.code == KeyCode::F(1) {
        st.show_help = !st.show_help;
        return;
    }
    if st.show_help {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            st.show_help = false;
        }
        return;
    }

    // Transcript scrollback.
    match key.code {
        KeyCode::PageUp => {
            scroll_up(st, PAGE);
            return;
        }
        KeyCode::PageDown => {
            scroll_down(st, PAGE);
            return;
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            st.scroll.follow = false;
            st.scroll.offset = 0;
            return;
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            st.scroll.follow = true;
            return;
        }
        _ => {}
    }
    // Ctrl+U / Ctrl+D scroll a half-screen (don't collide with editor editing,
    // which uses bare keys).
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('u') => {
                scroll_up(st, PAGE / 2);
                return;
            }
            KeyCode::Char('d') => {
                scroll_down(st, PAGE / 2);
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            // Shift+Enter / Alt+Enter insert a newline; bare Enter submits.
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            {
                st.input.insert_newline();
            } else {
                submit(st);
            }
        }
        KeyCode::Tab => autocomplete(st),
        KeyCode::Up => {
            // Walk history only when the cursor is on the first editor row.
            if st.input.cursor().0 == 0 {
                history_prev(st);
            } else {
                st.input.move_cursor(CursorMove::Up);
            }
        }
        KeyCode::Down => {
            let last_row = st.input.lines().len().saturating_sub(1);
            if st.input.cursor().0 == last_row {
                history_next(st);
            } else {
                st.input.move_cursor(CursorMove::Down);
            }
        }
        _ => {
            // Everything else is editing — forward to the line editor.
            st.input.input(Event::Key(key));
        }
    }
}

fn scroll_up(st: &mut ChatState, lines: usize) {
    st.scroll.follow = false;
    st.scroll.offset = st.scroll.offset.saturating_sub(lines);
}

fn scroll_down(st: &mut ChatState, lines: usize) {
    if st.scroll.follow {
        return;
    }
    st.scroll.offset = st.scroll.offset.saturating_add(lines);
    // The renderer clamps offset to the bottom; following resumes via Ctrl+End.
}

fn current_text(st: &ChatState) -> String {
    st.input.lines().join("\n")
}

fn submit(st: &mut ChatState) {
    let text = current_text(st);
    let trimmed = text.trim().to_string();
    st.input = ChatState::fresh_input();
    st.hist_idx = None;
    if trimmed.is_empty() {
        return;
    }
    // Record in history (skip consecutive duplicates).
    if st.history.last().map(String::as_str) != Some(trimmed.as_str()) {
        st.history.push(trimmed.clone());
    }
    // Sending resumes follow mode so the new turn is visible.
    st.scroll.follow = true;

    if trimmed.starts_with('/') {
        match command::parse(&trimmed) {
            Ok(cmd) => command::exec(cmd, st),
            Err(e) => st.push_item(TranscriptItem::System { body: e }),
        }
        return;
    }

    // Optimistically show the human turn before the store round-trips.
    st.push_item(TranscriptItem::Human {
        body: trimmed.clone(),
    });
    st.send_request(Request::Send {
        to: COMPOSER_NAME.to_string(),
        body: trimmed,
        urgent: false,
    });
}

/// Tab-complete a slash command when the buffer is a single `/`-prefixed token.
fn autocomplete(st: &mut ChatState) {
    let text = current_text(st);
    let trimmed = text.trim_start();
    if !trimmed.starts_with('/') || trimmed.contains(char::is_whitespace) {
        return;
    }
    let matches = command::complete(trimmed);
    if let [only] = matches.as_slice() {
        let mut ta = ChatState::fresh_input();
        ta.insert_str(format!("{only} "));
        st.input = ta;
    } else if !matches.is_empty() {
        st.push_item(TranscriptItem::System {
            body: matches.join("  "),
        });
    }
}

fn history_prev(st: &mut ChatState) {
    if st.history.is_empty() {
        return;
    }
    let idx = match st.hist_idx {
        Some(0) => 0,
        Some(i) => i - 1,
        None => st.history.len() - 1,
    };
    st.hist_idx = Some(idx);
    set_input_text(st, &st.history[idx].clone());
}

fn history_next(st: &mut ChatState) {
    match st.hist_idx {
        Some(i) if i + 1 < st.history.len() => {
            st.hist_idx = Some(i + 1);
            set_input_text(st, &st.history[i + 1].clone());
        }
        Some(_) => {
            // Past the newest entry — clear back to an empty editor.
            st.hist_idx = None;
            st.input = ChatState::fresh_input();
        }
        None => {}
    }
}

fn set_input_text(st: &mut ChatState, text: &str) {
    let mut ta: TextArea<'static> = ChatState::fresh_input();
    ta.insert_str(text);
    st.input = ta;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn type_str(st: &mut ChatState, s: &str) {
        for c in s.chars() {
            handle_key(st, key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn history_walk_up_up_down() {
        let mut st = ChatState::for_test();
        st.history = vec!["one".into(), "two".into(), "three".into()];
        // Up recalls newest first.
        handle_key(&mut st, key(KeyCode::Up));
        assert_eq!(st.input.lines(), &["three".to_string()]);
        // Up again walks back.
        handle_key(&mut st, key(KeyCode::Up));
        assert_eq!(st.input.lines(), &["two".to_string()]);
        // Down walks forward again.
        handle_key(&mut st, key(KeyCode::Down));
        assert_eq!(st.input.lines(), &["three".to_string()]);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut st = ChatState::for_test();
        type_str(&mut st, "line1");
        handle_key(&mut st, key_mod(KeyCode::Enter, KeyModifiers::SHIFT));
        type_str(&mut st, "line2");
        assert_eq!(st.input.lines().len(), 2);
        assert_eq!(
            st.input.lines(),
            &["line1".to_string(), "line2".to_string()]
        );
    }

    #[test]
    fn enter_on_slash_routes_to_command_not_send() {
        let mut st = ChatState::for_test();
        // A bad command: parse error pushes a System item synchronously and
        // does NOT optimistically push a Human turn (which a Send would).
        type_str(&mut st, "/bogus");
        handle_key(&mut st, key(KeyCode::Enter));
        assert!(matches!(
            st.transcript.last(),
            Some(TranscriptItem::System { .. })
        ));
        assert!(
            !st.transcript
                .iter()
                .any(|i| matches!(i, TranscriptItem::Human { .. })),
            "slash command must not produce a Human turn"
        );
        // Input is cleared after submit.
        assert_eq!(st.input.lines(), &[String::new()]);
    }

    #[tokio::test]
    async fn enter_on_plain_text_pushes_human_turn() {
        // send_request spawns onto the runtime, so run inside one.
        let mut st = ChatState::for_test();
        type_str(&mut st, "hello composer");
        handle_key(&mut st, key(KeyCode::Enter));
        assert_eq!(
            st.transcript.last(),
            Some(&TranscriptItem::Human {
                body: "hello composer".into()
            })
        );
        // Recorded in history.
        assert_eq!(st.history, vec!["hello composer".to_string()]);
    }

    #[test]
    fn ctrl_c_arms_then_confirms_quit() {
        let mut st = ChatState::for_test();
        handle_key(&mut st, key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(st.confirm_quit);
        assert!(!st.should_quit);
        handle_key(&mut st, key(KeyCode::Char('y')));
        assert!(st.should_quit);
    }

    #[test]
    fn pageup_stops_following() {
        let mut st = ChatState::for_test();
        assert!(st.scroll.follow);
        handle_key(&mut st, key(KeyCode::PageUp));
        assert!(!st.scroll.follow);
    }
}
