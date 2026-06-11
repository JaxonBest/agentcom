//! Builders for NDJSON lines written to a child `claude` process's stdin.

use serde_json::json;

/// One user message, one line (caller appends the newline when writing).
pub fn user_message(text: &str) -> String {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn user_message_is_single_line() {
        let msg = super::user_message("hello\nworld");
        assert!(!msg.contains('\n') || msg.matches('\n').count() == msg.matches("\\n").count());
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["message"]["content"][0]["text"], "hello\nworld");
    }
}
