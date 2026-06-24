//! Shared blocking HTTP client.
//!
//! A single, process-wide `reqwest::blocking::Client` that is initialized once
//! and never dropped. This matters because each blocking client owns an internal
//! tokio runtime; dropping a per-call client *inside a Tauri async command*
//! (which runs on a tokio worker) panics with "Cannot drop a runtime in a
//! context where blocking is not allowed". Sharing one never-dropped client
//! avoids that panic entirely — and reuses connections as a bonus.

use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

/// The process-wide blocking HTTP client. The 120s timeout covers slow AI
/// (Gemini) calls; other callers finish well under it.
pub fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build shared reqwest client")
    })
}

/// Per-attempt timeout for interactive (non-AI) reads. Much tighter than the
/// client-wide 120s so the UI doesn't hang on a stalled connection.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Total send attempts (1 initial + retries) for idempotent reads.
const MAX_ATTEMPTS: u32 = 3;

/// Send an **idempotent** request (GET) with a tight per-attempt timeout and
/// retry-with-backoff on transient failures: connection/timeout errors and
/// retryable statuses (429, 502, 503, 504). The last response/error is returned
/// once attempts are exhausted.
///
/// Do NOT use this for writes (POST/PUT/DELETE): a response timeout could double
/// -submit (duplicate Jira issue, duplicate attachment). Writes keep plain
/// `.send()`.
pub fn send_retrying(
    req: reqwest::blocking::RequestBuilder,
) -> reqwest::Result<reqwest::blocking::Response> {
    let req = req.timeout(READ_TIMEOUT);
    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        // `try_clone` fails only for streaming bodies; GETs always clone.
        let this = match req.try_clone() {
            Some(c) => c,
            None => return req.send(),
        };
        match this.send() {
            Ok(resp) => {
                let s = resp.status();
                let retryable = s == 429 || s == 502 || s == 503 || s == 504;
                if retryable && attempt + 1 < MAX_ATTEMPTS {
                    sleep(backoff(attempt));
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                let transient = e.is_timeout() || e.is_connect();
                if transient && attempt + 1 < MAX_ATTEMPTS {
                    last_err = Some(e);
                    sleep(backoff(attempt));
                    continue;
                }
                return Err(e);
            }
        }
    }
    // Unreachable in practice (the loop returns), but satisfy the type.
    Err(last_err.expect("retry loop exhausted without an error"))
}

/// Exponential backoff: 300ms, 900ms, …
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(300 * 3u64.pow(attempt))
}
