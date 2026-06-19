# QA Cockpit v1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a macOS desktop app that records work activity in the background, syncs Jira/GitHub, and flags under/over-pointed tickets using a fixed rate of 1 hour = 2 points.

**Architecture:** Tauri app. Rust backend = recorder (active window + idle sampling), SQLite storage, Jira/GitHub HTTP clients, and a local Gemma (LM Studio) client. All business logic lives in pure, unit-tested Rust modules that take plain data in and return plain data out — I/O is a thin shell around them. Web UI (vanilla TS + Vite) renders a single dashboard from data exposed via Tauri commands.

**Tech Stack:** Tauri v2, Rust, `rusqlite` (bundled SQLite), `reqwest` (HTTP), `serde`, `chrono`, `active-win-pos-rs` (active window), `core-graphics` (idle time), Vite + TypeScript (frontend).

---

## Design reference

Full design: `docs/plans/2026-06-19-qa-cockpit-design.md`. Read it first. Key locked decisions:
- Recording = active **app + window title** only (no screenshots).
- Points: **1 hour worked = 2 points**, flat for all work types in v1.
- Everything stored **locally** in SQLite. No cloud.
- Gemma only for narrative text + ambiguous matching; all math is plain Rust + cached.

---

## ⚠️ macOS gotchas to know up front

1. **Window title needs permission.** Getting the active app name is free, but reading the window *title* on modern macOS requires **Screen Recording** permission (System Settings → Privacy & Security → Screen Recording). The app must detect when permission is missing and tell the user how to grant it. App name always works even without it.
2. **Idle time** comes from `CGEventSourceSecondsSinceLastEventType` (Core Graphics) — no special permission needed.
3. Background sampling should be lightweight (a timer every ~5s), not a busy loop.

---

## Milestone 0 — Scaffolding

### Task 0.1: Create the Tauri project

**Files:**
- Create: whole project skeleton under `~/Documents/Important/QACockpit/`

**Step 1:** Confirm prerequisites are installed.

Run:
```bash
rustc --version && cargo --version && node --version
```
Expected: all three print versions. If Rust is missing, install via `https://rustup.rs`. If Node missing, install Node 18+.

**Step 2:** Scaffold a Tauri v2 app with the vanilla-TS frontend, *into the existing folder*.

Run:
```bash
cd ~/Documents/Important/QACockpit
npm create tauri-app@latest -- --template vanilla-ts --manager npm --identifier site.hexalabs.qacockpit .
```
Expected: creates `src/` (frontend), `src-tauri/` (Rust), `package.json`. If it refuses because the folder isn't empty, scaffold into a temp dir and move files in, keeping `docs/` and `.git/`.

**Step 3:** Install JS deps and verify the app builds & launches.

Run:
```bash
npm install && npm run tauri dev
```
Expected: a desktop window opens showing the default Tauri template. Close it.

**Step 4: Commit**
```bash
git add -A && git commit -m "chore: scaffold Tauri v2 vanilla-ts app"
```

### Task 0.2: Add Rust dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`

**Step 1:** Add to `[dependencies]` in `src-tauri/Cargo.toml`:
```toml
rusqlite = { version = "0.31", features = ["bundled"] }
reqwest = { version = "0.12", features = ["json", "blocking"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
active-win-pos-rs = "0.8"
core-graphics = "0.23"
anyhow = "1"
```

**Step 2:** Verify it compiles.

Run: `cd src-tauri && cargo build`
Expected: builds (downloads crates). Warnings OK, no errors.

**Step 3: Commit**
```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock && git commit -m "chore: add rust deps"
```

---

## Milestone 1 — Pure logic core (TDD, the heart)

> These modules have NO I/O. They are the most important and most testable part.
> Create `src-tauri/src/core/mod.rs` and declare `pub mod ...;` for each new module.
> Wire `mod core;` into `src-tauri/src/lib.rs`.

### Task 1.1: Domain types

**Files:**
- Create: `src-tauri/src/core/mod.rs`
- Create: `src-tauri/src/core/types.rs`

