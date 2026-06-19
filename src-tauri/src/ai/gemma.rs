//! Local Gemma client via LM Studio (OpenAI-compatible API).
//!
//! Pure request-builder / response-parser / prompt-builders are unit-tested.
//! The actual HTTP call ([`complete`]) is a thin wrapper that degrades
//! gracefully (never panics) when LM Studio is not running.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// LM Studio OpenAI-compatible chat endpoint.
pub const LM_STUDIO_URL: &str = "http://localhost:1234/v1/chat/completions";

/// Build an OpenAI-compatible chat-completion request body.
pub fn build_chat_request(model: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            { "role": "user", "content": prompt }
        ],
        "temperature": 0.3,
        "stream": false
    })
}

/// Extract `choices[0].message.content` from an OpenAI-compatible response.
/// Returns an `Err` (never panics) when the field is missing or the body is
/// an error payload.
pub fn parse_chat_response(json: &str) -> Result<String> {
    let root: Value = serde_json::from_str(json)?;

    if let Some(err) = root.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(anyhow!("LM Studio error: {msg}"));
    }

    root.get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("response missing choices[0].message.content"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_shape() {
        let v = build_chat_request("gemma-3", "halo");
        assert_eq!(v["model"], "gemma-3");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "halo");
        assert_eq!(v["stream"], false);
    }

    #[test]
    fn parse_extracts_content() {
        let fixture = r#"{"choices":[{"message":{"role":"assistant","content":"Hello"}}]}"#;
        assert_eq!(parse_chat_response(fixture).unwrap(), "Hello");
    }

    #[test]
    fn parse_error_payload_is_err_not_panic() {
        let fixture = r#"{"error":{"message":"no model loaded"}}"#;
        assert!(parse_chat_response(fixture).is_err());
    }

    #[test]
    fn parse_empty_object_is_err() {
        assert!(parse_chat_response("{}").is_err());
    }
}
