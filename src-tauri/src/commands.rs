//! Tauri command layer — the integration keystone.
//!
//! Commands are kept THIN: they open a short-lived db connection (rusqlite
//! `Connection` is not `Sync`, so it must not live in shared state) and delegate
//! to plain, testable functions where there is real logic — chiefly
//! [`build_dashboard`], the aggregator behind [`get_dashboard`].

use serde::{Deserialize, Serialize};

use crate::core::fairness::{assess, deserved_points, Fairness};
use crate::db;
use crate::integrations;
use crate::recorder::Recorder;
use rusqlite::Connection;

/// Shared application state held by Tauri. The `Recorder` is `Send + Sync`;
/// the db is opened per-command from `db_path` (never stored open in state).
pub struct AppState {
    pub recorder: Recorder,
    pub db_path: String,
}

impl AppState {
    fn conn(&self) -> Result<Connection, String> {
        db::open(&self.db_path).map_err(|e| e.to_string())
    }
}

/// Today's date in the LOCAL timezone as `YYYY-MM-DD`.
pub fn local_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// All configuration values, surfaced to/accepted from the frontend as one blob.
/// Tokens are included — this is a local, single-user app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub jira_base_url: String,
    pub jira_email: String,
    pub jira_token: String,
    pub jira_story_point_field: String,
    /// Project / board key to pull tickets from (e.g. "QAT"). Empty = all projects.
    pub jira_project: String,
    /// Assignee filter. Empty = the logged-in user (currentUser()).
    pub jira_assignee: String,
    /// Deprecated: status filtering moved to the in-table filter. Kept (with a
    /// serde default) so older saved configs / payloads still deserialize; it is
    /// always "" now, so the JQL applies no status filter.
    #[serde(default)]
    pub jira_status_category: String,
    /// Sprint scope: "" (all-time) | "active" (current sprint) | "backlog".
    pub jira_sprint_scope: String,
    pub github_token: String,
    /// Google Gemini API key (the only AI provider). The model is hardcoded
    /// (see [`crate::ai::gemma::GEMINI_MODEL`]) and not user-configurable.
    #[serde(default)]
    pub gemini_api_key: String,
    /// Output language for AI generation (test cases, etc.): "Indonesia" |
    /// "English". Empty/legacy configs default to "Indonesia" in `load_config`.
    #[serde(default)]
    pub ai_language: String,
    /// Read-only presence flags for the masked secrets. Set by `get_config` so
    /// the frontend can tell "a token is saved" without ever receiving its
    /// value. Ignored on the way in (saving never persists these).
    #[serde(default)]
    pub has_jira_token: bool,
    #[serde(default)]
    pub has_github_token: bool,
    #[serde(default)]
    pub has_gemini_key: bool,
}

const DEFAULT_STORY_POINT_FIELD: &str = "customfield_10016";

fn load_config(conn: &Connection) -> Result<AppConfig, String> {
    let get = |k: &str| db::get_config(conn, k).map_err(|e| e.to_string());
    let spf = get("jira_story_point_field")?
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_STORY_POINT_FIELD.to_string());
    Ok(AppConfig {
        jira_base_url: get("jira_base_url")?.unwrap_or_default(),
        jira_email: get("jira_email")?.unwrap_or_default(),
        jira_token: get("jira_token")?.unwrap_or_default(),
        jira_story_point_field: spf,
        jira_project: get("jira_project")?.unwrap_or_default(),
        jira_assignee: get("jira_assignee")?.unwrap_or_default(),
        jira_status_category: get("jira_status_category")?.unwrap_or_default(),
        jira_sprint_scope: get("jira_sprint_scope")?.unwrap_or_default(),
        github_token: get("github_token")?.unwrap_or_default(),
        gemini_api_key: get("gemini_api_key")?.unwrap_or_default(),
        ai_language: get("ai_language")?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Indonesia".to_string()),
        // Presence flags are a get_config concern only; the loaded config holds
        // the real secrets, so leave them false here.
        has_jira_token: false,
        has_github_token: false,
        has_gemini_key: false,
    })
}

/// Resolve the Gemini AI target. The model is hardcoded; a missing API key
/// surfaces as the graceful AI-unavailable message at call time.
fn ai_target(cfg: &AppConfig) -> crate::ai::gemma::AiTarget {
    crate::ai::gemma::AiTarget::gemini(cfg.gemini_api_key.trim(), crate::ai::gemma::GEMINI_MODEL)
}

/// Run a DB-using command body on a blocking thread, off the async runtime.
///
/// Command bodies call `reqwest::blocking`, whose internal tokio runtime panics
/// ("Cannot drop a runtime in a context where blocking is not allowed") if used
/// directly inside a Tauri async worker. `spawn_blocking` moves the work to a
/// blocking-pool thread where blocking is allowed, keeping the UI responsive.
/// A fresh `Connection` is opened on that thread and handed to `f`.
async fn with_conn<T, F>(state: &tauri::State<'_, AppState>, f: F) -> Result<T, String>
where
    F: FnOnce(Connection) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db::open(&db_path).map_err(|e| e.to_string())?;
        f(conn)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn save_config(conn: &Connection, cfg: &AppConfig) -> Result<(), String> {
    let set = |k: &str, v: &str| db::set_config(conn, k, v).map_err(|e| e.to_string());
    // A secret is only overwritten when the frontend sends a non-empty value;
    // an empty field means "unchanged" (the frontend never receives the stored
    // value, so it can't echo it back). This keeps the masked round-trip
    // lossless — open Settings, save, and your tokens survive.
    let set_secret = |k: &str, v: &str| {
        if v.trim().is_empty() {
            Ok(())
        } else {
            set(k, v)
        }
    };
    set("jira_base_url", &cfg.jira_base_url)?;
    set("jira_email", &cfg.jira_email)?;
    set_secret("jira_token", &cfg.jira_token)?;
    set("jira_story_point_field", &cfg.jira_story_point_field)?;
    set("jira_project", &cfg.jira_project)?;
    set("jira_assignee", &cfg.jira_assignee)?;
    set("jira_status_category", &cfg.jira_status_category)?;
    set("jira_sprint_scope", &cfg.jira_sprint_scope)?;
    set_secret("github_token", &cfg.github_token)?;
    set_secret("gemini_api_key", &cfg.gemini_api_key)?;
    set("ai_language", &cfg.ai_language)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dashboard aggregation (pure, testable)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DashboardHeader {
    pub deserved_total: f64,
    pub assigned_total: f64,
    pub net_work_secs: i64,
}

#[derive(Debug, Serialize)]
pub struct TicketRow {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub story_points: Option<f64>,
    pub worked_secs: i64,
    pub deserved: f64,
    pub assigned: f64,
    pub fairness: String,
}

#[derive(Debug, Serialize)]
pub struct TimelineRow {
    pub id: i64,
    pub app: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub minutes: i64,
    pub is_idle: bool,
    pub ticket_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PrRow {
    pub number: i64,
    pub repo: String,
    pub title: String,
    pub state: String,
    pub url: String,
    pub updated: String,
}

#[derive(Debug, Serialize)]
pub struct TicketOption {
    pub key: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct Dashboard {
    pub day: String,
    pub header: DashboardHeader,
    pub tickets: Vec<TicketRow>,
    /// Every synced Jira ticket (key + summary), for the timeline assignment
    /// dropdown — independent of whether time has been logged against them.
    pub all_tickets: Vec<TicketOption>,
    pub timeline: Vec<TimelineRow>,
    pub prs: Vec<PrRow>,
    pub notes: String,
    pub ai_summary: String,
}

fn fairness_label(f: &Fairness) -> &'static str {
    match f {
        Fairness::Fair => "Fair",
        Fairness::UnderPointed => "UnderPointed",
        Fairness::OverPointed => "OverPointed",
    }
}

/// Build a TicketRow, assessing fairness only when there's logged time;
/// untouched tickets get worked 0 and the "Untracked" status and do NOT
/// contribute to the day's worked/assigned totals.
#[allow(clippy::too_many_arguments)]
fn make_ticket_row(
    key: String,
    summary: String,
    status: String,
    story_points: Option<f64>,
    worked_secs: i64,
    worked_total: &mut i64,
    assigned_total: &mut f64,
) -> TicketRow {
    let assigned = story_points.unwrap_or(0.0);
    if worked_secs > 0 {
        let a = assess(worked_secs, assigned);
        *worked_total += worked_secs;
        *assigned_total += assigned;
        TicketRow {
            key,
            summary,
            status,
            story_points,
            worked_secs,
            deserved: a.deserved,
            assigned: a.assigned,
            fairness: fairness_label(&a.status).to_string(),
        }
    } else {
        TicketRow {
            key,
            summary,
            status,
            story_points,
            worked_secs: 0,
            deserved: 0.0,
            assigned,
            fairness: "Untracked".to_string(),
        }
    }
}

/// Look up a jira ticket's (summary, status, story_points) by key, if known.
fn lookup_jira(conn: &Connection, key: &str) -> (String, String, Option<f64>) {
    conn.query_row(
        "SELECT summary, status, story_points FROM jira_tickets WHERE key = ?1",
        [key],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<f64>>(2)?,
            ))
        },
    )
    .unwrap_or_else(|_| (String::new(), String::new(), None))
}

