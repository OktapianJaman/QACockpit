//! Shared blocking HTTP client.
//!
//! A single, process-wide `reqwest::blocking::Client` that is initialized once
//! and never dropped. This matters because each blocking client owns an internal
//! tokio runtime; dropping a per-call client *inside a Tauri async command*
//! (which runs on a tokio worker) panics with "Cannot drop a runtime in a
//! context where blocking is not allowed". Sharing one never-dropped client
//! avoids that panic entirely — and reuses connections as a bonus.

use std::sync::OnceLock;
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