**Step 1:** Write the types (no test needed — pure data).
```rust
// src-tauri/src/core/types.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One raw sample taken by the recorder every ~5s.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub at: DateTime<Utc>,
    pub app: String,
    pub title: String,
    pub idle_seconds: u64,
}

/// A merged span of continuous work in one window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityBlock {
    pub app: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub is_idle: bool,
}

impl ActivityBlock {
    pub fn duration_secs(&self) -> i64 {
        (self.end - self.start).num_seconds().max(0)
    }
}
```

**Step 2:** Add to `src-tauri/src/core/mod.rs`:
```rust
pub mod types;
pub mod sessions;
pub mod matching;
pub mod fairness;
```
(`sessions`, `matching`, `fairness` files come in later tasks — they won't compile until created, so create empty stubs `// placeholder` now to keep the tree building, or add the `pub mod` lines as each task lands. Prefer adding each line when its file exists.)

**Step 3:** Wire into `src-tauri/src/lib.rs`: add `mod core;` near the top.

**Step 4: Commit**
```bash
git add src-tauri/src/core/ src-tauri/src/lib.rs && git commit -m "feat(core): domain types"
```

### Task 1.2: Merge samples into activity blocks

**Files:**
- Create: `src-tauri/src/core/sessions.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Rule:** consecutive samples with the same `(app, title)` merge into one block. A sample with `idle_seconds >= idle_threshold` marks the block as idle. A gap between samples larger than `2 * sample_interval` closes the current block.

**Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn s(secs: i64, app: &str, title: &str, idle: u64) -> Sample {
        Sample { at: Utc.timestamp_opt(secs, 0).unwrap(), app: app.into(), title: title.into(), idle_seconds: idle }
    }

    #[test]
    fn merges_same_window_into_one_block() {
        let samples = vec![
            s(0, "VS Code", "login_test.dart", 0),
            s(5, "VS Code", "login_test.dart", 0),
            s(10, "VS Code", "login_test.dart", 0),
        ];
        let blocks = merge_samples(&samples, 5, 180);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].duration_secs(), 10);
        assert!(!blocks[0].is_idle);
    }

    #[test]
    fn splits_when_window_changes() {
        let samples = vec![
            s(0, "VS Code", "a", 0),
            s(5, "Chrome", "JIRA-1", 0),
        ];
        let blocks = merge_samples(&samples, 5, 180);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn marks_idle_block() {
        let samples = vec![ s(0, "VS Code", "a", 0), s(5, "VS Code", "a", 200) ];
        let blocks = merge_samples(&samples, 5, 180);
        // idle sample closes the active block and starts an idle one
        assert!(blocks.iter().any(|b| b.is_idle));
    }
}
```

**Step 2: Run, verify it fails**

Run: `cd src-tauri && cargo test sessions`
Expected: FAIL — `merge_samples` not found.

**Step 3: Implement**
```rust
use crate::core::types::{ActivityBlock, Sample};

/// Merge raw samples into activity blocks.
/// `interval` = expected seconds between samples; a gap > 2*interval closes a block.
/// `idle_threshold` = idle_seconds at/above which a sample is considered idle.
pub fn merge_samples(samples: &[Sample], interval: i64, idle_threshold: u64) -> Vec<ActivityBlock> {
    let mut blocks: Vec<ActivityBlock> = Vec::new();
    for sm in samples {
        let idle = sm.idle_seconds >= idle_threshold;
        let same = blocks.last().map_or(false, |b| {
            b.app == sm.app
                && b.title == sm.title
                && b.is_idle == idle
                && (sm.at - b.end).num_seconds() <= 2 * interval
        });
        if same {
            let last = blocks.last_mut().unwrap();
            last.end = sm.at;
        } else {
            blocks.push(ActivityBlock {
                app: sm.app.clone(),
                title: sm.title.clone(),
                start: sm.at,
                end: sm.at,
                is_idle: idle,
            });
        }
    }
    blocks
}
```

**Step 4: Run, verify pass**

Run: `cd src-tauri && cargo test sessions`
Expected: PASS (3 tests).

**Step 5: Commit**
```bash
git add src-tauri/src/core/sessions.rs src-tauri/src/core/mod.rs && git commit -m "feat(core): merge samples into activity blocks (TDD)"
```

### Task 1.3: Match a window/title to a Jira ticket key

**Files:**
- Create: `src-tauri/src/core/matching.rs`

**Rule (strongest → weakest):** explicit ticket key regex in title (`[A-Z]+-\d+`) wins; else caller-supplied branch string; else `None` (AI handles the rest later).

**Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_key_in_title() {
        assert_eq!(extract_ticket_key("JIRA-1234 - Login bug"), Some("JIRA-1234".to_string()));
        assert_eq!(extract_ticket_key("feature/ABC-9 work"), Some("ABC-9".to_string()));
    }

    #[test]
    fn returns_none_when_no_key() {
        assert_eq!(extract_ticket_key("Slack | general"), None);
    }

    #[test]
    fn picks_first_key_when_multiple() {
        assert_eq!(extract_ticket_key("AB-1 vs CD-2"), Some("AB-1".to_string()));
    }
}
```

**Step 2: Run, verify fail**

Run: `cd src-tauri && cargo test matching`
Expected: FAIL — `extract_ticket_key` not found.

**Step 3: Implement** (add `regex = "1"` to Cargo.toml deps first)
```rust
use regex::Regex;
use std::sync::OnceLock;