/// Build the full dashboard for `day` from the database. Pure aggregation over a
/// connection — no Tauri, no network — so it is unit-testable end to end.
pub fn build_dashboard(conn: &Connection, day: &str) -> Result<Dashboard, String> {
    let map = |e: anyhow::Error| e.to_string();

    // --- ticket rows: ALL assigned/synced tickets, with today's worked time
    //     merged in. Tickets without logged time show worked_secs 0 and the
    //     "Untracked" status; only worked tickets contribute to the header. ---
    let worked: std::collections::HashMap<String, i64> =
        db::get_ticket_time(conn, day).map_err(map)?.into_iter().collect();

    // Pull every synced ticket once (key + summary + status + story points).
    let mut jstmt = conn
        .prepare("SELECT key, summary, status, story_points FROM jira_tickets")
        .map_err(|e| e.to_string())?;
    let jrows = jstmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, Option<f64>>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut tickets: Vec<TicketRow> = Vec::new();
    let mut worked_total: i64 = 0;
    let mut assigned_total: f64 = 0.0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in jrows {
        let (key, summary, status, story_points) = r.map_err(|e| e.to_string())?;
        seen.insert(key.clone());
        let worked_secs = worked.get(&key).copied().unwrap_or(0);
        tickets.push(make_ticket_row(
            key,
            summary,
            status,
            story_points,
            worked_secs,
            &mut worked_total,
            &mut assigned_total,
        ));
    }
    // Include any worked ticket that isn't in the synced set (e.g. a ticket key
    // seen in a window title but not pulled from Jira).
    for (key, worked_secs) in &worked {
        if !seen.contains(key) {
            let (summary, status, story_points) = lookup_jira(conn, key);
            tickets.push(make_ticket_row(
                key.clone(),
                summary,
                status,
                story_points,
                *worked_secs,
                &mut worked_total,
                &mut assigned_total,
            ));
        }
    }
    // Worked tickets first (most time on top), then untouched tickets by key.
    tickets.sort_by(|a, b| {
        b.worked_secs
            .cmp(&a.worked_secs)
            .then_with(|| a.key.cmp(&b.key))
    });

    let _ = worked_total; // header now uses total activity, not just tagged time

    // --- timeline: all blocks for the day (incl. idle), with row ids ---
    let mut stmt = conn
        .prepare(
            "SELECT id, app, title, start, end, is_idle, ticket_key
             FROM activity_blocks
             WHERE substr(start, 1, 10) = ?1
             ORDER BY start",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([day], |row| {
            let start: String = row.get(3)?;
            let end: String = row.get(4)?;
            let minutes = duration_minutes(&start, &end);
            Ok(TimelineRow {
                id: row.get(0)?,
                app: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                start,
                end,
                minutes,
                is_idle: row.get::<_, i64>(5)? != 0,
                ticket_key: row.get::<_, Option<String>>(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut timeline = Vec::new();
    for r in rows {
        timeline.push(r.map_err(|e| e.to_string())?);
    }

    // Header reflects ALL non-idle activity today (so it moves as soon as you
    // record), while "assigned" stays the story points of tickets you've tagged.
    let net_work_secs: i64 = timeline
        .iter()
        .filter(|b| !b.is_idle)
        .map(|b| duration_secs(&b.start, &b.end))
        .sum();
    let header = DashboardHeader {
        deserved_total: deserved_points(net_work_secs),
        assigned_total,
        net_work_secs,
    };

    // --- all pull requests ---
    let mut pstmt = conn
        .prepare("SELECT number, repo, title, state, url, updated FROM pull_requests ORDER BY updated DESC")
        .map_err(|e| e.to_string())?;
    let prows = pstmt
        .query_map([], |row| {
            Ok(PrRow {
                number: row.get(0)?,
                repo: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                state: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                url: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                updated: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?;
    let mut prs = Vec::new();
    for r in prows {
        prs.push(r.map_err(|e| e.to_string())?);
    }

    // --- all synced tickets (for the timeline assignment dropdown) ---
    let mut tstmt = conn
        .prepare("SELECT key, summary FROM jira_tickets ORDER BY key")
        .map_err(|e| e.to_string())?;
    let trows = tstmt
        .query_map([], |row| {
            Ok(TicketOption {
                key: row.get::<_, String>(0)?,
                summary: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?;
    let mut all_tickets = Vec::new();
    for r in trows {
        all_tickets.push(r.map_err(|e| e.to_string())?);
    }

    let notes = db::get_note(conn, day).map_err(map)?.unwrap_or_default();
    let ai_summary = db::get_ai_summary(conn, day, "daily")
        .map_err(map)?
        .unwrap_or_default();

    Ok(Dashboard {
        day: day.to_string(),
        header,
        tickets,
        all_tickets,
        timeline,
        prs,
        notes,
        ai_summary,
    })
}

/// Whole-minute duration between two RFC3339 timestamps; 0 on parse failure.
fn duration_secs(start: &str, end: &str) -> i64 {
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc))
    };
    match (parse(start), parse(end)) {
        (Some(s), Some(e)) => (e - s).num_seconds().max(0),
        _ => 0,
    }
}

fn duration_minutes(start: &str, end: &str) -> i64 {
    duration_secs(start, end) / 60
}

// ---------------------------------------------------------------------------
// Tauri commands (thin wrappers)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SyncResult {
    pub tickets: usize,
    pub prs: usize,
}

#[tauri::command]
pub fn recorder_start(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.recorder.start();
    Ok(())
}

#[tauri::command]
pub fn recorder_stop(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // Persist buffered samples before stopping the sampling thread.
    state.recorder.flush().map_err(|e| e.to_string())?;
    state.recorder.stop();
    Ok(())
}

#[tauri::command]
pub fn recorder_status(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.recorder.is_running())
}

#[tauri::command]
pub fn screen_recording_ok() -> Result<bool, String> {
    Ok(crate::recorder::window::screen_recording_permission_ok())
}

#[tauri::command]
pub fn get_config(state: tauri::State<'_, AppState>) -> Result<AppConfig, String> {
    let conn = state.conn()?;
    let mut cfg = load_config(&conn)?;
    // Never ship the actual secrets to the frontend. Surface only whether each
    // one is set, then blank the values. Saving with an empty secret preserves
    // the stored one (see `save_config`), so a blanked round-trip is lossless.
    cfg.has_jira_token = !cfg.jira_token.trim().is_empty();
    cfg.has_github_token = !cfg.github_token.trim().is_empty();
    cfg.has_gemini_key = !cfg.gemini_api_key.trim().is_empty();
    cfg.jira_token = String::new();
    cfg.github_token = String::new();
    cfg.gemini_api_key = String::new();
    Ok(cfg)
}

#[tauri::command]
pub fn set_config(state: tauri::State<'_, AppState>, cfg: AppConfig) -> Result<(), String> {
    let conn = state.conn()?;
    save_config(&conn, &cfg)
}

/// Test Jira credentials; returns a success line for the UI. The frontend saves
/// the form first, so this reads the just-saved config.
#[tauri::command]
pub async fn test_jira_connection(state: tauri::State<'_, AppState>) -> Result<String, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        require_jira_creds(&cfg)?;
        let name = integrations::jira::fetch_myself(&cfg.jira_base_url, &cfg.jira_email, &cfg.jira_token)
            .map_err(|e| format!("Gagal konek Jira: {e}"))?;
        Ok(format!("Terhubung sebagai {name}"))
    })
    .await
}

/// Test the GitHub token; returns the authenticated login.
#[tauri::command]
pub async fn test_github_connection(state: tauri::State<'_, AppState>) -> Result<String, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        if cfg.github_token.trim().is_empty() {
            return Err("Isi GitHub Token dulu".into());
        }
        let login = integrations::github::fetch_user(&cfg.github_token)
            .map_err(|e| format!("Gagal konek GitHub: {e}"))?;
        Ok(format!("Terhubung sebagai @{login}"))
    })
    .await
}

/// Test the Gemini API key with a minimal request.
#[tauri::command]
pub async fn test_gemini_connection(state: tauri::State<'_, AppState>) -> Result<String, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        crate::ai::gemma::test_connection(&ai_target(&cfg))?;
        Ok(format!("Gemini OK ({})", crate::ai::gemma::GEMINI_MODEL))
    })
    .await
}

