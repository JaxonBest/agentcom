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
        let mut out: Vec<String> = self
            .lines
            .iter()
            .rev()
            .take(n)
            .rev()
            .cloned()
            .collect();
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

    pub fn len(&self) -> usize {
        self.lines.len() + usize::from(self.open.as_ref().is_some_and(|o| !o.is_empty()))
    }

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
    fn capped() {
        let mut rb = RingBuf::new();
        for i in 0..(RING_CAP + 100) {
            rb.push_line(i.to_string());
        }
        assert_eq!(rb.len(), RING_CAP);
        assert_eq!(rb.tail(1), vec![(RING_CAP + 99).to_string()]);
    }
}
