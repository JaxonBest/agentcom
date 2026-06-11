//! Typed view of the `claude --output-format stream-json` NDJSON stream.
//!
//! Drift tolerance is layered three deep so a CLI upgrade can never crash
//! the hub: unknown `type` tags land in [`CliEvent::Unknown`], unknown
//! fields are ignored by serde's default behavior, and lines that fail to
//! parse entirely degrade to [`ParsedLine::Raw`].

use serde::Deserialize;
use serde_json::Value;

// Some fields are deserialized but not (yet) read in code; they document the
// wire shapes and surface in Debug logs.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliEvent {
    /// `subtype: "init"` carries the session id and model.
    System {
        subtype: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    Assistant {
        message: AssistantMessage,
    },
    /// Tool results echoed back into the transcript.
    User {
        #[serde(default)]
        message: Value,
    },
    /// End of a turn. ANY result — success or error subtype — is treated as
    /// the turn-end barrier; subtypes only inform logging.
    Result {
        subtype: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        num_turns: Option<u64>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        result: Option<String>,
    },
    /// Partial-message API events (`--include-partial-messages`).
    StreamEvent {
        event: Value,
    },
    ControlResponse {
        #[serde(default)]
        response: Value,
    },
    /// Echoes of stdin user messages (`--replay-user-messages`) and any
    /// future event types.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        #[serde(default)]
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug)]
pub enum ParsedLine {
    Event(CliEvent),
    Raw(String),
}

pub fn parse_line(line: &str) -> ParsedLine {
    match serde_json::from_str::<CliEvent>(line) {
        Ok(ev) => ParsedLine::Event(ev),
        Err(e) => {
            tracing::warn!(error = %e, line, "unparsed cli event line");
            ParsedLine::Raw(line.to_string())
        }
    }
}

/// Pull displayable text deltas out of a `stream_event` for live TUI output.
/// Returns `None` for non-text events (tool deltas, message boundaries...).
pub fn stream_text_delta(event: &Value) -> Option<&str> {
    if event.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = event.get("delta")?;
    match delta.get("type")?.as_str()? {
        "text_delta" => delta.get("text")?.as_str(),
        _ => None,
    }
}

/// True when a `stream_event` closes a content block (flush the open TUI line).
pub fn stream_block_end(event: &Value) -> bool {
    event
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| t == "content_block_stop" || t == "message_stop")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"claude-sonnet-4-6","tools":["Bash"],"cwd":"C:\\x"}"#;
        match parse_line(line) {
            ParsedLine::Event(CliEvent::System {
                subtype,
                session_id,
                ..
            }) => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id.as_deref(), Some("abc-123"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_result_with_extra_fields() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"total_cost_usd":0.0042,"num_turns":3,"session_id":"abc","result":"done","brand_new_field":{"x":1}}"#;
        match parse_line(line) {
            ParsedLine::Event(CliEvent::Result {
                subtype,
                total_cost_usd,
                ..
            }) => {
                assert_eq!(subtype, "success");
                assert_eq!(total_cost_usd, Some(0.0042));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_type_is_tolerated() {
        let line = r#"{"type":"totally_new_event","payload":42}"#;
        assert!(matches!(
            parse_line(line),
            ParsedLine::Event(CliEvent::Unknown)
        ));
    }

    #[test]
    fn garbage_degrades_to_raw() {
        assert!(matches!(parse_line("not json at all"), ParsedLine::Raw(_)));
    }

    #[test]
    fn extracts_text_delta() {
        let ev: Value = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        )
        .unwrap();
        assert_eq!(stream_text_delta(&ev), Some("hi"));
        let stop: Value = serde_json::from_str(r#"{"type":"content_block_stop","index":0}"#).unwrap();
        assert!(stream_block_end(&stop));
    }

    /// Real NDJSON captured from claude CLI 2.1.173 on 2026-06-11, including
    /// an interrupted turn (result subtype `error_during_execution`). Every
    /// line must parse as a typed event — never fall back to Raw.
    #[test]
    fn live_fixtures_parse() {
        let fixtures = include_str!("../../tests/fixtures/live_claude_2.1.173.ndjson");
        let mut results = 0;
        let mut interrupted = 0;
        let mut control_responses = 0;
        for line in fixtures.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line) {
                ParsedLine::Event(ev) => match ev {
                    CliEvent::Result { is_error, .. } => {
                        results += 1;
                        if is_error {
                            interrupted += 1;
                        }
                    }
                    CliEvent::ControlResponse { .. } => control_responses += 1,
                    _ => {}
                },
                ParsedLine::Raw(raw) => panic!("fixture line failed to parse: {raw}"),
            }
        }
        assert_eq!(results, 2, "one success + one interrupted result");
        assert_eq!(interrupted, 1);
        assert_eq!(control_responses, 1);
    }

    #[test]
    fn parses_assistant_blocks() {
        let line = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"hello"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
        match parse_line(line) {
            ParsedLine::Event(CliEvent::Assistant { message }) => {
                assert_eq!(message.content.len(), 2);
                assert!(matches!(&message.content[0], ContentBlock::Text { text } if text == "hello"));
                assert!(
                    matches!(&message.content[1], ContentBlock::ToolUse { name, .. } if name == "Bash")
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