#[tauri::command]
pub async fn sync_now(state: tauri::State<'_, AppState>) -> Result<SyncResult, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;

    if cfg.jira_base_url.is_empty() || cfg.jira_email.is_empty() || cfg.jira_token.is_empty() {
        return Err("Jira credentials missing (set base URL, email, and token in Settings)".into());
    }

    let tickets = integrations::jira::fetch_my_issues(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &cfg.jira_story_point_field,
        &cfg.jira_project,
        &cfg.jira_assignee,
        // Status filtering is now done in the ticket table, not at sync time, so
        // we always pull all statuses (within the chosen sprint scope).
        "",
        &cfg.jira_sprint_scope,
    )
    .map_err(|e| format!("Jira sync failed: {e}"))?;
    integrations::save_tickets(&conn, &tickets).map_err(|e| e.to_string())?;

    // GitHub is optional (a QA may not use it). Only sync if a token is set.
    let prs = if cfg.github_token.is_empty() {
        Vec::new()
    } else {
        let prs = integrations::github::fetch_my_prs(&cfg.github_token)
            .map_err(|e| format!("GitHub sync failed: {e}"))?;
        integrations::save_prs(&conn, &prs).map_err(|e| e.to_string())?;
        prs
    };

    Ok(SyncResult {
        tickets: tickets.len(),
        prs: prs.len(),
    })
    })
    .await
}

#[tauri::command]
pub fn recompute(state: tauri::State<'_, AppState>, day: String) -> Result<(), String> {
    // Flush any buffered samples first so the rollup sees the latest blocks.
    state.recorder.flush().map_err(|e| e.to_string())?;
    let conn = state.conn()?;
    db::recompute_ticket_time(&conn, &day).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_note(
    state: tauri::State<'_, AppState>,
    day: String,
    body: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::set_note(&conn, &day, &body).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_ticket_for_block(
    state: tauri::State<'_, AppState>,
    block_id: i64,
    ticket_key: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::set_block_ticket(&conn, block_id, &ticket_key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_ai_summary(
    state: tauri::State<'_, AppState>,
    day: String,
) -> Result<String, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;

    // QA actions logged today (status moves + point sets).
    let activities = db::list_qa_activity_for_day(&conn, &day).map_err(|e| e.to_string())?;

    // Current board snapshot (all synced tickets) for context.
    let tickets = load_all_tickets(&conn).map_err(|e| e.to_string())?;

    let summary = crate::ai::gemma::qa_summary(&ai_target(&cfg), &activities, &tickets);
    db::set_ai_summary(&conn, &day, "daily", &summary).map_err(|e| e.to_string())?;
    Ok(summary)
    })
    .await
}

/// Load every synced Jira ticket as a `JiraTicket` (for the daily summary's
/// board snapshot).
fn load_all_tickets(conn: &Connection) -> Result<Vec<crate::integrations::jira::JiraTicket>, String> {
    let mut stmt = conn
        .prepare("SELECT key, summary, status, story_points FROM jira_tickets ORDER BY key")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(crate::integrations::jira::JiraTicket {
                key: row.get::<_, String>(0)?,
                summary: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                status: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                story_points: row.get::<_, Option<f64>>(3)?,
                updated: String::new(),
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Read the cached daily summary for `day` (empty string when none yet).
/// Lets the UI show a previously-generated summary without re-calling the AI.
#[tauri::command]
pub fn get_daily_summary(
    state: tauri::State<'_, AppState>,
    day: String,
) -> Result<String, String> {
    let conn = state.conn()?;
    Ok(db::get_ai_summary(&conn, &day, "daily")
        .map_err(|e| e.to_string())?
        .unwrap_or_default())
}

#[tauri::command]
pub fn get_dashboard(
    state: tauri::State<'_, AppState>,
    day: String,
) -> Result<Dashboard, String> {
    let conn = state.conn()?;
    build_dashboard(&conn, &day)
}

#[tauri::command]
pub fn today() -> Result<String, String> {
    Ok(local_today())
}

/// Reject when the three required Jira credentials aren't all present.
fn require_jira_creds(cfg: &AppConfig) -> Result<(), String> {
    if cfg.jira_base_url.is_empty() || cfg.jira_email.is_empty() || cfg.jira_token.is_empty() {
        return Err("Isi Base URL, Email, dan API token Jira dulu".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn list_jira_fields(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<integrations::jira::JiraField>, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        require_jira_creds(&cfg)?;
        integrations::jira::fetch_fields(&cfg.jira_base_url, &cfg.jira_email, &cfg.jira_token)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn list_jira_projects(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<integrations::jira::JiraProject>, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        require_jira_creds(&cfg)?;
        integrations::jira::fetch_projects(&cfg.jira_base_url, &cfg.jira_email, &cfg.jira_token)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn list_jira_assignees(
    state: tauri::State<'_, AppState>,
    project: String,
) -> Result<Vec<integrations::jira::JiraUser>, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    // Fall back to the saved project when the caller passes an empty one.
    let project = if project.trim().is_empty() {
        cfg.jira_project.clone()
    } else {
        project
    };
    integrations::jira::fetch_assignees(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &project,
    )
    .map_err(|e| e.to_string())
    })
    .await
}

/// List the workflow transitions available for a Jira issue (e.g. To Do →
/// In Progress → Done). Read-only.
#[tauri::command]
pub async fn list_transitions(
    state: tauri::State<'_, AppState>,
    key: String,
) -> Result<Vec<integrations::jira::JiraTransition>, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::fetch_transitions(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
    )
    .map_err(|e| e.to_string())
    })
    .await
}

/// Move a Jira issue to a new status via `transition_id`. This is a WRITE to
/// Jira — the frontend gates it behind a confirmation dialog. After success the
/// frontend re-syncs, so this command does not re-sync itself.
#[tauri::command]
pub async fn transition_issue(
    state: tauri::State<'_, AppState>,
    key: String,
    transition_id: String,
    to_status: String,
) -> Result<(), String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    // Capture the current (pre-move) status + summary for the activity log.
    let (summary, from_status, _) = lookup_jira(&conn, &key);
    integrations::jira::do_transition(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
        &transition_id,
    )
    .map_err(|e| e.to_string())?;
    // Reflect the new status locally so the board updates on Refresh without a
    // full re-Sync. (Jira is the source of truth; this is an optimistic mirror.)
    if !to_status.trim().is_empty() {
        conn.execute(
            "UPDATE jira_tickets SET status = ?1 WHERE key = ?2",
            rusqlite::params![to_status, key],
        )
        .map_err(|e| e.to_string())?;
    }
    // Log the move for the daily QA summary (best-effort; never fails the move).
    let _ = db::log_qa_activity(
        &conn,
        &local_today(),
        &chrono::Local::now().to_rfc3339(),
        &key,
        &summary,
        "transition",
        &from_status,
        to_status.trim(),
        None,
    );
    Ok(())
    })
    .await
}