fn key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[A-Z][A-Z0-9]+-\d+\b").unwrap())
}

/// Extract the first Jira-style ticket key from a string, if any.
pub fn extract_ticket_key(text: &str) -> Option<String> {
    key_re().find(text).map(|m| m.as_str().to_string())
}
```

**Step 4: Run, verify pass**

Run: `cd src-tauri && cargo test matching`
Expected: PASS.

**Step 5: Commit**
```bash
git add src-tauri/src/core/matching.rs src-tauri/src/core/mod.rs src-tauri/Cargo.toml && git commit -m "feat(core): extract jira ticket key from text (TDD)"
```

### Task 1.4: Point-fairness calculation (THE killer feature)

**Files:**
- Create: `src-tauri/src/core/fairness.rs`

**Rule:** `deserved = hours_worked * 2`. Compare to `assigned` story points. Status thresholds: within ±20% (relative) AND within ±1 pt → Fair; deserved > assigned → UnderPointed; deserved < assigned → OverPointed.

**Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserved_is_two_per_hour() {
        // 6 hours = 21600 seconds
        assert_eq!(deserved_points(21600), 12.0);
        assert_eq!(deserved_points(1800), 1.0); // 30 min = 1 pt
    }

    #[test]
    fn flags_under_pointed() {
        let f = assess(21600, 3.0); // 6h => deserved 12, assigned 3
        assert_eq!(f.deserved, 12.0);
        assert_eq!(f.status, Fairness::UnderPointed);
    }

    #[test]
    fn flags_over_pointed() {
        let f = assess(7200, 8.0); // 2h => deserved 4, assigned 8
        assert_eq!(f.status, Fairness::OverPointed);
    }

    #[test]
    fn flags_fair_when_close() {
        let f = assess(10800, 6.0); // 3h => deserved 6, assigned 6
        assert_eq!(f.status, Fairness::Fair);
    }
}
```

**Step 2: Run, verify fail**

Run: `cd src-tauri && cargo test fairness`
Expected: FAIL.

**Step 3: Implement**
```rust
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
pub enum Fairness { Fair, UnderPointed, OverPointed }

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub deserved: f64,
    pub assigned: f64,
    pub status: Fairness,
}

/// 1 hour worked = 2 points.
pub fn deserved_points(worked_secs: i64) -> f64 {
    (worked_secs as f64 / 3600.0) * 2.0
}

pub fn assess(worked_secs: i64, assigned: f64) -> Assessment {
    let deserved = deserved_points(worked_secs);
    let diff = deserved - assigned;
    let rel = if assigned > 0.0 { (diff.abs()) / assigned } else { 1.0 };
    let status = if diff.abs() <= 1.0 || rel <= 0.20 {
        Fairness::Fair
    } else if diff > 0.0 {
        Fairness::UnderPointed
    } else {
        Fairness::OverPointed
    };
    Assessment { deserved, assigned, status }
}
```

