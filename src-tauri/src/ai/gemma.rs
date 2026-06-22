//! Google Gemini client (OpenAI-compatible API).
//!
//! Pure request-builder / response-parser / prompt-builders are unit-tested.
//! The actual HTTP call ([`complete`]) is a thin wrapper that degrades
//! gracefully (never panics) when Gemini cannot be reached.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::core::fairness::{Assessment, Fairness};
use crate::core::types::ActivityBlock;
use crate::db::QaActivity;
use crate::integrations::jira::JiraTicket;

/// Google Gemini OpenAI-compatible chat endpoint.
pub const GEMINI_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";

/// The single Gemini model the app uses (not user-configurable).
pub const GEMINI_MODEL: &str = "gemini-2.5-flash";

/// Cap on how many activity blocks we list in a summary prompt.
const MAX_BLOCKS: usize = 20;

/// Friendly Indonesian message shown when the AI cannot be reached.
const AI_UNAVAILABLE: &str =
    "(AI tidak tersedia — cek API key Gemini di Settings)";

/// Where to send a chat-completion request: the Gemini endpoint URL, the Bearer
/// API key, and a model id. Speaks the OpenAI-compatible chat API.
#[derive(Debug, Clone)]
pub struct AiTarget {
    pub url: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl AiTarget {
    /// Google Gemini cloud target (Bearer auth).
    pub fn gemini(api_key: &str, model: &str) -> Self {
        Self {
            url: GEMINI_URL.to_string(),
            api_key: Some(api_key.to_string()),
            model: model.to_string(),
        }
    }
}

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
        return Err(anyhow!("Gemini error: {msg}"));
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

/// Build an Indonesian prompt for the daily QA summary from the actions logged
/// today (status moves + point sets) and the current board snapshot. Unlike
/// [`daily_summary_prompt`] this needs no desktop activity tracking — it reports
/// what the QA actually did in the app plus where the board stands.
pub fn qa_summary_prompt(activities: &[QaActivity], tickets: &[JiraTicket]) -> String {
    let fmt = |p: f64| p.to_string(); // f64 Display prints 3.0 as "3", 3.5 as "3.5"

    let mut s = String::new();
    s.push_str(
        "Kamu adalah asisten QA. Buat ringkasan kerja QA harian dalam bahasa Indonesia \
         yang singkat dan jelas, berdasarkan AKSI yang dilakukan hari ini dan kondisi \
         board saat ini. Sebutkan apa yang dikerjakan (tiket digeser/di-pass/di-fail), \
         dan total poin yang diselesaikan.\n\n",
    );

    s.push_str("Aksi hari ini:\n");
    if activities.is_empty() {
        s.push_str("- (belum ada aksi tercatat hari ini)\n");
    } else {
        for a in activities {
            match a.kind.as_str() {
                "points" => {
                    let p = a.points.map(fmt).unwrap_or_else(|| "-".to_string());
                    s.push_str(&format!("- {} ({}): set {} poin\n", a.ticket_key, a.summary, p));
                }
                _ => s.push_str(&format!(
                    "- {} ({}): {} -> {}\n",
                    a.ticket_key, a.summary, a.from_status, a.to_status
                )),
            }
        }
    }

    s.push_str("\nKondisi board sekarang (status: jumlah tiket, poin):\n");
    if tickets.is_empty() {
        s.push_str("- (tidak ada tiket)\n");
    } else {
        use std::collections::BTreeMap;
        let mut by_status: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
        let mut total = 0.0;
        for t in tickets {
            let e = by_status.entry(t.status.as_str()).or_insert((0, 0.0));
            e.0 += 1;
            let p = t.story_points.unwrap_or(0.0);
            e.1 += p;
            total += p;
        }
        for (status, (count, pts)) in &by_status {
            s.push_str(&format!("- {}: {} tiket, {} poin\n", status, count, fmt(*pts)));
        }
        s.push_str(&format!("Total story point semua tiket: {}\n", fmt(total)));
    }

    s.push_str("\nBuat ringkasan kerja QA harian dari data di atas.");
    s
}

/// Convenience: generate the daily QA summary via the configured model.
pub fn qa_summary(target: &AiTarget, activities: &[QaActivity], tickets: &[JiraTicket]) -> String {
    complete(target, &qa_summary_prompt(activities, tickets))
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
    target: &AiTarget,
    key: &str,
    summary: &str,
) -> Vec<(String, String, String)> {
    parse_test_cases(&complete(target, &test_cases_prompt(summary, key)))
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
pub fn review_pr(target: &AiTarget, key: &str, summary: &str, diff: &str) -> String {
    complete(target, &pr_review_prompt(key, summary, diff))
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
    target: &AiTarget,
    key: &str,
    summary: &str,
    diff: &str,
) -> Vec<(String, String, String)> {
    parse_test_cases(&complete(target, &test_cases_from_diff_prompt(key, summary, diff)))
}

/// POST a prompt to the configured AI [`AiTarget`] and return the model's reply.
///
/// Sends the same OpenAI-compatible body to either LM Studio (no auth) or
/// Gemini (Bearer auth) depending on `target`. Degrades gracefully: any failure
/// (provider down, timeout, bad payload) returns [`AI_UNAVAILABLE`] instead of
/// erroring or panicking.
pub fn complete(target: &AiTarget, prompt: &str) -> String {
    post_chat(target, build_chat_request(&target.model, prompt))
}

/// POST a pre-built OpenAI-compatible body to `target` and return the reply.
/// Shared by [`complete`] (text) and [`generate_bug_report`] (multimodal); same
/// graceful-degrade contract — any failure returns [`AI_UNAVAILABLE`].
fn post_chat(target: &AiTarget, body: Value) -> String {
    let client = crate::net::client();

    let mut req = client.post(&target.url).json(&body);
    if let Some(key) = &target.api_key {
        req = req.bearer_auth(key);
    }

    let resp = match req.send() {
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

// ---------------------------------------------------------------------------
// Ticket Builder prompts
// ---------------------------------------------------------------------------

/// Prompt Gemini to parse a free-form QA ticket blob into structured JSON rows.
pub fn parse_ticket_rows_prompt(blob: &str) -> String {
    format!(
        "You parse a QA engineer's free-form list of pull requests into JSON. The text \
         may be in any format. Extract the Epic key, the app label, and one row per PR.\n\n\
         Return ONLY valid JSON (no prose, no code fences) in EXACTLY this shape:\n\
         {{\n\
         \x20 \"epic\": \"<epic key like QAT-3423, or empty>\",\n\
         \x20 \"app\": \"<short app label like GTG, or empty>\",\n\
         \x20 \"rows\": [\n\
         \x20\x20 {{\n\
         \x20\x20\x20 \"source_ticket\": \"<linked Jira key like USSTOCK-2835, or empty>\",\n\
         \x20\x20\x20 \"title\": \"<the change title>\",\n\
         \x20\x20\x20 \"pr_number\": \"<github PR number, digits only>\",\n\
         \x20\x20\x20 \"pr_url\": \"<full github PR url>\",\n\
         \x20\x20\x20 \"assignee\": \"<person name from the @mention>\"\n\
         \x20\x20 }}\n\
         \x20 ]\n\
         }}\n\n\
         Rules:\n\
         - One row per PR line. Keep the original title text.\n\
         - source_ticket only if a Jira ticket is explicitly linked; else empty string.\n\
         - assignee is the name in the @mention (e.g. \"Reva Anggada (Reva)\").\n\
         - Ignore instruction lines like \"assign the created ticket ...\".\n\n\
         Input:\n{blob}"
    )
}

/// Extract a JSON object/array from a model reply that may wrap it in code
/// fences or prose. Returns the substring from the first `{`/`[` to the matching
/// last `}`/`]`; falls back to the trimmed input.
pub fn extract_json(raw: &str) -> &str {
    let t = raw.trim();
    let t = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = t.find(['{', '[']);
    let end = t.rfind(['}', ']']);
    match (start, end) {
        (Some(s), Some(e)) if e >= s => &t[s..=e],
        _ => t,
    }
}

/// Prompt Gemini to draft acceptance criteria for a PR (used when there is no
/// source ticket). Asks for a short numbered list, one criterion per line.
pub fn generate_ac_prompt(pr_title: &str, pr_body: &str, pr_number: &str) -> String {
    let body = if pr_body.chars().count() > 4000 {
        pr_body.chars().take(4000).collect::<String>()
    } else {
        pr_body.to_string()
    };
    format!(
        "You are a QA engineer. Write concise QA acceptance criteria for the following \
         GitHub pull request (PR #{pr_number}). Output ONLY a numbered list, one acceptance \
         criterion per line (e.g. \"1. ...\"), 3 to 6 items, in English. No preamble.\n\n\
         PR title: {pr_title}\n\
         PR description:\n{body}"
    )
}

/// Split a model's numbered acceptance-criteria reply into clean lines (drops
/// leading numbering / bullets). Reuses [`strip_leading_marker`].
pub fn parse_ac_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| strip_leading_marker(l.trim()).to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Probe the AI target with a minimal request, surfacing the real error (bad
/// key, network, quota) instead of the swallowed [`AI_UNAVAILABLE`]. Used by the
/// Settings "Test Connection" button.
pub fn test_connection(target: &AiTarget) -> Result<(), String> {
    if target.api_key.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err("API key Gemini belum diisi".into());
    }
    let client = crate::net::client();
    let mut req = client.post(&target.url).json(&build_chat_request(&target.model, "ping"));
    if let Some(key) = &target.api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().map_err(|e| format!("gagal konek ke Gemini: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(msg);
    }
    parse_chat_response(&text).map(|_| ()).map_err(|e| e.to_string())
}

/// Convenience: generate a structured bug report from free-form text and an
/// optional screenshot (bare base64 or `data:` URL). Returns `(title, body, raw)`.
/// Thin HTTP wrapper around the tested [`build_bug_prompt`] / [`build_vision_request`]
/// / [`parse_title_and_body`]; not unit-tested.
pub fn generate_bug_report(
    target: &AiTarget,
    text: &str,
    image_base64: Option<&str>,
    language: &str,
    sections: &[String],
) -> (String, String, String) {
    let system = build_bug_prompt(language, sections);
    let user = if text.trim().is_empty() {
        "Generate a bug report from the attached image.".to_string()
    } else {
        format!("User description:\n{}", text.trim())
    };
    let combined = format!("{system}\n\n{user}");
    let raw = post_chat(target, build_vision_request(&target.model, &combined, image_base64));
    let (title, body) = parse_title_and_body(&raw);
    (title, body, raw)
}

/// Convenience: summarize the workday via the configured model.
pub fn daily_summary(target: &AiTarget, blocks: &[ActivityBlock], tickets: &[JiraTicket]) -> String {
    complete(target, &daily_summary_prompt(blocks, tickets))
}

/// Convenience: explain story-point fairness via the configured model.
pub fn explain_fairness(target: &AiTarget, items: &[(String, Assessment)]) -> String {
    complete(target, &explain_fairness_prompt(items))
}

/// Bug-report section catalog: `(key, English label, model instruction)`.
/// Labels stay English regardless of output language (matches the prompt rule).
const BUG_SECTIONS: &[(&str, &str, &str)] = &[
    ("issue", "Issue", "Clear, concise description of the bug/issue. Include what happens, where it happens, and any relevant context."),
    ("steps", "Steps to Reproduce", "Numbered list of steps to reproduce the bug. Be specific about actions, inputs, and navigation."),
    ("expected", "Expected Result", "What should happen instead. Be specific about the correct behavior."),
    ("actual", "Actual Result", "What actually happens. Describe the incorrect behavior, error messages, or visual glitches observed."),
    ("severity", "Severity / Priority", "Assess the severity (Critical, Major, Minor, Trivial) and priority (High, Medium, Low) based on the impact."),
    ("environment", "Environment", "Infer or suggest the likely environment details (device, OS, browser/app version, etc.) based on the context provided."),
    ("preconditions", "Preconditions", "Any conditions or setup that must be in place before the bug can be reproduced."),
];

/// Sections included when the caller does not specify any.
pub const DEFAULT_BUG_SECTIONS: &[&str] =
    &["issue", "steps", "expected", "actual", "severity", "environment"];

/// Build the system prompt for the Bug Writer. Emits a `TITLE:` line followed by
/// the selected sections; output is written in `language`, but the section labels
/// (and TITLE) stay English. Unknown section keys are ignored.
pub fn build_bug_prompt(language: &str, sections: &[String]) -> String {
    let picked: Vec<&(&str, &str, &str)> = BUG_SECTIONS
        .iter()
        .filter(|(key, _, _)| sections.iter().any(|s| s == key))
        .collect();
    let format_lines = picked
        .iter()
        .map(|(_, label, instr)| format!("{label}:\n[{instr}]"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let labels = picked
        .iter()
        .map(|(_, label, _)| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "You are an expert QA engineer assistant. Your job is to generate a structured \
         bug report from the user's input.\n\n\
         The user will provide one or more of the following:\n\
         - A text description of the bug or scenario (may be in any language)\n\
         - A screenshot/image showing the issue\n\n\
         Based on all provided inputs, generate a bug report in EXACTLY this format:\n\n\
         TITLE: <one short, action-oriented bug title — max 80 chars, no period at end, \
         suitable as a Jira issue Summary>\n\n\
         {format_lines}\n\n\
         Rules:\n\
         - IMPORTANT: Write the ENTIRE output in {language}. The user's input may be in any \
         language, but your output MUST be in {language}.\n\
         - The TITLE line MUST be the very first line of your response, prefixed with \"TITLE: \"\n\
         - TITLE should NOT include \"Bug:\", \"Issue:\", \"[BUG]\" or similar prefixes\n\
         - Leave one blank line after TITLE before the first section\n\
         - Write in a professional, clear tone; be specific and actionable\n\
         - If an image is provided, describe what you observe in the context of the bug\n\
         - If only an image is provided without text, infer the issue from visual context\n\
         - Do NOT include any sections beyond: TITLE and {labels}\n\
         - The section labels (TITLE, {labels}) MUST stay in English regardless of output language"
    )
}

/// Parse a Bug Writer response into `(title, body)`. The title is taken from a
/// leading `TITLE: ...` line (surrounding quotes stripped); when absent the title
/// is empty and the body is the full text. Blank lines around the title are dropped.
pub fn parse_title_and_body(raw: &str) -> (String, String) {
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut title = String::new();
    let mut body_start = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            body_start = i + 1;
            continue;
        }
        if let Some(rest) = strip_title_prefix(trimmed) {
            title = rest.trim_matches(|c| c == '"' || c == '\'').trim().to_string();
            body_start = i + 1;
        }
        break; // only the first non-empty line can be a title
    }
    while body_start < lines.len() && lines[body_start].trim().is_empty() {
        body_start += 1;
    }
    (title, lines[body_start..].join("\n"))
}

/// If `line` begins with a case-insensitive `TITLE:` prefix, return the non-empty
/// remainder; otherwise `None`.
fn strip_title_prefix(line: &str) -> Option<&str> {
    let head: String = line.chars().take(5).collect::<String>().to_ascii_lowercase();
    if head != "title" {
        return None;
    }
    let rest = line[5..].trim_start().strip_prefix(':')?.trim_start();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// Build an OpenAI-compatible chat-completion request that optionally carries an
/// image. With `image_base64`, the `content` becomes a multimodal array
/// (`text` + `image_url`); a bare base64 string is wrapped into a PNG data URL,
/// while an existing `data:` URL is passed through. Without an image, `content`
/// is a plain string (identical to [`build_chat_request`]).
pub fn build_vision_request(model: &str, prompt: &str, image_base64: Option<&str>) -> Value {
    let content = match image_base64 {
        Some(img) => {
            let url = if img.starts_with("data:") {
                img.to_string()
            } else {
                format!("data:image/png;base64,{img}")
            };
            json!([
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": url } }
            ])
        }
        None => json!(prompt),
    };
    json!({
        "model": model,
        "messages": [ { "role": "user", "content": content } ],
        "temperature": 0.3,
        "stream": false
    })
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
    fn ticket_rows_prompt_embeds_blob_and_asks_json() {
        let blob = "Epic: QAT-3423\nUAT GTG\nSocial #3197 @Reva";
        let p = parse_ticket_rows_prompt(blob);
        assert!(p.contains("QAT-3423"));
        assert!(p.contains("JSON"));
        assert!(p.contains("rows"));
    }

    #[test]
    fn extract_json_strips_code_fences() {
        assert_eq!(extract_json("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(extract_json("{\"a\":1}"), "{\"a\":1}");
        // Leading prose before the object is dropped.
        assert_eq!(extract_json("Here:\n{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn generate_ac_prompt_contains_title_and_pr() {
        let p = generate_ac_prompt("feat(ipo): surface IPO stocks", "adds IPO to search", "3200");
        assert!(p.contains("feat(ipo)"));
        assert!(p.contains("3200"));
        assert!(p.contains("acceptance criteria") || p.contains("Acceptance Criteria"));
    }

    #[test]
    fn qa_summary_prompt_lists_actions_and_board() {
        let acts = vec![
            crate::db::QaActivity {
                ts: "2026-06-22T09:00:00+07:00".into(),
                ticket_key: "QAT-1".into(),
                summary: "Support CX Account Opening".into(),
                kind: "transition".into(),
                from_status: "Ready for QA".into(),
                to_status: "QA In Progress".into(),
                points: None,
            },
            crate::db::QaActivity {
                ts: "2026-06-22T10:00:00+07:00".into(),
                ticket_key: "QAT-2".into(),
                summary: "Deposit Method".into(),
                kind: "points".into(),
                from_status: String::new(),
                to_status: String::new(),
                points: Some(3.0),
            },
        ];
        let tickets = vec![ticket("QAT-1", "Support CX Account Opening")];
        let p = qa_summary_prompt(&acts, &tickets);
        // Actions present.
        assert!(p.contains("QAT-1"));
        assert!(p.contains("QA In Progress"));
        assert!(p.contains("3")); // points set
        // Board snapshot + Indonesian instruction.
        assert!(p.contains("board"));
        assert!(p.contains("ringkasan"));
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
    fn blocking_http_from_async_context_does_not_panic() {
        // Regression: blocking HTTP issued DIRECTLY inside the Tauri async runtime
        // panics ("Cannot drop a runtime in a context where blocking is not
        // allowed"), which left the spinner stuck forever. The fix is to run the
        // blocking work via spawn_blocking (a blocking-pool thread where blocking
        // IS allowed). This mirrors how the async commands now call their bodies.
        let mut target = AiTarget::gemini("dummy-key", "model");
        target.url = "http://127.0.0.1:9/".to_string(); // nothing listening → fast refuse
        let res = tauri::async_runtime::block_on(async move {
            tauri::async_runtime::spawn_blocking(move || test_connection(&target)).await
        });
        // No panic; the inner call returns Err (connection refused).
        assert!(res.expect("join ok").is_err());
    }

    #[test]
    fn parse_title_and_body_extracts_title_prefix() {
        let raw = "TITLE: Place Order gagal (500)\n\nIssue: muncul error 500 saat checkout.\nSteps to Reproduce:\n1. buka cart";
        let (title, body) = parse_title_and_body(raw);
        assert_eq!(title, "Place Order gagal (500)");
        assert!(body.starts_with("Issue:"));
        assert!(!body.contains("TITLE:"));
    }

    #[test]
    fn parse_title_and_body_no_prefix_keeps_full_body() {
        let raw = "Issue: error 500\nActual Result: HTTP 500";
        let (title, body) = parse_title_and_body(raw);
        assert_eq!(title, "");
        assert_eq!(body, raw);
    }

    #[test]
    fn parse_title_and_body_strips_quotes_and_leading_blanks() {
        let raw = "\n\nTITLE: \"Login button stays disabled\"\n\nIssue: ...";
        let (title, body) = parse_title_and_body(raw);
        assert_eq!(title, "Login button stays disabled");
        assert!(body.starts_with("Issue:"));
    }

    #[test]
    fn build_bug_prompt_includes_selected_sections_and_rules() {
        let sections = vec!["issue".to_string(), "steps".to_string(), "severity".to_string()];
        let p = build_bug_prompt("Indonesia", &sections);
        // Selected section labels present.
        assert!(p.contains("Issue"));
        assert!(p.contains("Steps to Reproduce"));
        assert!(p.contains("Severity / Priority"));
        // Unselected section label absent.
        assert!(!p.contains("Expected Result"));
        // Output-language rule + TITLE rule present.
        assert!(p.contains("Indonesia"));
        assert!(p.contains("TITLE"));
    }

    #[test]
    fn build_bug_prompt_ignores_unknown_section_keys() {
        let sections = vec!["issue".to_string(), "bogus".to_string()];
        let p = build_bug_prompt("English", &sections);
        assert!(p.contains("Issue"));
        assert!(!p.contains("bogus"));
    }

    #[test]
    fn build_vision_request_without_image_uses_string_content() {
        let v = build_vision_request("gemini-2.0-flash", "halo", None);
        assert_eq!(v["model"], "gemini-2.0-flash");
        assert_eq!(v["messages"][0]["content"], "halo");
        assert_eq!(v["stream"], false);
    }

    #[test]
    fn build_vision_request_with_image_uses_array_content() {
        let v = build_vision_request("gemini-2.0-flash", "lihat ini", Some("AAAA"));
        let content = &v["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "lihat ini");
        assert_eq!(content[1]["type"], "image_url");
        // Bare base64 is wrapped into a data URL.
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn build_vision_request_preserves_existing_data_url() {
        let v = build_vision_request("m", "x", Some("data:image/jpeg;base64,ZZZZ"));
        let content = &v["messages"][0]["content"];
        assert_eq!(content[1]["image_url"]["url"], "data:image/jpeg;base64,ZZZZ");
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