/// A ticket card for the Kanban board.
#[derive(Debug, Serialize)]
pub struct BoardTicket {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub story_points: Option<f64>,
}

#[tauri::command]
pub fn list_board_tickets(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<BoardTicket>, String> {
    let conn = state.conn()?;
    let mut stmt = conn
        .prepare("SELECT key, summary, status, story_points FROM jira_tickets ORDER BY key")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(BoardTicket {
                key: row.get::<_, String>(0)?,
                summary: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                status: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                story_points: row.get::<_, Option<f64>>(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

#[tauri::command]
pub async fn set_story_points(
    state: tauri::State<'_, AppState>,
    key: String,
    points: Option<f64>,
) -> Result<(), String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::update_story_points(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
        &cfg.jira_story_point_field,
        points,
    )
    .map_err(|e| e.to_string())?;
    // Reflect locally so the board updates without a full re-sync.
    conn.execute(
        "UPDATE jira_tickets SET story_points = ?1 WHERE key = ?2",
        rusqlite::params![points, key],
    )
    .map_err(|e| e.to_string())?;
    // Log the point set for the daily QA summary (best-effort; skip clears).
    if points.is_some() {
        let (summary, _, _) = lookup_jira(&conn, &key);
        let _ = db::log_qa_activity(
            &conn,
            &local_today(),
            &chrono::Local::now().to_rfc3339(),
            &key,
            &summary,
            "points",
            "",
            "",
            points,
        );
    }
    Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Test cases (per-ticket QA test cases + AI generation)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_test_cases(
    state: tauri::State<'_, AppState>,
    key: String,
) -> Result<Vec<db::TestCase>, String> {
    let conn = state.conn()?;
    db::list_test_cases(&conn, &key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_test_case(
    state: tauri::State<'_, AppState>,
    key: String,
    title: String,
    steps: String,
    expected: String,
) -> Result<i64, String> {
    let conn = state.conn()?;
    db::add_test_case(&conn, &key, &title, &steps, &expected).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_test_case_status(
    state: tauri::State<'_, AppState>,
    id: i64,
    status: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::set_test_case_status(&conn, id, &status).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_test_case_notes(
    state: tauri::State<'_, AppState>,
    id: i64,
    notes: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::set_test_case_notes(&conn, id, &notes).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_test_case(
    state: tauri::State<'_, AppState>,
    id: i64,
    title: String,
    steps: String,
    expected: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::update_test_case(&conn, id, &title, &steps, &expected).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_test_case(
    state: tauri::State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    let conn = state.conn()?;
    db::delete_test_case(&conn, id).map_err(|e| e.to_string())
}

/// Ask the local model to draft test cases for a ticket, persist each, and
/// return the freshly-listed cases for the ticket.
#[tauri::command]
pub async fn generate_test_cases(
    state: tauri::State<'_, AppState>,
    key: String,
    summary: String,
) -> Result<Vec<db::TestCase>, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;

    let drafted =
        crate::ai::gemma::generate_test_cases(&ai_target(&cfg), &key, &summary, &cfg.ai_language);
    if drafted.is_empty() {
        return Err("AI nggak menghasilkan test case — coba lagi atau cek API key Gemini".into());
    }
    for (title, steps, expected) in &drafted {
        db::add_test_case(&conn, &key, title, steps, expected).map_err(|e| e.to_string())?;
    }
    db::list_test_cases(&conn, &key).map_err(|e| e.to_string())
    })
    .await
}

/// Fetch a PR's diff, ask the local model to draft test cases FROM the code
/// change, persist each, and return the freshly-listed cases for the ticket.
#[tauri::command]
pub async fn generate_test_cases_from_pr(
    state: tauri::State<'_, AppState>,
    key: String,
    summary: String,
    repo: String,
    number: i64,
) -> Result<Vec<db::TestCase>, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    if cfg.github_token.is_empty() {
        return Err("Isi GitHub Token di Settings dulu".into());
    }
    let diff = integrations::github::fetch_pr_diff(&cfg.github_token, &repo, number)
        .map_err(|e| e.to_string())?;
    let cases = crate::ai::gemma::parse_test_cases(&crate::ai::gemma::complete(
        &ai_target(&cfg),
        &crate::ai::gemma::test_cases_from_diff_prompt(&key, &summary, &diff, &cfg.ai_language),
    ));
    if cases.is_empty() {
        return Err("AI nggak menghasilkan test case dari PR ini — coba lagi atau cek API key Gemini".into());
    }
    for (title, steps, expected) in &cases {
        db::add_test_case(&conn, &key, title, steps, expected).map_err(|e| e.to_string())?;
    }
    db::list_test_cases(&conn, &key).map_err(|e| e.to_string())
    })
    .await
}

/// One PR to combine when generating test cases across repos.
#[derive(Debug, Deserialize)]
pub struct PrInput {
    pub repo: String,
    pub number: i64,
}

/// Generate test cases from the COMBINED diffs of several PRs (a ticket can span
/// e.g. a native repo + a Flutter repo). Diffs are concatenated with a header
/// per PR; the prompt builder truncates the combined text to its budget.
#[tauri::command]
pub async fn generate_test_cases_from_prs(
    state: tauri::State<'_, AppState>,
    key: String,
    summary: String,
    prs: Vec<PrInput>,
) -> Result<Vec<db::TestCase>, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    if cfg.github_token.is_empty() {
        return Err("Isi GitHub Token di Settings dulu".into());
    }
    if prs.is_empty() {
        return Err("Belum ada PR yang ditempel".into());
    }
    let mut combined = String::new();
    for pr in &prs {
        let diff = integrations::github::fetch_pr_diff(&cfg.github_token, &pr.repo, pr.number)
            .map_err(|e| format!("Gagal ambil diff {}#{}: {e}", pr.repo, pr.number))?;
        combined.push_str(&format!("### PR #{} — {}\n{}\n\n", pr.number, pr.repo, diff));
    }
    let cases = crate::ai::gemma::parse_test_cases(&crate::ai::gemma::complete(
        &ai_target(&cfg),
        &crate::ai::gemma::test_cases_from_diff_prompt(&key, &summary, &combined, &cfg.ai_language),
    ));
    if cases.is_empty() {
        return Err("AI nggak menghasilkan test case dari PR-PR ini — coba lagi atau cek API key Gemini".into());
    }
    for (title, steps, expected) in &cases {
        db::add_test_case(&conn, &key, title, steps, expected).map_err(|e| e.to_string())?;
    }
    db::list_test_cases(&conn, &key).map_err(|e| e.to_string())
    })
    .await
}

/// A generated bug report: an editable title + body, plus the raw model output.
#[derive(Debug, Serialize)]
pub struct BugReport {
    pub title: String,
    pub body: String,
    pub raw: String,
}

/// Generate a structured bug report from free-form text and an optional
/// screenshot (base64, bare or `data:` URL). The result is returned for the user
/// to review/edit before it is pushed to Jira via [`create_jira_bug`].
#[tauri::command]
pub async fn generate_bug_report(
    state: tauri::State<'_, AppState>,
    text: String,
    images: Vec<String>,
    language: String,
    sections: Vec<String>,
) -> Result<BugReport, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;

    // Drop blanks so an empty array / stray "" doesn't count as an attachment.
    let images: Vec<String> = images.into_iter().filter(|s| !s.trim().is_empty()).collect();
    if text.trim().is_empty() && images.is_empty() {
        return Err("Isi deskripsi bug atau lampirkan screenshot dulu".into());
    }
    let lang = if language.trim().is_empty() { "Indonesia" } else { language.trim() };
    let sections: Vec<String> = if sections.is_empty() {
        crate::ai::gemma::DEFAULT_BUG_SECTIONS.iter().map(|s| s.to_string()).collect()
    } else {
        sections
    };

    let (title, body, raw) = crate::ai::gemma::generate_bug_report(
        &ai_target(&cfg),
        &text,
        &images,
        lang,
        &sections,
    );
    if body.trim().is_empty() {
        return Err("AI nggak menghasilkan bug report — coba lagi atau cek API key Gemini".into());
    }
    Ok(BugReport { title, body, raw })
    })
    .await
}

