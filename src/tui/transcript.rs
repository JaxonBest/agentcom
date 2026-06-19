//! The chat transcript: a typed, append-only ring of items that render into
//! wrapped `Line`s. Decoupled from the 200-row `msg_recent` window so that real
//! scrollback works; the live `UiEvent` stream appends to it incrementally.

use super::theme;
use crate::store::transcript::{EventKind, TranscriptEvent};
use crate::store::Message;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Hard cap on retained transcript items; oldest are trimmed on push.
pub const TRANSCRIPT_CAP: usize = 2000;

/// One rendered unit in the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    /// A turn the human typed.
    Human { body: String },
    /// A reply from the composer (lead agent).
    Composer { body: String },
    /// Output attributed to a named worker agent.
    Agent { name: String, body: String },
    /// A compact fleet-activity one-liner (state change, tool use, task event).
    Activity { line: String },
    /// A system / command-acknowledgement line.
    System { body: String },
    /// A question directed at the human (highlighted).
    Question { from: String, body: String },
}

impl TranscriptItem {
    /// Render this item to one-or-more styled lines. Wrapping is handled by the
    /// `Paragraph` widget; we only emit the prefix + body styling here.
    pub fn to_lines(&self) -> Vec<Line<'static>> {
        match self {
            TranscriptItem::Human { body } => vec![Line::from(vec![
                Span::styled(
                    "you: ",
                    Style::default()
                        .fg(theme::HUMAN)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(body.clone()),
            ])],
            TranscriptItem::Composer { body } => vec![Line::from(vec![
                Span::styled(
                    "composer: ",
                    Style::default()
                        .fg(theme::COMPOSER)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(body.clone()),
            ])],
            TranscriptItem::Agent { name, body } => vec![Line::from(vec![
                Span::styled(
                    format!("{name}: "),
                    Style::default()
                        .fg(theme::AGENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(body.clone()),
            ])],
            TranscriptItem::Activity { line } => vec![Line::from(Span::styled(
                format!("· {line}"),
                Style::default().fg(theme::ACTIVITY),
            ))],
            TranscriptItem::System { body } => vec![Line::from(Span::styled(
                body.clone(),
                Style::default().fg(theme::SYSTEM),
            ))],
            TranscriptItem::Question { from, body } => vec![Line::from(vec![
                Span::styled(
                    format!("? {from}: "),
                    Style::default()
                        .fg(theme::QUESTION)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(body.clone(), Style::default().fg(theme::QUESTION)),
            ])],
        }
    }
}

/// Append an item to a transcript ring, trimming to [`TRANSCRIPT_CAP`].
pub fn push(transcript: &mut Vec<TranscriptItem>, item: TranscriptItem) {
    transcript.push(item);
    if transcript.len() > TRANSCRIPT_CAP {
        let overflow = transcript.len() - TRANSCRIPT_CAP;
        transcript.drain(0..overflow);
    }
}

/// Heuristic: a body that contains a `?` reads as a question to the human.
pub fn looks_like_question(body: &str) -> bool {
    body.trim().contains('?')
}

/// Build the initial transcript from the durable persisted transcript, so the
/// conversation and fleet activity survive a hub restart. This is the
/// authoritative hydration source (replacing the old `msg_recent` fallback).
pub fn hydrate_from_transcript(events: &[TranscriptEvent]) -> Vec<TranscriptItem> {
    events.iter().filter_map(from_event).collect()
}

/// Convert one persisted transcript event into a renderable item. Returns
/// `None` for events not shown inline in the chat (raw hub logs).
pub fn from_event(e: &TranscriptEvent) -> Option<TranscriptItem> {
    Some(match e.kind {
        EventKind::HumanMsg => TranscriptItem::Human {
            body: e.body.clone(),
        },
        EventKind::ComposerMsg => {
            if looks_like_question(&e.body) {
                TranscriptItem::Question {
                    from: e.actor.clone(),
                    body: e.body.clone(),
                }
            } else {
                TranscriptItem::Composer {
                    body: e.body.clone(),
                }
            }
        }
        EventKind::AgentLine => TranscriptItem::Agent {
            name: e.actor.clone(),
            body: e.body.clone(),
        },
        EventKind::AgentState => TranscriptItem::Activity {
            line: format!("{} → {}", e.actor, e.body),
        },
        EventKind::TaskEvent => TranscriptItem::Activity {
            line: e.body.clone(),
        },
        EventKind::HubLog => return None,
    })
}

/// Convert a single persisted message into a transcript item. Used by both the
/// initial hydration and the incremental tail re-hydration in the run loop.
pub fn item_from_message(m: &Message) -> TranscriptItem {
    if m.from_who == "human" {
        TranscriptItem::Human {
            body: m.body.clone(),
        }
    } else if m.to_who == "human" && looks_like_question(&m.body) {
        TranscriptItem::Question {
            from: m.from_who.clone(),
            body: m.body.clone(),
        }
    } else {
        // Anything else to/from the human is a composer-side reply. Worker
        // chatter never reaches the human directly, so this stays readable.
        TranscriptItem::Composer {
            body: m.body.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, from: &str, to: &str, body: &str) -> Message {
        Message {
            id,
            from_who: from.into(),
            to_who: to.into(),
            body: body.into(),
            urgent: false,
            delivered: false,
            created_at: id,
            delivered_at: None,
        }
    }

    fn ev(seq: i64, kind: EventKind, actor: &str, body: &str) -> TranscriptEvent {
        TranscriptEvent {
            seq,
            ts: seq,
            kind,
            actor: actor.into(),
            body: body.into(),
            task_id: None,
            session_id: None,
        }
    }

    #[test]
    fn hydrate_from_transcript_maps_kinds() {
        let events = vec![
            ev(1, EventKind::HumanMsg, "human", "build me a thing"),
            ev(2, EventKind::ComposerMsg, "composer", "should it be blue?"),
            ev(3, EventKind::ComposerMsg, "composer", "done, shipped it"),
            ev(4, EventKind::AgentState, "builder", "working"),
            ev(5, EventKind::HubLog, "hub", "internal noise"),
        ];
        let items = hydrate_from_transcript(&events);
        assert_eq!(
            items,
            vec![
                TranscriptItem::Human {
                    body: "build me a thing".into()
                },
                TranscriptItem::Question {
                    from: "composer".into(),
                    body: "should it be blue?".into()
                },
                TranscriptItem::Composer {
                    body: "done, shipped it".into()
                },
                TranscriptItem::Activity {
                    line: "builder → working".into()
                },
                // HubLog is intentionally filtered out of the chat pane.
            ]
        );
    }

    #[test]
    fn item_from_message_maps_roles() {
        assert_eq!(
            item_from_message(&msg(1, "human", "composer", "build")),
            TranscriptItem::Human {
                body: "build".into()
            }
        );
        assert_eq!(
            item_from_message(&msg(2, "composer", "human", "blue?")),
            TranscriptItem::Question {
                from: "composer".into(),
                body: "blue?".into()
            }
        );
        assert_eq!(
            item_from_message(&msg(3, "composer", "human", "done")),
            TranscriptItem::Composer {
                body: "done".into()
            }
        );
    }

    #[test]
    fn to_lines_is_non_empty_and_styled() {
        let item = TranscriptItem::Human {
            body: "hello".into(),
        };
        let lines = item.to_lines();
        assert_eq!(lines.len(), 1);
        // Prefix span carries the human color.
        let first = &lines[0].spans[0];
        assert_eq!(first.style.fg, Some(theme::HUMAN));
    }

    #[test]
    fn question_detection() {
        assert!(looks_like_question("is this right?"));
        assert!(!looks_like_question("this is fine"));
    }

    #[test]
    fn ring_trims_at_cap() {
        let mut t = Vec::new();
        for i in 0..(TRANSCRIPT_CAP + 50) {
            push(
                &mut t,
                TranscriptItem::System {
                    body: i.to_string(),
                },
            );
        }
        assert_eq!(t.len(), TRANSCRIPT_CAP);
        // Oldest entries were dropped; the newest survives.
        assert_eq!(
            t.last(),
            Some(&TranscriptItem::System {
                body: (TRANSCRIPT_CAP + 49).to_string()
            })
        );
    }
}
