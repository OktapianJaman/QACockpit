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

/// LM Studio OpenAI-compatible models-list endpoint.
pub const LM_STUDIO_MODELS_URL: &str = "http://localhost:1234/v1/models";

/// Cap on how many activity blocks we list in a summary prompt.
const MAX_BLOCKS: usize = 20;

/// Friendly Indonesian message shown when the local AI cannot be reached.
const AI_UNAVAILABLE: &str =
    "(AI lokal tidak tersedia — pastikan LM Studio jalan di localhost:1234)";

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

/// Extract the model ids from an OpenAI-compatible `/v1/models` response
/// (`data[].id`). Returns an empty list if the shape is unexpected.
pub fn parse_models(json: &str) -> Result<Vec<String>> {
    let root: Value = serde_json::from_str(json)?;
    let ids = root
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(ids)
}

/// Fetch the list of model ids loaded in LM Studio.
///
/// Degrades gracefully: returns an empty list (never errors) if LM Studio is
/// not reachable, so the UI can fall back to manual entry.
pub fn list_models() -> Vec<String> {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let text = match client.get(LM_STUDIO_MODELS_URL).send().and_then(|r| r.text()) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    parse_models(&text).unwrap_or_default()
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

/// Build an Indonesian prompt asking Gemma to draft test cases for a ticket.
/// Uses a SIMPLE pipe-separated, one-per-line format that a small local model
/// can follow reliably: `Judul | Langkah | Hasil yang diharapkan`.
pub fn test_cases_prompt(summary: &str, key: &str) -> String {
    format!(
        "Kamu adalah asisten QA. Buatkan 3 sampai 6 test case untuk tiket Jira berikut.\n\n\
         Tiket: {key}\n\
         Ringkasan: {summary}\n\n\
         ATURAN OUTPUT (wajib diikuti):\n\
         - Satu test case per baris.\n\
         - Format tiap baris PERSIS: Judul | Langkah | Hasil yang diharapkan\n\
         - Pakai tanda pipa (|) sebagai pemisah ketiga bagian.\n\
         - JANGAN pakai penomoran, bullet, atau teks tambahan apa pun.\n\
         - Tulis dalam bahasa Indonesia.\n\n\
         Contoh:\n\
         Login dengan kredensial valid | Buka halaman login, isi email & password benar, klik Masuk | Pengguna masuk ke dashboard\n\n\
         Sekarang tulis test case-nya:"
    )
}

/// Parse the model's pipe-separated test-case output into `(title, steps, expected)`
/// tuples. Tolerant: strips leading markdown bullets / numbering (`- `, `* `,
/// `N. `, `N) `), splits each line on `|` into at most 3 parts, trims, and skips
/// any line that yields no title.
pub fn parse_test_cases(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = strip_leading_marker(raw.trim());
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let title = parts.next().unwrap_or("").trim().to_string();
        if title.is_empty() {
            continue;
        }
        let steps = parts.next().unwrap_or("").trim().to_string();
        let expected = parts.next().unwrap_or("").trim().to_string();
        out.push((title, steps, expected));
    }
    out
}

/// Strip a leading markdown bullet (`- `, `* `) or numbering (`12. `, `3) `)
/// from a trimmed line.
fn strip_leading_marker(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return rest.trim_start();
    }
    // Numbered: leading digits followed by '.' or ')' then whitespace.
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &line[digits.len()..];
        if let Some(after) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            if after.starts_with(char::is_whitespace) {
                return after.trim_start();
            }
        }
    }
    line
}

/// Convenience: generate draft test cases for a ticket via the local model.
pub fn generate_test_cases(
    model: &str,
    key: &str,
    summary: &str,
) -> Vec<(String, String, String)> {
    parse_test_cases(&complete(model, &test_cases_prompt(summary, key)))
}

/// Max diff length embedded in a PR-review prompt (local model has a small
/// context window). Diffs longer than this are truncated with a note.
const MAX_DIFF_CHARS: usize = 8000;