/// Create a Bug issue in Jira from a (reviewed) report and optionally attach the
/// screenshot. Returns the new issue key + browse URL.
#[tauri::command]
pub async fn create_jira_bug(
    state: tauri::State<'_, AppState>,
    project_key: String,
    summary: String,
    body: String,
    priority: Option<String>,
    assignee_id: Option<String>,
    images: Vec<String>,
    videos: Vec<String>,
) -> Result<integrations::jira::CreatedIssue, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;

    if project_key.trim().is_empty() {
        return Err("Pilih project Jira dulu".into());
    }
    if summary.trim().is_empty() {
        return Err("Title bug nggak boleh kosong".into());
    }

    let issue_type_id = integrations::jira::find_issue_type(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        project_key.trim(),
        "Bug",
    )
    .map_err(|e| e.to_string())?;

    // The report body goes into the Acceptance Criteria field (customfield_10125),
    // which is what the team's bug view surfaces; Description is left empty.
    let ac = integrations::jira::text_to_adf(&body);
    let priority = priority.as_deref().filter(|p| !p.trim().is_empty());
    let assignee = assignee_id.as_deref().filter(|a| !a.trim().is_empty());

    let created = integrations::jira::create_issue(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        project_key.trim(),
        &issue_type_id,
        summary.trim(),
        &ac,
        priority,
        assignee,
    )
    .map_err(|e| e.to_string())?;

    // Attach every screenshot + recording. A failed upload must not lose the
    // created issue, so collect failures and report them softly.
    let clean = |v: Vec<String>| -> Vec<String> {
        v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    };
    let images = clean(images);
    let videos = clean(videos);
    let total = images.len() + videos.len();
    let mut failed = 0u32;
    let attach = |name: &str, data: &str| {
        integrations::jira::upload_attachment(
            &cfg.jira_base_url,
            &cfg.jira_email,
            &cfg.jira_token,
            &created.key,
            name,
            data,
        )
    };
    for (i, img) in images.iter().enumerate() {
        if attach(&format!("screenshot-{}.png", i + 1), img).is_err() {
            failed += 1;
        }
    }
    for (i, vid) in videos.iter().enumerate() {
        if attach(&format!("recording-{}.mov", i + 1), vid).is_err() {
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(format!(
            "Bug {} dibuat, tapi {failed} dari {total} lampiran gagal diunggah",
            created.key
        ));
    }

    Ok(created)
    })
    .await
}

/// Capture a user-selected screen region and return it as a PNG data URL, or
/// `None` if the user cancelled the selection. macOS only (uses the built-in
/// `screencapture -i`); other platforms return an error so the UI can hide the
/// button gracefully.
#[tauri::command]
pub fn capture_screen_region() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        use base64::Engine;
        let mut path = std::env::temp_dir();
        path.push("qacockpit-region-capture.png");
        // Clear any stale file so a cancelled capture can't resurface an old one.
        let _ = std::fs::remove_file(&path);

        // `-i` = interactive region/window selection. Esc-cancel exits 0 but
        // writes no file.
        let status = std::process::Command::new("screencapture")
            .args(["-i", "-t", "png"])
            .arg(&path)
            .status()
            .map_err(|e| format!("gagal menjalankan screencapture: {e}"))?;
        if !status.success() {
            return Err("Capture gagal".into());
        }
        if !path.exists() {
            return Ok(None); // user pressed Esc
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("gagal baca hasil capture: {e}"))?;
        let _ = std::fs::remove_file(&path);
        if bytes.is_empty() {
            return Ok(None);
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(format!("data:image/png;base64,{b64}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Region capture cuma didukung di macOS".into())
    }
}

