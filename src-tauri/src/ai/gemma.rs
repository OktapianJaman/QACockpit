//! Local Gemma client via LM Studio (OpenAI-compatible API).
//!
//! Pure request-builder / response-parser / prompt-builders are unit-tested.
//! The actual HTTP call ([`complete`]) is a thin wrapper that degrades
//! gracefully (never panics) when LM Studio is not running.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::core::fairness::{Assessment, Fairness};
use crate::core::types::ActivityBlock;
use crate::integrations::jira::JiraTicket;

/// LM Studio OpenAI-compatible chat endpoint.
pub const LM_STUDIO_URL: &str = "http://localhost:1234/v1/chat/completions";

/// Cap on how many activity blocks we list in a summary prompt.
const MAX_BLOCKS: usize = 20;

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

/// Build an Indonesian prompt asking Gemma to summarize the workday from
/// activity blocks and Jira tickets. Lists the top [`MAX_BLOCKS`] non-idle
/// blocks by duration.
pub fn daily_summary_prompt(blocks: &[ActivityBlock], tickets: &[JiraTicket]) -> String {
    let mut active: Vec<&ActivityBlock> = blocks.iter().filter(|b| !b.is_idle).collect();
    active.sort_by(|a, b| b.duration_secs().cmp(&a.duration_secs()));
    active.truncate(MAX_BLOCKS);

    let mut s = String::new();
    s.push_str(
        "Kamu adalah asisten QA. Ringkas dan rangkum aktivitas kerja hari ini \
         dalam bahasa Indonesia yang singkat dan jelas. Sebutkan fokus utama \
         dan kaitan dengan tiket Jira bila ada.\n\n",
    );

    s.push_str("Aktivitas (aplikasi - judul - menit):\n");
    if active.is_empty() {
        s.push_str("- (tidak ada aktivitas aktif)\n");
    } else {
        for b in &active {
            let minutes = b.duration_secs() / 60;
            s.push_str(&format!("- {} - {} - {} menit\n", b.app, b.title, minutes));
        }
    }

    s.push_str("\nTiket Jira:\n");
    if tickets.is_empty() {
        s.push_str("- (tidak ada tiket)\n");
    } else {
        for t in tickets {
            s.push_str(&format!("- {}: {}\n", t.key, t.summary));
        }
    }

    s.push_str("\nBuat ringkasan kerja harian dari data di atas.");
    s
}

/// Build an Indonesian prompt asking Gemma to explain which tickets are
/// under/over-pointed and give advice, using the rule "1 jam = 2 poin".
pub fn explain_fairness_prompt(items: &[(String, Assessment)]) -> String {
    let mut s = String::new();
    s.push_str(
        "Kamu adalah asisten QA. Aturannya: 1 jam kerja = 2 poin. Jelaskan \
         dalam bahasa Indonesia tiket mana yang kurang poin (under-pointed) \
         atau kelebihan poin (over-pointed), lalu beri saran penyesuaian \
         story point.\n\n",
    );

    s.push_str("Penilaian per tiket (poin layak vs poin diberikan):\n");
    if items.is_empty() {
        s.push_str("- (tidak ada tiket)\n");
    } else {
        for (key, a) in items {
            let status = match a.status {
                Fairness::Fair => "wajar",
                Fairness::UnderPointed => "kurang poin",
                Fairness::OverPointed => "kelebihan poin",
            };
            s.push_str(&format!(
                "- {}: layak {} poin, diberikan {} poin -> {}\n",
                key, a.deserved, a.assigned, status
            ));
        }
    }

    s.push_str("\nBerikan penjelasan dan saran poin untuk tiap tiket di atas.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

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

    fn block(app: &str, title: &str, minutes: i64) -> ActivityBlock {
        let start = Utc.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
        ActivityBlock {
            app: app.to_string(),
            title: title.to_string(),
            start,
            end: start + chrono::Duration::minutes(minutes),
            is_idle: false,
        }
    }

    fn ticket(key: &str, summary: &str) -> JiraTicket {
        JiraTicket {
            key: key.to_string(),
            summary: summary.to_string(),
            status: "In Progress".to_string(),
            story_points: Some(3.0),
            updated: "2026-06-19".to_string(),
        }
    }

    #[test]
    fn daily_summary_prompt_contains_key_facts() {
        let blocks = vec![block("VS Code", "JIRA-1 work", 30)];
        let tickets = vec![ticket("JIRA-1", "fix the thing")];
        let p = daily_summary_prompt(&blocks, &tickets);
        assert!(p.contains("VS Code"));
        assert!(p.contains("JIRA-1"));
        // Indonesian instruction word.
        assert!(p.contains("ringkas") || p.contains("rangkum"));
    }

    #[test]
    fn fairness_prompt_contains_key_facts() {
        let items = vec![(
            "JIRA-9".to_string(),
            Assessment {
                deserved: 12.0,
                assigned: 3.0,
                status: Fairness::UnderPointed,
            },
        )];
        let p = explain_fairness_prompt(&items);
        assert!(p.contains("JIRA-9"));
        assert!(p.contains("12"));
        assert!(p.contains("3"));
        assert!(p.contains("poin"));
    }
}