/// Build an Indonesian prompt asking Gemma, as a QA assistant, to review a PR
/// diff for a ticket: (1) a short change summary, and (2) what a QA should test
/// / risk areas. The diff is truncated to [`MAX_DIFF_CHARS`] before embedding;
/// when truncated a "(diff dipotong)" note is appended.
pub fn pr_review_prompt(key: &str, summary: &str, diff: &str) -> String {
    let (diff_text, truncated) = if diff.chars().count() > MAX_DIFF_CHARS {
        let cut: String = diff.chars().take(MAX_DIFF_CHARS).collect();
        (cut, true)
    } else {
        (diff.to_string(), false)
    };
    let note = if truncated { "\n(diff dipotong)" } else { "" };

    format!(
        "Kamu adalah asisten QA. Diberikan tiket Jira dan diff Pull Request-nya, \
         bantu QA memahami perubahan dan apa yang harus dites.\n\n\
         Tiket: {key}\n\
         Ringkasan: {summary}\n\n\
         Diff PR:\n\
         ```diff\n{diff_text}{note}\n```\n\n\
         Tulis dalam bahasa Indonesia dengan dua bagian:\n\
         1. Ringkasan singkat perubahan (apa yang diubah dan kenapa).\n\
         2. Apa yang harus dites / area berisiko (daftar poin yang perlu diperhatikan QA).\n"
    )
}

/// Convenience: ask the local model to review a PR diff for a ticket.
pub fn review_pr(model: &str, key: &str, summary: &str, diff: &str) -> String {
    complete(model, &pr_review_prompt(key, summary, diff))
}

/// Build an Indonesian prompt asking Gemma, as a QA assistant, to DRAFT test
/// cases from a PR diff. The Jira ticket is often empty, so the test cases must
/// be based on the actual code change (the diff). Uses the SAME strict
/// one-per-line pipe format as [`test_cases_prompt`]:
/// `Judul | Langkah | Hasil yang diharapkan`. The diff is truncated to
/// [`MAX_DIFF_CHARS`]; when cut a "(diff dipotong)" note is appended.
pub fn test_cases_from_diff_prompt(key: &str, summary: &str, diff: &str) -> String {
    let (diff_text, truncated) = if diff.chars().count() > MAX_DIFF_CHARS {
        let cut: String = diff.chars().take(MAX_DIFF_CHARS).collect();
        (cut, true)
    } else {
        (diff.to_string(), false)
    };
    let note = if truncated { "\n(diff dipotong)" } else { "" };

    format!(
        "Kamu adalah asisten QA. Buatkan test case berdasarkan PERUBAHAN KODE \
         (diff Pull Request) di bawah ini. Tiket Jira sering kosong, jadi dasarkan \
         test case pada perubahan kode yang sebenarnya, bukan cuma ringkasan tiket.\n\n\
         Tiket: {key}\n\
         Ringkasan: {summary}\n\n\
         Diff PR:\n\
         ```diff\n{diff_text}{note}\n```\n\n\
         ATURAN OUTPUT (wajib diikuti):\n\
         - Buat 3 sampai 8 test case.\n\
         - Satu test case per baris.\n\
         - Format tiap baris PERSIS: Judul | Langkah | Hasil yang diharapkan\n\
         - Pakai tanda pipa (|) sebagai pemisah ketiga bagian.\n\
         - JANGAN pakai penomoran, bullet, atau teks tambahan apa pun.\n\
         - Tulis dalam bahasa Indonesia.\n\n\
         Contoh:\n\
         Validasi input kosong | Kirim form tanpa isi field wajib | Muncul pesan error validasi\n\n\
         Sekarang tulis test case-nya:"
    )
}

/// Convenience: ask the local model to draft test cases from a PR diff.
pub fn generate_test_cases_from_diff(
    model: &str,
    key: &str,
    summary: &str,
    diff: &str,
) -> Vec<(String, String, String)> {
    parse_test_cases(&complete(model, &test_cases_from_diff_prompt(key, summary, diff)))
}

/// POST a prompt to LM Studio and return the model's reply.
///
/// Degrades gracefully: any failure (LM Studio down, timeout, bad payload)
/// returns [`AI_UNAVAILABLE`] instead of erroring or panicking.
pub fn complete(model: &str, prompt: &str) -> String {
    let body = build_chat_request(model, prompt);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(_) => return AI_UNAVAILABLE.to_string(),
    };

    let resp = match client.post(LM_STUDIO_URL).json(&body).send() {
        Ok(r) => r,
        Err(_) => return AI_UNAVAILABLE.to_string(),
    };

    let text = match resp.text() {
        Ok(t) => t,
        Err(_) => return AI_UNAVAILABLE.to_string(),
    };

    match parse_chat_response(&text) {
        Ok(content) => content,
        Err(_) => AI_UNAVAILABLE.to_string(),
    }
}

/// Convenience: summarize the workday via the local model.
pub fn daily_summary(model: &str, blocks: &[ActivityBlock], tickets: &[JiraTicket]) -> String {
    complete(model, &daily_summary_prompt(blocks, tickets))
}