/// Record a screen clip and return it as a QuickTime (.mov) data URL, or `None`
/// if the user cancelled. macOS only: uses `screencapture -v` (interactive
/// selection — the user picks the area/screen and stops via the menu-bar
/// control, exactly like ⌘⇧5). Requires Screen Recording permission.
#[tauri::command]
pub fn capture_screen_video() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        use base64::Engine;
        if !crate::recorder::window::screen_recording_permission_ok() {
            return Err(
                "Izin Screen Recording belum aktif. Buka System Settings → Privacy & Security → \
                 Screen Recording, aktifkan QA Cockpit, lalu coba lagi."
                    .into(),
            );
        }
        let mut path = std::env::temp_dir();
        path.push("qacockpit-recording.mov");
        let _ = std::fs::remove_file(&path);

        let status = std::process::Command::new("screencapture")
            .arg("-v") // interactive video capture; blocks until the user stops
            .arg(&path)
            .status()
            .map_err(|e| format!("gagal menjalankan screencapture: {e}"))?;
        if !status.success() {
            return Err("Rekam gagal".into());
        }
        if !path.exists() {
            return Ok(None); // user cancelled before recording
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("gagal baca hasil rekaman: {e}"))?;
        let _ = std::fs::remove_file(&path);
        if bytes.is_empty() {
            return Ok(None);
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(format!("data:video/quicktime;base64,{b64}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Rekam layar cuma didukung di macOS".into())
    }
}

// ---------------------------------------------------------------------------
// Ticket Builder (bulk Story creation under an epic)
// ---------------------------------------------------------------------------

/// One parsed row from the pasted blob (editable in the UI before creating).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderRow {
    #[serde(default)]
    pub source_ticket: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pr_number: String,
    #[serde(default)]
    pub pr_url: String,
    #[serde(default)]
    pub assignee: String,
}

/// The AI-parsed blob: epic + app label + rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedBlob {
    #[serde(default)]
    pub epic: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub rows: Vec<BuilderRow>,
}

/// Parse a free-form ticket blob into structured rows via Gemini. The UI shows
/// these in an editable table for review before creating.
#[tauri::command]
pub async fn parse_ticket_blob(
    state: tauri::State<'_, AppState>,
    blob: String,
) -> Result<ParsedBlob, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        if blob.trim().is_empty() {
            return Err("Tempel daftar PR-nya dulu".into());
        }
        let raw = crate::ai::gemma::complete(
            &ai_target(&cfg),
            &crate::ai::gemma::parse_ticket_rows_prompt(&blob),
        );
        let json = crate::ai::gemma::extract_json(&raw);
        let mut parsed = serde_json::from_str::<ParsedBlob>(json)
            .map_err(|e| format!("AI gagal mem-parse daftar (cek API key Gemini): {e}"))?;
        enrich_rows_from_pr(&cfg, &parsed.epic, &mut parsed.rows);
        Ok(parsed)
    })
    .await
}

/// Fill in missing `source_ticket` / `title` from each row's PR. For rows that
/// lack a source or title, fetches the PR's title+body and:
/// - sets `source_ticket` from the first Jira key in the PR (excluding the
///   epic's own project, so the epic key isn't mistaken for the source);
/// - sets `title` from the PR title.
/// A no-op without a GitHub token; per-row fetch failures are skipped silently
/// (the row stays as the AI parsed it and remains editable in the table).
fn enrich_rows_from_pr(cfg: &AppConfig, epic: &str, rows: &mut [BuilderRow]) {
    if cfg.github_token.trim().is_empty() {
        return;
    }
    let epic_project = epic.split('-').next().unwrap_or("");
    for row in rows.iter_mut() {
        let need_source = row.source_ticket.trim().is_empty();
        let need_title = row.title.trim().is_empty();
        if !need_source && !need_title {
            continue;
        }
        let Some((repo, number)) = integrations::github::parse_pr_url(&row.pr_url) else {
            continue;
        };
        let Ok((pr_title, pr_body)) =
            integrations::github::fetch_pr_detail(&cfg.github_token, &repo, number)
        else {
            continue;
        };
        if need_source {
            let haystack = format!("{pr_title}\n{pr_body}");
            if let Some(key) =
                crate::core::matching::extract_ticket_key_excluding(&haystack, epic_project)
            {
                row.source_ticket = key;
            }
        }
        if need_title && !pr_title.trim().is_empty() {
            row.title = pr_title.trim().to_string();
        }
    }
}

/// The outcome of creating one Story (per-row; failures don't stop the rest).
#[derive(Debug, Serialize)]
pub struct StoryResult {
    pub title: String,
    pub key: Option<String>,
    pub url: Option<String>,
    pub error: Option<String>,
}

/// Create one Story from a row. Returns the created issue or a per-row error.
fn build_and_create_story(
    cfg: &AppConfig,
    project: &str,
    epic: &str,
    app: &str,
    story_type_id: &str,
    sprint_id: Option<i64>,
    reporter_id: Option<&str>,
    row: &BuilderRow,
) -> Result<integrations::jira::CreatedIssue, String> {
    let base = &cfg.jira_base_url;
    let email = &cfg.jira_email;
    let token = &cfg.jira_token;
    let m = |e: anyhow::Error| e.to_string();

    if row.pr_number.trim().is_empty() && row.title.trim().is_empty() {
        return Err("baris kosong".into());
    }

    let assignee_id = integrations::jira::resolve_user(base, email, token, project, &row.assignee)
        .map_err(m)
        .unwrap_or(None);

    let src_key = row.source_ticket.trim();
    let source = if src_key.is_empty() {
        None
    } else {
        integrations::jira::fetch_source_ticket(base, email, token, src_key).ok()
    };
    let source_summary = source.as_ref().map(|s| s.summary.clone());

    // Acceptance Criteria: copy the source AC verbatim, else generate from the PR.
    let ac_adf = if let Some(s) = source.as_ref().filter(|s| s.ac_adf.is_some()) {
        integrations::jira::build_ac_adf(
            Some(src_key),
            source_summary.as_deref(),
            s.ac_adf.as_ref(),
            base,
            &row.pr_url,
            &row.pr_number,
            &[],
        )
    } else {
        let mut generated = generate_ac_from_pr(cfg, &row.pr_url, &row.pr_number);
        // No source and nothing usable from GitHub: fall back to the row title as
        // the acceptance criterion so the Story still has valid AC and gets created.
        if generated.iter().all(|l| l.trim().is_empty()) {
            let t = row.title.trim();
            if !t.is_empty() {
                generated = vec![t.to_string()];
            }
        }
        let key = if src_key.is_empty() { None } else { Some(src_key) };
        integrations::jira::build_ac_adf(
            key,
            source_summary.as_deref(),
            None,
            base,
            &row.pr_url,
            &row.pr_number,
            &generated,
        )
    };

    let prefix = if src_key.is_empty() {
        String::new()
    } else {
        format!("[{src_key}] ")
    };
    let pr_suffix = if row.pr_number.trim().is_empty() {
        String::new()
    } else {
        format!(" #{}", row.pr_number.trim())
    };
    let summary = format!(
        "[UAT] [{}] {}{}{}",
        app.trim(),
        prefix,
        row.title.trim(),
        pr_suffix
    );

    // Squad: copy from source; default to Quality Assurance Team (QAT) when no source.
    let squad_value = source
        .as_ref()
        .and_then(|s| s.squad.clone())
        .unwrap_or_else(|| serde_json::json!({ "key": "QAT" }));

    let body = integrations::jira::build_story_body(&integrations::jira::StoryFields {
        project_key: project,
        issue_type_id: story_type_id,
        summary: &summary,
        epic_key: epic.trim(),
        sprint_id,
        reporter_id,
        assignee_id: assignee_id.as_deref(),
        squad: Some(&squad_value),
        developer_id: source.as_ref().and_then(|s| s.developer.as_deref()),
        ac_adf: &ac_adf,
    });
    integrations::jira::create_issue_raw(base, email, token, &body).map_err(m)
}