**Step 4: Run, verify pass**

Run: `cd src-tauri && cargo test fairness`
Expected: PASS (4 tests).

**Step 5: Commit**
```bash
git add src-tauri/src/core/fairness.rs src-tauri/src/core/mod.rs && git commit -m "feat(core): point-fairness assessment (TDD)"
```

---

## Milestone 2 — Persistence (SQLite)

### Task 2.1: DB schema + open helper

**Files:**
- Create: `src-tauri/src/db/mod.rs`
- Create: `src-tauri/src/db/schema.sql`

**Step 1:** Write `schema.sql` with the tables from the design doc:
`activity_blocks`, `jira_tickets`, `pull_requests`, `ticket_time`, `notes`, `ai_summaries`. Each with sensible columns + primary keys. (Engineer: mirror the fields on the core types.)

**Step 2: Write a failing test** in `db/mod.rs` that opens an in-memory DB, runs the schema, inserts one `activity_block`, reads it back, asserts equality.

Run: `cd src-tauri && cargo test db`
Expected: FAIL.

**Step 3: Implement** `open(path) -> Result<Connection>` that runs `schema.sql` via `include_str!`, plus `insert_block` / `list_blocks_for_day`. Use `?` + `anyhow`.

**Step 4: Run, verify pass.** **Step 5: Commit.**

### Task 2.2: Persist computed ticket_time + fairness

**Files:** Modify `src-tauri/src/db/mod.rs`

TDD: write test → insert blocks for a day, run a `recompute_ticket_time(day)` that groups non-idle blocks by extracted ticket key, sums seconds, writes `ticket_time`. Verify the sum. Implement. Commit.

---

## Milestone 3 — Recorder (macOS I/O — thin shell)

> Keep this layer thin: it only produces `Sample`s. All shaping is Milestone 1.

### Task 3.1: Active window probe

**Files:** Create `src-tauri/src/recorder/window.rs`

**Step 1:** Implement `current_window() -> Option<(String app, String title)>` using `active-win-pos-rs`. No unit test (OS-dependent) — instead a manual smoke command (Task 6.x exposes it).

**Step 2:** Implement `screen_recording_permission_ok() -> bool` — heuristic: if titles come back empty for known-titled apps, assume permission missing. Document it.

**Commit.**

### Task 3.2: Idle probe

**Files:** Create `src-tauri/src/recorder/idle.rs`

Implement `idle_seconds() -> u64` via `CGEventSourceSecondsSinceLastEventType` (core-graphics). Commit.

### Task 3.3: Sampling loop

**Files:** Create `src-tauri/src/recorder/mod.rs`

Implement a recorder that, when started, spawns a thread sampling every 5s, building `Sample`s and appending them to an in-memory buffer guarded by a `Mutex` inside Tauri state. A `flush()` merges buffer → blocks (Milestone 1) → DB (Milestone 2). Provide `start()` / `stop()` / `is_running()`. Commit.

---

## Milestone 4 — Integrations (HTTP — thin shell)

### Task 4.1: Jira client

**Files:** Create `src-tauri/src/integrations/jira.rs`

`fetch_my_issues(base_url, email, token) -> Vec<JiraTicket>` using Jira REST v3 `/rest/api/3/search` with JQL `assignee = currentUser() AND updated >= -1d`, Basic auth (email:token). Parse key, summary, status, story points (the custom field, often `customfield_10016` — make it configurable). TDD the JSON *parsing* function with a captured sample response fixture; the HTTP call itself stays untested. Commit.

### Task 4.2: GitHub client