/// Convenience: explain story-point fairness via the local model.
pub fn explain_fairness(model: &str, items: &[(String, Assessment)]) -> String {
    complete(model, &explain_fairness_prompt(items))
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

    #[test]
    fn parse_models_extracts_ids() {
        let fixture = r#"{"data":[{"id":"gemma-4-e4b-it","object":"model"},
                                  {"id":"text-embedding-nomic","object":"model"}],
                          "object":"list"}"#;
        let ids = parse_models(fixture).unwrap();
        assert_eq!(ids, vec!["gemma-4-e4b-it", "text-embedding-nomic"]);
    }

    #[test]
    fn parse_models_unexpected_shape_is_empty() {
        assert!(parse_models("{}").unwrap().is_empty());
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
    fn test_cases_prompt_contains_key_facts() {
        let p = test_cases_prompt("Login bug di halaman utama", "QAT-7");
        assert!(p.contains("QAT-7"));
        assert!(p.contains("Login bug di halaman utama"));
        // Indonesian + the pipe format.
        assert!(p.contains("test case"));
        assert!(p.contains("Judul | Langkah | Hasil"));
    }

    #[test]
    fn parse_test_cases_extracts_clean_tuples() {
        let fixture = "\
Login valid | Buka login, isi kredensial benar, klik Masuk | Masuk ke dashboard
Login invalid | Isi password salah | Muncul pesan error
- Logout | Klik tombol Logout | Kembali ke halaman login

Cuma judul tanpa pipa
1. Lupa password | Klik 'Lupa password' | Email reset terkirim";

        let got = parse_test_cases(fixture);
        assert_eq!(got.len(), 5);
        assert_eq!(
            got[0],
            (
                "Login valid".to_string(),
                "Buka login, isi kredensial benar, klik Masuk".to_string(),
                "Masuk ke dashboard".to_string()
            )
        );
        // Bullet stripped.
        assert_eq!(got[2].0, "Logout");
        // A line with only a title still yields a tuple (steps/expected empty).
        assert_eq!(
            got[3],
            ("Cuma judul tanpa pipa".to_string(), String::new(), String::new())
        );
        // Numbering stripped.
        assert_eq!(got[4].0, "Lupa password");
    }

    #[test]
    fn parse_test_cases_skips_blank_and_titleless() {
        let fixture = "\n   \n| langkah | hasil\n\nValid | a | b\n";
        let got = parse_test_cases(fixture);
        // The "| langkah | hasil" line has an empty title and is skipped.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "Valid");
    }

    #[test]
    fn pr_review_prompt_contains_key_facts() {
        let diff = "diff --git a/login.ts b/login.ts\n+const x = retry();";
        let p = pr_review_prompt("QAT-7", "Login bug di halaman utama", diff);
        assert!(p.contains("QAT-7"));
        // Part of the diff is embedded.
        assert!(p.contains("retry()"));
        // Indonesian instruction word.
        assert!(p.contains("dites") || p.contains("Ringkasan"));
    }

    #[test]
    fn test_cases_from_diff_prompt_contains_key_facts() {
        let diff = "diff --git a/login.ts b/login.ts\n+const x = retry();";
        let p = test_cases_from_diff_prompt("QAT-7", "Login bug di halaman utama", diff);
        assert!(p.contains("QAT-7"));
        // Part of the diff is embedded.
        assert!(p.contains("retry()"));
        // Instruction word + the pipe format.
        assert!(p.contains("test case"));
        assert!(p.contains("Judul | Langkah | Hasil"));
    }

    #[test]
    fn test_cases_from_diff_prompt_truncates_long_diff() {
        let diff = "x".repeat(MAX_DIFF_CHARS + 500);
        let p = test_cases_from_diff_prompt("QAT-1", "summary", &diff);
        assert!(p.contains("(diff dipotong)"));
        assert!(!p.contains(&"x".repeat(MAX_DIFF_CHARS + 1)));
    }

    #[test]
    fn pr_review_prompt_truncates_long_diff() {
        let diff = "x".repeat(MAX_DIFF_CHARS + 500);
        let p = pr_review_prompt("QAT-1", "summary", &diff);
        assert!(p.contains("(diff dipotong)"));
        // The embedded diff body is capped (prompt is prefix + capped diff + suffix).
        assert!(!p.contains(&"x".repeat(MAX_DIFF_CHARS + 1)));
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