/// Generate AC lines from a PR's title/body via Gemini (empty when no GitHub
/// token or the PR can't be fetched).
fn generate_ac_from_pr(cfg: &AppConfig, pr_url: &str, pr_number: &str) -> Vec<String> {
    if cfg.github_token.trim().is_empty() {
        return Vec::new();
    }
    let Some((repo, number)) = integrations::github::parse_pr_url(pr_url) else {
        return Vec::new();
    };
    let Ok((title, body)) = integrations::github::fetch_pr_detail(&cfg.github_token, &repo, number)
    else {
        return Vec::new();
    };
    let raw = crate::ai::gemma::complete(
        &ai_target(cfg),
        &crate::ai::gemma::generate_ac_prompt(&title, &body, pr_number),
    );
    crate::ai::gemma::parse_ac_lines(&raw)
}

/// Create QAT Story tickets under `epic` from the (reviewed) rows. Resolves the
/// Story type, active sprint, and reporter once, then creates each row;
/// per-row failures are reported but don't stop the batch.
#[tauri::command]
pub async fn create_story_tickets(
    state: tauri::State<'_, AppState>,
    epic: String,
    app: String,
    rows: Vec<BuilderRow>,
) -> Result<Vec<StoryResult>, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        require_jira_creds(&cfg)?;
        if epic.trim().is_empty() {
            return Err("Isi Epic key dulu".into());
        }
        if rows.is_empty() {
            return Err("Belum ada baris buat dibuat".into());
        }
        let project = epic.split('-').next().unwrap_or("QAT").to_string();
        let story_type_id = integrations::jira::find_issue_type(
            &cfg.jira_base_url,
            &cfg.jira_email,
            &cfg.jira_token,
            &project,
            "Story",
        )
        .map_err(|e| e.to_string())?;
        // Sprint + reporter are best-effort (omitted if not resolvable).
        let sprint_id = integrations::jira::fetch_active_sprint_id(
            &cfg.jira_base_url,
            &cfg.jira_email,
            &cfg.jira_token,
            &project,
        )
        .ok();
        let reporter_id = integrations::jira::resolve_user(
            &cfg.jira_base_url,
            &cfg.jira_email,
            &cfg.jira_token,
            &project,
            "Theo",
        )
        .ok()
        .flatten();

        let mut results = Vec::with_capacity(rows.len());
        for row in &rows {
            match build_and_create_story(
                &cfg,
                &project,
                &epic,
                &app,
                &story_type_id,
                sprint_id,
                reporter_id.as_deref(),
                row,
            ) {
                Ok(ci) => results.push(StoryResult {
                    title: row.title.clone(),
                    key: Some(ci.key),
                    url: Some(ci.url),
                    error: None,
                }),
                Err(e) => results.push(StoryResult {
                    title: row.title.clone(),
                    key: None,
                    url: None,
                    error: Some(e),
                }),
            }
        }
        Ok(results)
    })
    .await
}

/// Send the ticket's test results to Jira as a comment with an ADF table.
/// Returns the summary line so the UI can toast it.
#[tauri::command]
pub async fn post_test_results(
    state: tauri::State<'_, AppState>,
    key: String,
) -> Result<String, String> {
    with_conn(&state, move |conn| {
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;

    let cases = db::list_test_cases(&conn, &key).map_err(|e| e.to_string())?;
    if cases.is_empty() {
        return Err("Belum ada test case buat dikirim".into());
    }

    // Counts drive both the panel color/text and the (returned) toast line.
    let total = cases.len();
    let (mut passed, mut failed, mut untested) = (0usize, 0usize, 0usize);
    let mut rows: Vec<integrations::jira::ResultRow> = Vec::with_capacity(total);
    for c in &cases {
        match c.status.as_str() {
            "passed" => passed += 1,
            "failed" => failed += 1,
            _ => untested += 1,
        }
        rows.push(integrations::jira::ResultRow {
            title: c.title.clone(),
            steps: c.steps.clone(),
            expected: c.expected.clone(),
            status: c.status.clone(),
            notes: c.notes.clone(),
        });
    }

    // Panel: error if anything failed; success if all run + at least one pass;
    // info otherwise (e.g. some still untested).
    let panel_type = if failed > 0 {
        "error"
    } else if untested == 0 && passed > 0 {
        "success"
    } else {
        "info"
    };
    let panel_text = match panel_type {
        "success" => format!("Semua PASS — {passed}/{total} test case"),
        "error" => format!("{failed} test case GAGAL dari {total}"),
        _ => format!("{passed} pass · {failed} fail · {untested} belum dites"),
    };

    let date = chrono::Local::now().format("%d %b %Y").to_string();
    let tester = if cfg.jira_email.is_empty() {
        "QA".to_string()
    } else {
        cfg.jira_email.clone()
    };
    let heading = format!("🧪 Hasil Test QA — {key} · {date} · {tester}");

    let adf = integrations::jira::build_results_adf(&heading, panel_type, &panel_text, &rows);
    integrations::jira::add_comment(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
        &adf,
    )
    .map_err(|e| e.to_string())?;
    Ok(panel_text)
    })
    .await
}

// ---------------------------------------------------------------------------
// PR tab (on-demand: find a ticket's PR(s) + AI review of the diff)
// ---------------------------------------------------------------------------

const GITHUB_TOKEN_MISSING: &str = "Isi GitHub Token di Settings dulu buat fitur PR";

/// Search GitHub for PRs that mention a ticket key.
#[tauri::command]
pub async fn list_ticket_prs(
    state: tauri::State<'_, AppState>,
    key: String,
) -> Result<Vec<integrations::github::PrRef>, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        if cfg.github_token.is_empty() {
            return Err(GITHUB_TOKEN_MISSING.into());
        }
        integrations::github::search_prs_for_key(&cfg.github_token, &key).map_err(|e| e.to_string())
    })
    .await
}

/// Fetch a PR's diff and ask the model to summarize it + "what to test",
/// streaming the answer to the frontend chunk-by-chunk via `on_chunk`. Returns
/// the full text once complete (also persisted as the PR summary by the caller).
#[tauri::command]
pub async fn summarize_pr(
    state: tauri::State<'_, AppState>,
    key: String,
    summary: String,
    repo: String,
    number: i64,
    on_chunk: tauri::ipc::Channel<String>,
) -> Result<String, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        if cfg.github_token.is_empty() {
            return Err(GITHUB_TOKEN_MISSING.into());
        }
        let diff = integrations::github::fetch_pr_diff(&cfg.github_token, &repo, number)
            .map_err(|e| e.to_string())?;
        let target = ai_target(&cfg);
        let prompt = crate::ai::gemma::pr_review_prompt(&key, &summary, &diff);
        let body = crate::ai::gemma::build_chat_request(&target.model, &prompt);
        let full = crate::ai::gemma::stream_chat(&target, body, |delta| {
            let _ = on_chunk.send(delta.to_string());
        });
        if full == crate::ai::gemma::AI_UNAVAILABLE {
            return Err(full);
        }
        let _ = db::set_pr_summary(&conn, &repo, number, &full);
        Ok(full)
    })
    .await
}