**Files:** Create `src-tauri/src/integrations/github.rs`

`fetch_my_prs(token) -> Vec<Pr>` via `https://api.github.com/search/issues?q=author:@me+type:pr+updated:>=...`. TDD the parser with a fixture. Commit.

---

## Milestone 5 — AI (Gemma via LM Studio)

### Task 5.1: Gemma client

**Files:** Create `src-tauri/src/ai/gemma.rs`

`complete(prompt) -> String` → POST `http://localhost:1234/v1/chat/completions` (OpenAI-compatible), model name configurable (default the loaded Gemma id). TDD the request-body builder + response parser with fixtures; HTTP untested. Add a `daily_summary(blocks, tickets)` and `explain_fairness(assessments)` that build prompts. Handle "LM Studio not running" gracefully (return a friendly fallback string). Commit.

---

## Milestone 6 — Tauri commands (wire backend → frontend)

### Task 6.1: Commands

**Files:** Modify `src-tauri/src/lib.rs`

Expose `#[tauri::command]`s:
- `recorder_start`, `recorder_stop`, `recorder_status`
- `sync_now` (Jira + GitHub → DB; reads config)
- `get_dashboard(day)` → returns a single JSON payload: header points (deserved vs assigned totals), net work hours, per-ticket rows (with fairness), timeline blocks, PRs, notes.
- `recompute(day)`, `save_note`, `set_ticket_for_block` (manual correction), `generate_ai_summary(day)`
- `get_config`, `set_config` (Jira url/email/token, GitHub token, Gemma model, idle threshold) — store in a small JSON config file or a `config` table.

Register all in the `invoke_handler`. Smoke-test by calling from devtools. Commit.

---

## Milestone 7 — Frontend dashboard (vanilla TS)

> Single screen, panels from the design. Call commands via `@tauri-apps/api/core`'s `invoke`.

### Task 7.1: Settings screen
Form for Jira url/email/token, GitHub token, Gemma model, idle threshold → `set_config`. Show a banner if Screen Recording permission looks missing. Commit.

### Task 7.2: Header + AI summary panel
Show date, deserved-vs-assigned points, net hours, and the Gemma narrative (with a "menyusun ringkasan…" loading state). Commit.

### Task 7.3: Ticket table
Rows: title, real hours, deserved pts, Jira pts, status chip 🟢🟡🔴. Each block-level row gets a **ticket dropdown** for manual correction → `set_ticket_for_block` → `recompute` → refresh. Commit.

### Task 7.4: Timeline + PR panel + Notes panel
Render activity blocks as a simple time list, PRs as a list, notes as a textarea bound to `save_note`. Commit.

### Task 7.5: Recorder toggle
Prominent on/off control wired to `recorder_start`/`stop`, reflecting `recorder_status` on load. Commit.

---

## Milestone 8 — Glue & first real run

### Task 8.1: Daily auto-sync
On app launch (and once each morning), call `sync_now` + `recompute` + `generate_ai_summary` in the background. Commit.

### Task 8.2: Real-world smoke test
Run `npm run tauri dev`, grant Screen Recording permission, work for ~30 min across VS Code/Chrome/Slack, then open the dashboard and verify: blocks captured, idle excluded, a ticket matched, fairness shown, AI summary generated. Fix what breaks. Commit.

---

## Definition of done (v1)
- App runs in the background and produces accurate activity blocks (idle excluded).
- Jira + GitHub sync works with real credentials.
- Ticket matching works automatically and is correctable by hand.
- Fairness flags (🟢🟡🔴) compute correctly from the 1h=2pt rule.
- Dashboard shows everything on one screen; recorder toggle works.
- **Real acceptance:** used for 1–2 weeks and it actually helps. Only then start v2.

## Credentials needed before Milestone 4
- Jira base URL + account email + API token (id.atlassian.com → API tokens), and the story-point custom field id.
- GitHub personal access token (which gh account? see memory `user_github_accounts`).
- Confirm exact Gemma model id shown in LM Studio.
