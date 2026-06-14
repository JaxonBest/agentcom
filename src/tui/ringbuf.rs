//! Per-agent capped output buffer, written by the agent's stdout/stderr
//! reader tasks and read by the TUI output pane and `tail` connections.
//!
//! Streaming text deltas append to an "open" tail line so assistant text
//! renders live instead of arriving as one block; `close_line` seals it.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

pub const RING_CAP: usize = 5_000;

#[derive(Debug, Default)]
pub struct RingBuf {
    lines: VecDeque<String>,
    open: Option<String>,
    /// Set when streaming deltas have appended to the current message, so the
    /// trailing full-text block can be recognized as a duplicate and sealed
    /// rather than reprinted. Providers that never stream (DeepSeek, Codex)
    /// leave this false, so their full-text blocks always render.
    saw_delta: bool,
}

pub type SharedRingBuf = Arc<RwLock<RingBuf>>;

impl RingBuf {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(&mut self, line: impl Into<String>) {
        self.flush_open();
        self.lines.push_back(line.into());
        while self.lines.len() > RING_CAP {
            self.lines.pop_front();
        }
    }

    /// Append streaming text to the open tail line, splitting on newlines.
    pub fn push_delta(&mut self, text: &str) {
        self.saw_delta = true;
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            self.open.get_or_insert_with(String::new).push_str(first);
        }
        for part in parts {
            self.flush_open();
            self.open = Some(part.to_string());
        }
    }

    pub fn close_line(&mut self) {
        self.flush_open();
    }

    /// Returns whether streaming deltas have arrived since the last full-text
    /// block, clearing the flag so the next message starts fresh. A true result
    /// means the trailing full-text block is a duplicate of streamed content.
    pub fn take_saw_delta(&mut self) -> bool {
        std::mem::take(&mut self.saw_delta)
    }

    fn flush_open(&mut self) {
        if let Some(open) = self.open.take() {
            if !open.is_empty() {
                self.lines.push_back(open);
                while self.lines.len() > RING_CAP {
                    self.lines.pop_front();
                }
            }
        }
    }

    /// Last `n` lines (including the open tail line), oldest first.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let mut out: Vec<String> = self.lines.iter().rev().take(n).rev().cloned().collect();
        if let Some(open) = &self.open {
            if !open.is_empty() {
                out.push(open.clone());
                if out.len() > n {
                    out.remove(0);
                }
            }
        }
        out
    }

    #[allow(dead_code)] // buffer-size accessors kept for callers/tests
    pub fn len(&self) -> usize {
        self.lines.len() + usize::from(self.open.as_ref().is_some_and(|o| !o.is_empty()))
    }

    #[allow(dead_code)] // companion to len(); no live caller after the render-path fix
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_streaming_builds_lines() {
        let mut rb = RingBuf::new();
        rb.push_delta("hel");
        rb.push_delta("lo\nwor");
        assert_eq!(rb.tail(10), vec!["hello".to_string(), "wor".to_string()]);
        rb.push_delta("ld");
        rb.close_line();
        assert_eq!(rb.tail(10), vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn push_line_seals_open_delta() {
        let mut rb = RingBuf::new();
        rb.push_delta("partial");
        rb.push_line("[tool] Bash");
        assert_eq!(
            rb.tail(10),
            vec!["partial".to_string(), "[tool] Bash".to_string()]
        );
    }

    #[test]
    fn saw_delta_tracks_streaming() {
        let mut rb = RingBuf::new();
        // Non-streaming provider: no deltas, so a full-text block renders.
        assert!(!rb.take_saw_delta());
        // Streaming provider: deltas set the flag; take clears it.
        rb.push_delta("hel");
        rb.push_delta("lo");
        assert!(rb.take_saw_delta());
        assert!(!rb.take_saw_delta());
    }

    #[test]
    fn capped() {
        let mut rb = RingBuf::new();
        for i in 0..(RING_CAP + 100) {
            rb.push_line(i.to_string());
        }
        assert_eq!(rb.len(), RING_CAP);
        assert_eq!(rb.tail(1), vec![(RING_CAP + 99).to_string()]);
    }
}