/// The persisted AI state for one PR: cached summary + chat history. Loaded when
/// a PR is rendered so the summary and follow-up Q&A survive closing the modal.
#[derive(serde::Serialize)]
pub struct PrState {
    pub summary: Option<String>,
    pub chat: Vec<db::PrChatMsg>,
}

/// Load the cached summary + persisted chat for a PR.
#[tauri::command]
pub async fn get_pr_state(
    state: tauri::State<'_, AppState>,
    repo: String,
    number: i64,
) -> Result<PrState, String> {
    with_conn(&state, move |conn| {
        let summary = db::get_pr_summary(&conn, &repo, number).map_err(|e| e.to_string())?;
        let chat = db::list_pr_chat(&conn, &repo, number).map_err(|e| e.to_string())?;
        Ok(PrState { summary, chat })
    })
    .await
}

/// One turn of a PR follow-up chat: `role` is "user" or "assistant".
#[derive(serde::Deserialize)]
pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

/// Answer a QA follow-up question about a PR, grounded in its diff, streaming the
/// answer via `on_chunk`. `history` is the full running conversation (last entry
/// = the new question); `images` are screenshots attached to the new question.
/// The diff is re-fetched each turn, same as [`summarize_pr`].
#[tauri::command]
pub async fn ask_pr(
    state: tauri::State<'_, AppState>,
    key: String,
    summary: String,
    repo: String,
    number: i64,
    history: Vec<ChatMsg>,
    images: Vec<String>,
    on_chunk: tauri::ipc::Channel<String>,
) -> Result<String, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        if cfg.github_token.is_empty() {
            return Err(GITHUB_TOKEN_MISSING.into());
        }
        let diff = integrations::github::fetch_pr_diff(&cfg.github_token, &repo, number)
            .map_err(|e| e.to_string())?;
        let turns: Vec<(String, String)> =
            history.into_iter().map(|m| (m.role, m.content)).collect();
        let target = ai_target(&cfg);
        let prompt = crate::ai::gemma::pr_chat_prompt(&key, &summary, &diff, &turns);
        let body = crate::ai::gemma::build_vision_request_multi(&target.model, &prompt, &images);
        let full = crate::ai::gemma::stream_chat(&target, body, |delta| {
            let _ = on_chunk.send(delta.to_string());
        });
        if full == crate::ai::gemma::AI_UNAVAILABLE {
            return Err(full);
        }
        // Persist the turn (the new question + its answer) so the chat survives
        // closing the modal. `turns.last()` is the just-asked question.
        if let Some((role, content)) = turns.last() {
            let _ = db::add_pr_chat(&conn, &repo, number, role, content, &images);
        }
        let _ = db::add_pr_chat(&conn, &repo, number, "assistant", &full, &[]);
        Ok(full)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::ActivityBlock;
    use crate::integrations::jira::JiraTicket;
    use chrono::{NaiveDateTime, TimeZone, Utc};

    fn ts(s: &str) -> chrono::DateTime<Utc> {
        let s = s.trim_end_matches('Z');
        let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap();
        chrono::Local
            .from_local_datetime(&naive)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn build_dashboard_aggregates_tickets_header_and_timeline() {
        let conn = db::open(":memory:").unwrap();
        let day = "2026-06-18";

        // ABC-1: 1 hour worked (3600s) -> deserved 2.0 points.
        db::insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 work".into(),
                start: ts("2026-06-18T09:00:00Z"),
                end: ts("2026-06-18T10:00:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        // An idle block (should appear in timeline, excluded from rollup).
        db::insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 away".into(),
                start: ts("2026-06-18T10:00:00Z"),
                end: ts("2026-06-18T10:30:00Z"),
                is_idle: true,
            },
        )
        .unwrap();

        // Jira ticket assigned 5 story points -> over the deserved 2.0 by >1 and
        // >20% -> OverPointed.
        integrations::save_tickets(
            &conn,
            &[JiraTicket {
                key: "ABC-1".into(),
                summary: "Login bug".into(),
                status: "In Progress".into(),
                story_points: Some(5.0),
                updated: "2026-06-18T08:00:00Z".into(),
            }],
        )
        .unwrap();

        db::recompute_ticket_time(&conn, day).unwrap();

        let dash = build_dashboard(&conn, day).unwrap();

        assert_eq!(dash.day, day);
        // Header
        assert_eq!(dash.header.net_work_secs, 3600);
        assert!((dash.header.deserved_total - 2.0).abs() < 1e-9);
        assert!((dash.header.assigned_total - 5.0).abs() < 1e-9);

        // Ticket row
        assert_eq!(dash.tickets.len(), 1);
        let t = &dash.tickets[0];
        assert_eq!(t.key, "ABC-1");
        assert_eq!(t.summary, "Login bug");
        assert_eq!(t.worked_secs, 3600);
        assert!((t.deserved - 2.0).abs() < 1e-9);
        assert_eq!(t.story_points, Some(5.0));
        assert_eq!(t.fairness, "OverPointed");

        // Timeline includes both blocks (idle marked).
        assert_eq!(dash.timeline.len(), 2);
        assert!(dash.timeline.iter().any(|b| b.is_idle));
        assert!(dash.timeline.iter().all(|b| b.id > 0));
        let worked = dash.timeline.iter().find(|b| !b.is_idle).unwrap();
        assert_eq!(worked.minutes, 60);
        assert_eq!(worked.ticket_key.as_deref(), Some("ABC-1"));

        // No PRs/notes/summary seeded.
        assert!(dash.prs.is_empty());
        assert_eq!(dash.notes, "");
        assert_eq!(dash.ai_summary, "");
    }

    #[test]
    fn build_dashboard_unknown_ticket_defaults_to_under_pointed() {
        let conn = db::open(":memory:").unwrap();
        let day = "2026-06-18";

        // Worked on XYZ-9 but no jira_tickets row exists -> assigned defaults 0.
        db::insert_block(
            &conn,
            &ActivityBlock {
                app: "Editor".into(),
                title: "XYZ-9 coding".into(),
                start: ts("2026-06-18T09:00:00Z"),
                end: ts("2026-06-18T10:00:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        db::recompute_ticket_time(&conn, day).unwrap();

        let dash = build_dashboard(&conn, day).unwrap();
        assert_eq!(dash.tickets.len(), 1);
        let t = &dash.tickets[0];
        assert_eq!(t.key, "XYZ-9");
        assert_eq!(t.story_points, None);
        assert!((t.assigned - 0.0).abs() < 1e-9);
        // deserved 2.0 vs assigned 0 -> UnderPointed.
        assert_eq!(t.fairness, "UnderPointed");
        assert_eq!(dash.header.assigned_total, 0.0);

        // Notes round-trip into the dashboard.
        db::set_note(&conn, day, "did stuff").unwrap();
        let dash2 = build_dashboard(&conn, day).unwrap();
        assert_eq!(dash2.notes, "did stuff");
    }
}
