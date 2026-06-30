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
use tauri::Emitter;

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
    /// Sprint scope: "" (all-time) | "active" (current sprint) | "backlog" |
    /// "specific" (the one sprint in `jira_sprint`).
    pub jira_sprint_scope: String,
    /// Numeric id of the chosen sprint when `jira_sprint_scope` == "specific"
    /// (e.g. "9348"). Empty otherwise. Resolved via the Agile board API.
    #[serde(default)]
    pub jira_sprint: String,
    pub github_token: String,
    /// Google Gemini API key (the only AI provider). The model is hardcoded
    /// (see [`crate::ai::gemma::GEMINI_MODEL`]) and not user-configurable.
    #[serde(default)]
    pub gemini_api_key: String,
    /// Output language for AI generation (test cases, etc.): "Indonesia" |
    /// "English". Empty/legacy configs default to "Indonesia" in `load_config`.
    #[serde(default)]
    pub ai_language: String,
    /// Absolute path to the local GTI Flutter repo (gotradeindoapp) used by the
    /// integration-test / instrumentation pipeline. Empty = not configured.
    #[serde(default)]
    pub gti_path: String,
    /// Absolute path to the local GTG Flutter repo (tradecharlieflutter). Empty =
    /// not configured.
    #[serde(default)]
    pub gtg_path: String,
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
        jira_sprint: get("jira_sprint")?.unwrap_or_default(),
        github_token: get("github_token")?.unwrap_or_default(),
        gemini_api_key: get("gemini_api_key")?.unwrap_or_default(),
        ai_language: get("ai_language")?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Indonesia".to_string()),
        gti_path: get("gti_path")?.unwrap_or_default(),
        gtg_path: get("gtg_path")?.unwrap_or_default(),
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
    set("jira_sprint", &cfg.jira_sprint)?;
    set_secret("github_token", &cfg.github_token)?;
    set_secret("gemini_api_key", &cfg.gemini_api_key)?;
    set("ai_language", &cfg.ai_language)?;
    set("gti_path", &cfg.gti_path)?;
    set("gtg_path", &cfg.gtg_path)?;
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
        &cfg.jira_sprint,
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

/// List the active + future sprints for a project (for the Settings "Sprint
/// tertentu" picker). Resolves the project's board, then its sprints. Falls
/// back to the saved project when the caller passes an empty one; an empty
/// project yields an empty list. Read-only.
#[tauri::command]
pub async fn list_jira_sprints(
    state: tauri::State<'_, AppState>,
    project: String,
) -> Result<Vec<integrations::jira::JiraSprint>, String> {
    with_conn(&state, move |conn| {
        let cfg = load_config(&conn)?;
        require_jira_creds(&cfg)?;
        let project = if project.trim().is_empty() {
            cfg.jira_project.clone()
        } else {
            project
        };
        integrations::jira::fetch_sprints(
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
    // These come ONLY from the manual ✅/❌ buttons — stamp a [manual] marker in
    // the reason so the UI can distinguish a hand-set verdict from a real
    // automation result (never show a manual override as an automation pass).
    db::set_test_case_verdict(&conn, id, &status, "[manual] di-set manual oleh QA")
        .map_err(|e| e.to_string())
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

/// Run ONE test case on the connected device via the Mata engine, streaming live
/// progress to the frontend as `device_run` events, and persist the 3-state
/// verdict (passed | failed | not_auto) + reason. Returns the final status.
///
/// Bridges to `tools/mobile-agent/run_cockpit.py`, which emits NDJSON
/// (phase / frame / log / verdict) on stdout; we tag each line with the case id
/// and re-emit it. The subprocess read loop is blocking, so it runs on a blocking
/// thread to keep the async runtime free.
/// PIDs of in-flight device runs, keyed by test-case id, so `stop_device_run` can
/// terminate one. A run inserts its child pid on spawn and removes it on exit.
fn running_runs() -> &'static std::sync::Mutex<std::collections::HashMap<i64, u32>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, u32>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Stop a running device run (kills the bridge subprocess + its adb children).
#[tauri::command]
pub fn stop_device_run(id: i64) -> Result<(), String> {
    let pid = running_runs().lock().unwrap().get(&id).copied();
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            // Kill the bridge's children (adb/screencap) first, then the bridge.
            let _ = std::process::Command::new("pkill")
                .args(["-P", &pid.to_string()])
                .status();
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
    }
    Ok(())
}

/// List recent Firebase App Distribution builds for an app ("GTI" | "GTG").
/// Returns objects: { version, displayVersion, buildVersion, date, notes }.
#[tauri::command]
pub fn list_firebase_builds(app: String) -> Result<Vec<serde_json::Value>, String> {
    let agent_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/mobile-agent");
    let out = std::process::Command::new("python3")
        .arg("firebase_apk.py")
        .arg(&app)
        .arg("--json")
        .current_dir(agent_dir)
        .output()
        .map_err(|e| format!("gagal jalanin firebase_apk: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "firebase_apk error: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let txt = String::from_utf8_lossy(&out.stdout);
    let line = txt.lines().rev().find(|l| l.trim_start().starts_with('[')).unwrap_or("[]");
    serde_json::from_str(line.trim()).map_err(|e| format!("parse builds: {e}"))
}

/// List adb devices/emulators currently in the `device` state (serials only).
#[tauri::command]
pub fn list_adb_devices() -> Result<Vec<String>, String> {
    let out = std::process::Command::new("adb")
        .arg("devices")
        .output()
        .map_err(|e| format!("adb tidak ditemukan: {e}"))?;
    let txt = String::from_utf8_lossy(&out.stdout);
    let mut serials = Vec::new();
    for line in txt.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((serial, state)) = line.split_once('\t') {
            if state.trim() == "device" {
                serials.push(serial.trim().to_string());
            }
        }
    }
    Ok(serials)
}

#[tauri::command]
pub async fn run_test_case_on_device(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: i64,
    serial: Option<String>,
    fresh: Option<bool>,
    fb_app: Option<String>,
    version: Option<String>,
) -> Result<String, String> {
    let db_path = state.db_path.clone();
    let tc = {
        let conn = state.conn()?;
        db::get_test_case(&conn, id).map_err(|e| e.to_string())?
    };
    let fresh = fresh.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        run_device_pipeline(app, db_path, id, tc, serial, fresh, fb_app, version)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Blocking pipeline behind [`run_test_case_on_device`]: spawn the Python bridge,
/// relay its NDJSON events as `device_run`, and write the final verdict to the db.
fn run_device_pipeline(
    app: tauri::AppHandle,
    db_path: String,
    id: i64,
    tc: db::TestCase,
    serial: Option<String>,
    fresh: bool,
    fb_app: Option<String>,
    version: Option<String>,
) -> Result<String, String> {
    use std::process::{Command, Stdio};

    let emit = |mut v: serde_json::Value| {
        if let Some(map) = v.as_object_mut() {
            map.insert("id".into(), serde_json::json!(id));
        }
        let _ = app.emit("device_run", v);
    };

    if let Ok(conn) = db::open(&db_path) {
        let _ = db::set_test_case_verdict(&conn, id, "running", "");
    }
    emit(serde_json::json!({"t":"phase","key":"setup","label":"Menyiapkan device","state":"start"}));

    let agent_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/mobile-agent");
    let tcjson = serde_json::json!({
        "title": tc.title, "steps": tc.steps, "expected": tc.expected,
    });
    let tmp = std::env::temp_dir().join(format!("qacockpit_tc_{id}.json"));
    std::fs::write(&tmp, tcjson.to_string()).map_err(|e| e.to_string())?;

    let mut cmd = Command::new("python3");
    cmd.arg("run_cockpit.py")
        .arg(&tmp)
        .current_dir(agent_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(s) = serial.as_ref().filter(|s| !s.is_empty()) {
        cmd.env("ANDROID_SERIAL", s).arg("--serial").arg(s);
    }
    if fresh {
        cmd.arg("--fresh");
    }
    // Optional: download+install a specific Firebase build before running.
    if let (Some(a), Some(v)) = (
        fb_app.as_ref().filter(|s| !s.is_empty()),
        version.as_ref().filter(|s| !s.is_empty()),
    ) {
        cmd.arg("--app").arg(a).arg("--version").arg(v);
    }
    relay_bridge_run(&app, &db_path, id, cmd)
}

/// Spawn a bridge subprocess, relay its NDJSON lines as `device_run` events,
/// persist the final verdict, and return the status. Shared by the vision and
/// integration-test pipelines. The PID is registered so `stop_device_run` works.
fn relay_bridge_run(
    app: &tauri::AppHandle,
    db_path: &str,
    id: i64,
    mut cmd: std::process::Command,
) -> Result<String, String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let emit = |mut v: serde_json::Value| {
        if let Some(map) = v.as_object_mut() {
            map.insert("id".into(), serde_json::json!(id));
        }
        let _ = app.emit("device_run", v);
    };

    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("gagal start bridge: {e}"))?;
    running_runs().lock().unwrap().insert(id, child.id());

    let stdout = child.stdout.take().ok_or("no stdout from bridge")?;
    let mut verdict = "not_auto".to_string();
    let mut reason = String::new();
    let mut got_verdict = false;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("t").and_then(|x| x.as_str()) == Some("verdict") {
                got_verdict = true;
                verdict = v.get("verdict").and_then(|x| x.as_str()).unwrap_or("not_auto").to_string();
                reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string();
            }
            emit(v);
        }
    }
    let _ = child.wait();
    running_runs().lock().unwrap().remove(&id);

    if !got_verdict {
        reason = "Run dihentikan sebelum selesai.".to_string();
        emit(serde_json::json!({"t":"verdict","verdict":"not_auto","reason":reason}));
    }

    let status = match verdict.as_str() {
        "pass" => "passed",
        "fail" => "failed",
        _ => "not_auto",
    };
    if let Ok(conn) = db::open(db_path) {
        let _ = db::set_test_case_verdict(&conn, id, status, &reason);
    }
    emit(serde_json::json!({"t":"done","status":status,"reason":reason}));
    Ok(status.to_string())
}

/// Run the FLUTTER INTEGRATION TEST track for a test case: instrument the repo
/// source with QA Keys (streamed live), then `flutter test` builds from source +
/// runs it on the device → verdict. No APK download (the test compiles into the
/// build). The repo path comes from Settings (`gti_path`).
#[tauri::command]
pub async fn run_integration_test(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: i64,
    serial: Option<String>,
    app_key: Option<String>,
    email: Option<String>,
) -> Result<String, String> {
    let db_path = state.db_path.clone();
    let app_key = app_key.unwrap_or_else(|| "GTI".to_string()).to_uppercase();
    let (gti_path, gtg_path, tc) = {
        let conn = state.conn()?;
        let cfg = load_config(&conn)?;
        let tc = db::get_test_case(&conn, id).map_err(|e| e.to_string())?;
        (cfg.gti_path, cfg.gtg_path, tc)
    };
    tauri::async_runtime::spawn_blocking(move || {
        run_integration_pipeline(
            app, db_path, id, serial, app_key, gti_path, gtg_path,
            tc.ticket_key, tc.title, email,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn run_integration_pipeline(
    app: tauri::AppHandle,
    db_path: String,
    id: i64,
    serial: Option<String>,
    app_key: String,
    gti_path: String,
    gtg_path: String,
    ticket: String,
    title: String,
    email: Option<String>,
) -> Result<String, String> {
    use std::process::Command;

    if let Ok(conn) = db::open(&db_path) {
        let _ = db::set_test_case_verdict(&conn, id, "running", "");
    }

    // The python bridge decides feasibility from the test case's TITLE: it runs
    // the real automation if one exists (FLOWS), or emits an honest not_auto with
    // a specific reason (TRIAGE) — never a fake verdict. We pass both repo paths
    // so it can pick the repo for whichever app the matched flow belongs to.
    let agent_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/mobile-agent");
    let mut cmd = Command::new("python3");
    cmd.arg("instrument_gti.py")
        .arg("--case-id")
        .arg(id.to_string())
        .arg("--ticket")
        .arg(&ticket)
        .arg("--title")
        .arg(&title)
        .arg("--app")
        .arg(&app_key)
        .arg("--gti-dir")
        .arg(&gti_path)
        .arg("--gtg-dir")
        .arg(&gtg_path)
        .current_dir(agent_dir);
    if let Some(e) = email.as_ref().filter(|s| !s.is_empty()) {
        cmd.arg("--email").arg(e);
    }
    if let Some(s) = serial.as_ref().filter(|s| !s.is_empty()) {
        cmd.env("ANDROID_SERIAL", s).arg("--serial").arg(s);
    }
    relay_bridge_run(&app, &db_path, id, cmd)
}

#[derive(serde::Serialize, Clone)]
pub struct TriageCase {
    pub id: i64,
    pub title: String,
    pub bucket: String,
    pub verdict: String,
    pub reason: String,
}

/// Run a bridge command to completion and capture only the final verdict line
/// (verdict + bucket + reason). Used by ticket triage — no live streaming.
fn bridge_capture(mut cmd: std::process::Command) -> (String, String, String) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ("not_auto".into(), "config".into(), format!("gagal start bridge: {e}")),
    };
    let (mut verdict, mut bucket, mut reason) =
        ("not_auto".to_string(), "unknown".to_string(), String::new());
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v.get("t").and_then(|x| x.as_str()) == Some("verdict") {
                    verdict = v.get("verdict").and_then(|x| x.as_str()).unwrap_or("not_auto").to_string();
                    bucket = v.get("bucket").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
                    reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
            }
        }
    }
    let _ = child.wait();
    (verdict, bucket, reason)
}

/// Pull likely code identifiers from a test case (backtick-quoted tokens +
/// camelCase / snake_case / TitleCase symbol-ish words). These are what we grep
/// the repos for, to give the AI classifier real evidence about the build.
fn classify_search_terms(title: &str, steps: &str, expected: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let hay = format!("{title}\n{steps}\n{expected}");
    let mut terms: BTreeSet<String> = BTreeSet::new();
    let mut cur = String::new();
    let mut consider = |t: &str, set: &mut BTreeSet<String>| {
        if t.len() < 4 {
            return;
        }
        let has_upper = t.chars().any(|c| c.is_uppercase());
        let has_lower = t.chars().any(|c| c.is_lowercase());
        let symbolish = t.contains('_')
            || (has_upper && has_lower)
            || t.ends_with("Screen")
            || t.ends_with("Field")
            || t.ends_with("Button")
            || t.ends_with("Message")
            || t.ends_with("Dialog")
            || t.ends_with("Widget");
        if symbolish {
            set.insert(t.to_string());
        }
    };
    for ch in hay.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            consider(&cur, &mut terms);
            cur.clear();
        }
    }
    if !cur.is_empty() {
        consider(&cur, &mut terms);
    }
    terms.into_iter().take(12).collect()
}

/// Grep each term in each repo's lib/, returning compact evidence (or an explicit
/// "(TIDAK ADA)" when a term is absent — that absence is the strongest signal of
/// spec-vs-build drift).
fn classify_evidence(repos: &[(String, String)], terms: &[String]) -> String {
    use std::process::Command;
    let mut out = String::new();
    if terms.is_empty() {
        return out;
    }
    for term in terms {
        let mut found_any = false;
        let mut block = format!("`{term}`:\n");
        for (label, repo) in repos {
            if repo.trim().is_empty() || !std::path::Path::new(repo).is_dir() {
                continue;
            }
            if let Ok(o) = Command::new("grep")
                .args(["-rIn", "--include=*.dart", "-m", "2", term, "lib"])
                .current_dir(repo)
                .output()
            {
                let s = String::from_utf8_lossy(&o.stdout);
                for l in s.lines().take(2) {
                    found_any = true;
                    let l = if l.len() > 160 { &l[..160] } else { l };
                    block.push_str(&format!("  [{label}] {}\n", l.trim()));
                }
            }
        }
        if !found_any {
            block = format!("`{term}`: (TIDAK ADA di kode)\n");
        }
        out.push_str(&block);
        if out.len() > 5000 {
            break;
        }
    }
    out
}

/// Code context derived from a ticket's linked PR(s) — the *real* files the
/// feature touched, which beats literal-term grep for backend-driven work.
#[derive(Default, Clone)]
struct PrContext {
    /// Compact diff digest (changed .dart files + added lines) for the AI prompt.
    digest: String,
    /// Absolute local paths of changed .dart files, for the generator to read.
    files: Vec<String>,
}

/// Map a GitHub `OWNER/REPO` to the matching local repo path by basename
/// (e.g. ".../gotradeindoapp" matches "tr8-io/gotradeindoapp").
fn repo_local_path(pr_repo: &str, gti: &str, gtg: &str) -> Option<String> {
    let base = |p: &str| std::path::Path::new(p).file_name()
        .and_then(|s| s.to_str()).map(|s| s.to_lowercase());
    let pr = pr_repo.rsplit('/').next().unwrap_or(pr_repo).to_lowercase();
    for p in [gti, gtg] {
        if p.trim().is_empty() {
            continue;
        }
        if base(p).as_deref() == Some(pr.as_str()) {
            return Some(p.to_string());
        }
    }
    None
}

/// Resolve a ticket's linked PR(s) and build code context from their diffs.
/// Primary source is the ticket SUMMARY convention ("[GTG] … #3250") — reliable
/// because the PR rarely mentions the Jira key; falls back to a GitHub key
/// search. Returns an empty context (never errors) when no token is set, no PR
/// is found, or the network fails — callers fall back to grep.
fn ticket_pr_context(cfg: &AppConfig, summary: &str, ticket_key: &str, gti: &str, gtg: &str) -> PrContext {
    use crate::integrations::github;
    let token = cfg.github_token.trim();
    if token.is_empty() {
        return PrContext::default();
    }
    // (repo, number) pairs: summary convention first, then key search.
    let mut refs = github::parse_pr_refs_from_summary(summary);
    if refs.is_empty() && !ticket_key.trim().is_empty() {
        if let Ok(prs) = github::search_prs_for_key(token, ticket_key) {
            refs = prs.into_iter().map(|p| (p.repo, p.number)).collect();
        }
    }
    let mut ctx = PrContext::default();
    // Cap at a few PRs so triage stays responsive and the prompt stays bounded.
    for (repo, number) in refs.into_iter().take(3) {
        let diff = match github::fetch_pr_diff(token, &repo, number) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let (paths, digest) = github::diff_digest(&diff, 40, 6000);
        if digest.is_empty() {
            continue;
        }
        ctx.digest.push_str(&format!("PR #{number} ({repo}):\n{digest}\n"));
        if let Some(local) = repo_local_path(&repo, gti, gtg) {
            for rel in paths {
                let abs = std::path::Path::new(&local).join(&rel);
                if abs.is_file() {
                    if let Some(s) = abs.to_str() {
                        if !ctx.files.contains(&s.to_string()) {
                            ctx.files.push(s.to_string());
                        }
                    }
                }
            }
        }
        if ctx.digest.len() > 12_000 {
            break;
        }
    }
    ctx
}

/// Fetch a ticket's summary from the local DB (empty if unknown).
fn ticket_summary(db_path: &str, ticket_key: &str) -> String {
    db::open(db_path)
        .ok()
        .and_then(|conn| {
            conn.query_row(
                "SELECT summary FROM jira_tickets WHERE key = ?1",
                rusqlite::params![ticket_key],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
        })
        .unwrap_or_default()
}

/// Ask Gemini to classify a test case against real code evidence into
/// spec_drift | buildable | manual. Returns (bucket, reason). Falls back to
/// ("unknown", …) on any failure so triage never hard-fails.
fn ai_classify_case(
    cfg: &AppConfig,
    repos: &[(String, String)],
    title: &str,
    steps: &str,
    expected: &str,
    pr_ctx: &PrContext,
) -> (String, String) {
    if cfg.gemini_api_key.trim().is_empty() {
        return (
            "unknown".into(),
            "Set API key Gemini di Pengaturan buat auto-triage.".into(),
        );
    }
    let terms = classify_search_terms(title, steps, expected);
    let evidence = classify_evidence(repos, &terms);
    // The ticket's PR diff (when available) is the strongest signal: it's the
    // exact code the feature changed. Lead with it so backend-driven cases that
    // term-grep would miss get classified buildable, not falsely manual/drift.
    let pr_block = if pr_ctx.digest.trim().is_empty() {
        "(tidak ada PR ke-link / token GitHub belum di-set)".to_string()
    } else {
        pr_ctx.digest.clone()
    };
    let prompt = format!(
        "Kamu QA automation engineer untuk app Flutter. Klasifikasikan SATU test case \
ke salah satu bucket, BERDASARKAN BUKTI KODE (hasil grep di repo). Jujur — kalau \
fitur/flag/field yang disebut test case tidak ada di bukti, itu spec_drift.\n\n\
BUCKETS:\n\
- spec_drift: fitur/flag/field/layar yang disebut test case TIDAK ADA di kode \
(bukti '(TIDAK ADA)'), atau jelas beda dari implementasi. Test case ngarang/usang.\n\
- buildable: fitur ADA di kode & bisa diuji deterministik sebagai flutter widget/unit \
test (logic, mapping, format, default value, state).\n\
- manual: butuh kamera/scan/OCR asli/vision/interaksi device yang nggak bisa jadi \
flutter test otomatis.\n\n\
TEST CASE:\n\
Judul: {title}\n\
Langkah: {steps}\n\
Diharapkan: {expected}\n\n\
DIFF PR TIKET INI (sinyal TERKUAT — kode yang beneran diubah fitur ini; \
kalau field/flag/layar test case muncul di sini, itu buildable, bukan manual):\n\
{pr_block}\n\n\
BUKTI KODE GREP (per `term`: cuplikan, atau '(TIDAK ADA)'):\n{evidence}\n\n\
Jawab HANYA JSON satu baris, tanpa markdown: \
{{\"bucket\":\"spec_drift|buildable|manual\",\"reason\":\"1 kalimat ringkas, sebut term/kode spesifik\"}}"
    );
    let resp = crate::ai::gemma::complete(&ai_target(cfg), &prompt);
    let (start, end) = (resp.find('{'), resp.rfind('}'));
    if let (Some(s), Some(e)) = (start, end) {
        if e > s {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp[s..=e]) {
                let bucket = v.get("bucket").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if matches!(bucket.as_str(), "spec_drift" | "buildable" | "manual") {
                    // Tag PR-informed verdicts so the cache can tell them apart
                    // from older grep-only ones and refresh the stale grep ones.
                    let tag = if pr_ctx.digest.trim().is_empty() { "[AI]" } else { "[AI+PR]" };
                    return (bucket, format!("{tag} {reason}"));
                }
            }
        }
    }
    (
        "unknown".into(),
        "AI nggak bisa klasifikasi case ini — cek manual.".into(),
    )
}

/// Triage EVERY test case in a ticket: classify each (and run the automatable
/// ones for a real pass/fail), then return the breakdown. `fast` only classifies
/// (instant, no flutter test) — automatable cases come back as bucket "auto"
/// without a real verdict. This turns `not_auto` into a useful QA report instead
/// of a dead end: it tells you which cases are real bugs, which are spec-drift
/// (test case ≠ build), which are buildable, and which need a human.
#[tauri::command]
pub async fn triage_ticket(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    ticket: String,
    serial: Option<String>,
    app_key: Option<String>,
    email: Option<String>,
    fast: Option<bool>,
) -> Result<Vec<TriageCase>, String> {
    let db_path = state.db_path.clone();
    let app_key = app_key.unwrap_or_else(|| "GTI".to_string()).to_uppercase();
    let fast = fast.unwrap_or(false);
    let (cfg, gti_path, gtg_path, cases) = {
        let conn = state.conn()?;
        let cfg = load_config(&conn)?;
        let cases = db::list_test_cases(&conn, &ticket).map_err(|e| e.to_string())?;
        (cfg.clone(), cfg.gti_path, cfg.gtg_path, cases)
    };
    // Repos to grep for code evidence (label -> path); both apps, so a case is
    // classified honestly regardless of which app it belongs to.
    let repos: Vec<(String, String)> = vec![
        ("GTI".to_string(), gti_path.clone()),
        ("GTG".to_string(), gtg_path.clone()),
    ];

    tauri::async_runtime::spawn_blocking(move || {
        use std::process::Command;
        let agent_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/mobile-agent");
        let total = cases.len();

        // Resolve the ticket's PR(s) ONCE — all cases share the ticket key, so
        // the diff context is fetched a single time and reused for every case.
        let summary = ticket_summary(&db_path, &ticket);
        let pr_ctx = ticket_pr_context(&cfg, &summary, &ticket, &gti_path, &gtg_path);
        if !pr_ctx.files.is_empty() {
            let _ = app.emit("triage", serde_json::json!(
                {"t":"pr_context","files":pr_ctx.files.len()}));
        }
        let _ = app.emit("triage", serde_json::json!(
            {"t":"start","ticket":ticket,"total":total}));

        let mut out: Vec<TriageCase> = Vec::with_capacity(total);
        for (i, tc) in cases.iter().enumerate() {
            let _ = app.emit("triage", serde_json::json!(
                {"t":"running","id":tc.id,"title":tc.title,"done":i,"total":total}));

            let mk_cmd = |classify: bool| {
                let mut c = Command::new("python3");
                c.arg("instrument_gti.py")
                    .arg("--case-id").arg(tc.id.to_string())
                    .arg("--ticket").arg(&tc.ticket_key)
                    .arg("--title").arg(&tc.title)
                    .arg("--app").arg(&app_key)
                    .arg("--gti-dir").arg(&gti_path)
                    .arg("--gtg-dir").arg(&gtg_path)
                    .current_dir(agent_dir);
                if classify { c.arg("--classify"); }
                if let Some(e) = email.as_ref().filter(|s| !s.is_empty()) {
                    c.arg("--email").arg(e);
                }
                if let Some(s) = serial.as_ref().filter(|s| !s.is_empty()) {
                    c.env("ANDROID_SERIAL", s).arg("--serial").arg(s);
                }
                c
            };

            // 1) Curated registry first (instant). FLOWS/TRIAGE win over the AI.
            let (mut verdict, mut bucket, mut reason) = bridge_capture(mk_cmd(true));

            // 2) Unknown → AI classifier, grounded in real code (cached in DB).
            if bucket == "unknown" {
                // A cached verdict is reusable UNLESS we now have PR-diff context
                // that the cache didn't use (older "[AI]" / grep-only verdicts) —
                // those are stale and must be recomputed with the better signal.
                let pr_available = !pr_ctx.digest.trim().is_empty();
                let cache_fresh = !tc.triage_bucket.is_empty()
                    && tc.triage_bucket != "unknown"
                    && (!pr_available || tc.verdict_reason.starts_with("[AI+PR]"));
                if cache_fresh {
                    bucket = tc.triage_bucket.clone();
                    reason = tc.verdict_reason.clone();
                } else {
                    let (b, r) = ai_classify_case(&cfg, &repos, &tc.title, &tc.steps, &tc.expected, &pr_ctx);
                    bucket = b;
                    reason = r;
                    if let Ok(conn) = db::open(&db_path) {
                        let _ = db::set_triage(&conn, tc.id, &bucket, &reason);
                    }
                }
            }

            // 3) Automatable + full run → actually execute the flow for pass/fail.
            if bucket == "auto" && !fast {
                let (v, _, r) = bridge_capture(mk_cmd(false));
                verdict = v;
                reason = r;
            }

            // Persist the row verdict (skip when fast+auto: not actually run).
            if !(fast && bucket == "auto") {
                let status = match verdict.as_str() {
                    "pass" => "passed",
                    "fail" => "failed",
                    _ => "not_auto",
                };
                if let Ok(conn) = db::open(&db_path) {
                    let _ = db::set_test_case_verdict(&conn, tc.id, status, &reason);
                }
            }

            let case = TriageCase {
                id: tc.id, title: tc.title.clone(),
                bucket, verdict, reason,
            };
            let _ = app.emit("triage", serde_json::json!(
                {"t":"case","done":i+1,"total":total,"case":case}));
            out.push(case);
        }
        let _ = app.emit("triage", serde_json::json!({"t":"done","total":total}));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pick which repo a case belongs to by counting how many of its code-identifier
/// terms appear in each repo's lib/. The repo with more hits wins.
fn pick_repo(terms: &[String], gti: &str, gtg: &str) -> String {
    let count = |repo: &str| -> usize {
        if repo.trim().is_empty() || !std::path::Path::new(repo).is_dir() {
            return 0;
        }
        let mut n = 0;
        for t in terms {
            if let Ok(o) = std::process::Command::new("grep")
                .args(["-rIl", "--include=*.dart", "-m", "1", t, "lib"])
                .current_dir(repo)
                .output()
            {
                n += String::from_utf8_lossy(&o.stdout).lines().count();
            }
        }
        n
    };
    let (cg, cgg) = (count(gti), count(gtg));
    if cgg > cg && !gtg.trim().is_empty() {
        gtg.to_string()
    } else if !gti.trim().is_empty() {
        gti.to_string()
    } else {
        gtg.to_string()
    }
}

/// Core of a single test generation (blocking): runs gen_test.py for one case,
/// streams its `gen_run` events (tagged with the case id), persists the verdict,
/// and returns it. Shared by the single-case command and the bulk loop. Must be
/// called from a blocking context.
fn run_one_generation(
    app: &tauri::AppHandle,
    db_path: &str,
    cfg: &AppConfig,
    gti: &str,
    gtg: &str,
    tc: &db::TestCase,
    quiet_code: bool,
) -> String {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let id = tc.id;
    let emit = |mut v: serde_json::Value| {
        // In bulk (parallel) mode several cases stream at once; the single live
        // panel can't show interleaved `code` payloads, so drop them — the
        // gen_bulk tally + per-case status carry the progress instead.
        if quiet_code && v.get("t").and_then(|x| x.as_str()) == Some("code") {
            return;
        }
        if let Some(m) = v.as_object_mut() {
            m.insert("id".into(), serde_json::json!(id));
        }
        let _ = app.emit("gen_run", v);
    };

    let terms = classify_search_terms(&tc.title, &tc.steps, &tc.expected);
    let repo = pick_repo(&terms, gti, gtg);
    if repo.trim().is_empty() || !std::path::Path::new(&repo).is_dir() {
        emit(serde_json::json!({"t":"verdict","verdict":"fail",
            "reason":"Repo gak ke-set / gak ketemu. Set path repo di Pengaturan (⚙)."}));
        emit(serde_json::json!({"t":"done","verdict":"fail"}));
        return "fail".to_string();
    }

    // Real feature files from the ticket's PR diff → primary context.
    let summary = ticket_summary(db_path, &tc.ticket_key);
    let pr_ctx = ticket_pr_context(cfg, &summary, &tc.ticket_key, gti, gtg);
    let ctx_files: Vec<String> = pr_ctx.files.iter()
        .filter(|f| f.starts_with(&repo)).cloned().collect();
    if !ctx_files.is_empty() {
        emit(serde_json::json!({"t":"step","msg":
            format!("konteks dari PR tiket: {} file diubah", ctx_files.len())}));
    }

    let agent_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/mobile-agent");
    let mut cmd = Command::new("python3");
    cmd.arg("gen_test.py")
        .arg("--case-id").arg(id.to_string())
        .arg("--ticket").arg(&tc.ticket_key)
        .arg("--repo").arg(&repo)
        .arg("--model").arg("gemini-2.5-pro")
        .arg("--title").arg(&tc.title)
        .arg("--steps").arg(&tc.steps)
        .arg("--expected").arg(&tc.expected)
        .arg("--tries").arg("4");
    if !ctx_files.is_empty() {
        cmd.arg("--context-files").arg(ctx_files.join(","));
    }
    cmd.current_dir(agent_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emit(serde_json::json!({"t":"verdict","verdict":"fail","reason":format!("gagal start generator: {e}")}));
            emit(serde_json::json!({"t":"done","verdict":"fail"}));
            return "fail".to_string();
        }
    };
    running_runs().lock().unwrap().insert(id, child.id());

    let (mut verdict, mut reason) = ("fail".to_string(), String::new());
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v.get("t").and_then(|x| x.as_str()) == Some("verdict") {
                    verdict = v.get("verdict").and_then(|x| x.as_str()).unwrap_or("fail").to_string();
                    reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
                emit(v);
            }
        }
    }
    let _ = child.wait();
    running_runs().lock().unwrap().remove(&id);

    if let Ok(conn) = db::open(db_path) {
        if verdict == "pass" {
            let _ = db::set_triage(&conn, id, "auto", "");
            let _ = db::set_test_case_verdict(&conn, id, "passed", "");
        } else {
            // Generator couldn't build it honestly → effectively manual.
            let _ = db::set_triage(&conn, id, "manual", &reason);
        }
    }
    emit(serde_json::json!({"t":"done","verdict":verdict,"reason":reason}));
    verdict
}

/// "✨ Bikin test (AI)": auto-generate a Flutter test for a buildable case with
/// Gemini 2.5 Pro (gather context → write → compile+run → self-repair loop).
/// On pass the test is stored under test/generated/ (auto-discovered on future
/// runs) and the case flips to auto-pass. Streams `gen_run` progress events.
#[tauri::command]
pub async fn generate_case_test(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: i64,
) -> Result<String, String> {
    let db_path = state.db_path.clone();
    let (cfg, gti, gtg, tc) = {
        let conn = state.conn()?;
        let cfg = load_config(&conn)?;
        let tc = db::get_test_case(&conn, id).map_err(|e| e.to_string())?;
        (cfg.clone(), cfg.gti_path, cfg.gtg_path, tc)
    };
    tauri::async_runtime::spawn_blocking(move || {
        Ok(run_one_generation(&app, &db_path, &cfg, &gti, &gtg, &tc, false))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "⚙️ Generate semua buildable": bulk-generate tests for every `buildable` case
/// in a ticket, SEQUENTIALLY (flutter test shares a build dir — no safe
/// parallelism). The seam gate inside gen_test.py routes UI-interaction cases to
/// manual in seconds, so only real logic seams cost time. Streams per-case
/// `gen_run` events (reused by the live panel) plus `gen_bulk` tally events.
/// `max` caps how many cases run (cost guard); 0/None = all buildable.
#[tauri::command]
pub async fn generate_ticket_tests(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    ticket: String,
    max: Option<i64>,
) -> Result<serde_json::Value, String> {
    let db_path = state.db_path.clone();
    let (cfg, gti, gtg, cases) = {
        let conn = state.conn()?;
        let cfg = load_config(&conn)?;
        let cases = db::list_test_cases(&conn, &ticket).map_err(|e| e.to_string())?;
        (cfg.clone(), cfg.gti_path.clone(), cfg.gtg_path.clone(), cases)
    };
    // Only the buildable ones; respect an optional cap as a cost guard.
    let mut todo: Vec<db::TestCase> =
        cases.into_iter().filter(|c| c.triage_bucket == "buildable").collect();
    if let Some(m) = max {
        if m > 0 && (m as usize) < todo.len() {
            todo.truncate(m as usize);
        }
    }

    tauri::async_runtime::spawn_blocking(move || {
        use std::sync::atomic::{AtomicI64, Ordering};
        use std::sync::Arc;
        // The slow part of each case is the Gemini call (~10-30s), which is
        // independent across cases — so run them CONCURRENTLY. The `flutter test`
        // step is serialized per repo by a file lock in gen_test.py (shared build
        // dir), so builds never race. Cap concurrency to keep API + machine sane.
        const CONC: usize = 3;
        let total = todo.len();
        let _ = app.emit("gen_bulk", serde_json::json!({"t":"start","ticket":ticket,"total":total}));
        let cfg = Arc::new(cfg);
        let passed = Arc::new(AtomicI64::new(0));
        let manual = Arc::new(AtomicI64::new(0));
        let done = Arc::new(AtomicI64::new(0));
        for chunk in todo.chunks(CONC) {
            let mut handles = Vec::new();
            for tc in chunk {
                let (app, db_path) = (app.clone(), db_path.clone());
                let (cfg, gti, gtg, tc) = (cfg.clone(), gti.clone(), gtg.clone(), tc.clone());
                let (passed, manual, done) = (passed.clone(), manual.clone(), done.clone());
                handles.push(std::thread::spawn(move || {
                    let _ = app.emit("gen_bulk", serde_json::json!(
                        {"t":"case","id":tc.id,"title":tc.title}));
                    let verdict = run_one_generation(&app, &db_path, &cfg, &gti, &gtg, &tc, true);
                    if verdict == "pass" { passed.fetch_add(1, Ordering::SeqCst); }
                    else { manual.fetch_add(1, Ordering::SeqCst); }
                    let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = app.emit("gen_bulk", serde_json::json!(
                        {"t":"case_done","id":tc.id,"verdict":verdict,"done":d,"total":total,
                         "passed":passed.load(Ordering::SeqCst),"manual":manual.load(Ordering::SeqCst)}));
                }));
            }
            for h in handles { let _ = h.join(); }
        }
        let (passed, manual) = (passed.load(Ordering::SeqCst), manual.load(Ordering::SeqCst));
        let _ = app.emit("gen_bulk", serde_json::json!(
            {"t":"done","total":total,"passed":passed,"manual":manual}));
        Ok(serde_json::json!({"total":total,"passed":passed,"manual":manual}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "▶ Run semua test": re-run EVERY generated widget test for an app in a SINGLE
/// `flutter test` invocation (one compile, isolates run concurrently) instead of
/// one process per file. Measured ~3.2× faster than per-file (15.2s → 4.8s for 6
/// tests). A fast regression check — e.g. after merging develop into the QA
/// branch. `--no-pub` skips the per-run pub-get (deps warmed once).
#[tauri::command]
pub async fn run_generated_tests(
    state: tauri::State<'_, AppState>,
    app_key: Option<String>,
) -> Result<serde_json::Value, String> {
    let app_key = app_key.unwrap_or_else(|| "GTI".to_string()).to_uppercase();
    let repo = {
        let conn = state.conn()?;
        let cfg = load_config(&conn)?;
        repo_for_app(&cfg, &app_key)
    };
    if repo.trim().is_empty() || !std::path::Path::new(&repo).is_dir() {
        return Err(format!("Repo {app_key} gak ke-set / gak ketemu. Set di Pengaturan (⚙)."));
    }
    let gen_dir = std::path::Path::new(&repo).join("test/generated");
    if !gen_dir.is_dir() {
        return Ok(serde_json::json!({"total":0,"passed":0,"failed":0,
            "reason":"Belum ada test ter-generate di test/generated/."}));
    }
    tauri::async_runtime::spawn_blocking(move || {
        use std::process::Command;
        // Warm deps once, then run all in one invocation with --no-pub.
        let _ = Command::new("fvm").args(["flutter", "pub", "get"]).current_dir(&repo).output();
        let out = Command::new("fvm")
            .args(["flutter", "test", "--no-pub", "test/generated/"])
            .current_dir(&repo)
            .output()
            .map_err(|e| format!("gagal start flutter test: {e}"))?;
        let txt = format!("{}{}",
            String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        // Flutter's compact reporter streams "+P -F:" counters (e.g. "+10:" all
        // pass, or "+8 -2:" with failures). Tokens may carry a trailing ':'.
        let (mut passed, mut failed) = (0i64, 0i64);
        for cap in txt.split_whitespace() {
            let c = cap.trim_end_matches(':');
            if let Some(n) = c.strip_prefix('+').and_then(|s| s.parse::<i64>().ok()) { passed = n; }
            if let Some(n) = c.strip_prefix('-').and_then(|s| s.parse::<i64>().ok()) { failed = n; }
        }
        let all_pass = txt.contains("All tests passed!");
        if all_pass { failed = 0; }
        let tail: String = txt.lines().rev().take(20).collect::<Vec<_>>()
            .into_iter().rev().collect::<Vec<_>>().join("\n");
        Ok(serde_json::json!({
            "total": passed + failed, "passed": passed, "failed": failed,
            "all_pass": all_pass, "tail": tail,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- QA branch sync (one-way: merge develop INTO the qa branch) ------------

/// Status of the app's local QA test-harness branch relative to origin/develop.
#[derive(serde::Serialize, Default)]
pub struct QaBranchStatus {
    pub repo: String,
    pub branch: String,
    pub behind: i64,
    pub ahead: i64,
    pub dirty: bool,
    /// Non-empty when something prevents a safe sync (not a git repo, on
    /// develop/main, git missing, …). The UI shows this instead of a sync.
    pub error: String,
}

/// Resolve the local repo path for an app key ("GTI"/"GTG").
fn repo_for_app(cfg: &AppConfig, app_key: &str) -> String {
    if app_key.eq_ignore_ascii_case("GTG") {
        cfg.gtg_path.clone()
    } else {
        cfg.gti_path.clone()
    }
}

/// Run a git command in `repo`, returning (success, stdout_trimmed, stderr_trimmed).
fn git(repo: &str, args: &[&str]) -> (bool, String, String) {
    match std::process::Command::new("git")
        .arg("-C").arg(repo)
        .args(args)
        .output()
    {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (false, String::new(), e.to_string()),
    }
}

/// `git fetch origin develop`, authenticating with the cockpit's GitHub token
/// via a scoped `http.extraheader` (the same scheme actions/checkout uses) so it
/// doesn't depend on the ambient credential helper — the cockpit is launched
/// outside the user's shell and otherwise has no creds for private repos.
/// Returns (success, stderr_with_token_scrubbed).
fn fetch_develop(repo: &str, token: &str) -> (bool, String) {
    let token = token.trim();
    if token.is_empty() {
        let (ok, _, err) = git(repo, &["fetch", "origin", "develop"]);
        return (ok, err);
    }
    use base64::Engine;
    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("x-access-token:{token}"));
    let header = format!("http.https://github.com/.extraheader=AUTHORIZATION: basic {auth}");
    let (ok, _, err) = git(repo, &["-c", &header, "fetch", "origin", "develop"]);
    // Defensive: never let the encoded token escape into a surfaced message.
    (ok, err.replace(&auth, "***"))
}

/// Inspect the QA branch: current branch, how far behind/ahead origin/develop,
/// and whether the tree is dirty. Fetches origin/develop first so `behind` is
/// accurate. Read-only — never mutates the working tree.
#[tauri::command]
pub async fn qa_branch_status(
    state: tauri::State<'_, AppState>,
    app_key: String,
) -> Result<QaBranchStatus, String> {
    let cfg = { load_config(&state.conn()?)? };
    let repo = repo_for_app(&cfg, &app_key);
    tauri::async_runtime::spawn_blocking(move || {
        let mut st = QaBranchStatus { repo: repo.clone(), ..Default::default() };
        if repo.trim().is_empty() || !std::path::Path::new(&repo).is_dir() {
            st.error = "Path repo belum di-set / nggak ketemu (Pengaturan ⚙).".into();
            return Ok(st);
        }
        let (ok, branch, _) = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        if !ok {
            st.error = "Bukan git repo (atau git nggak ada).".into();
            return Ok(st);
        }
        st.branch = branch;
        st.dirty = !git(&repo, &["status", "--porcelain"]).1.is_empty();
        // Best-effort refresh of origin/develop. Network/auth failures are
        // SILENT here (no error in the badge) — the count just falls back to the
        // last-known origin/develop. A real fetch+merge happens on Sync.
        let _ = fetch_develop(&repo, &cfg.github_token);
        st.behind = git(&repo, &["rev-list", "--count", "HEAD..origin/develop"])
            .1.parse().unwrap_or(0);
        st.ahead = git(&repo, &["rev-list", "--count", "origin/develop..HEAD"])
            .1.parse().unwrap_or(0);
        Ok(st)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Merge origin/develop INTO the current QA branch (one-way refresh). Guards:
/// refuses on develop/main/master, refuses a dirty tree, aborts on conflict.
/// Never pushes. Returns a human-readable result string.
#[tauri::command]
pub async fn sync_qa_branch(
    state: tauri::State<'_, AppState>,
    app_key: String,
) -> Result<String, String> {
    let cfg = { load_config(&state.conn()?)? };
    let repo = repo_for_app(&cfg, &app_key);
    tauri::async_runtime::spawn_blocking(move || {
        if repo.trim().is_empty() || !std::path::Path::new(&repo).is_dir() {
            return Err("Path repo belum di-set / nggak ketemu (Pengaturan ⚙).".to_string());
        }
        let (ok, branch, _) = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        if !ok {
            return Err("Bukan git repo (atau git nggak ada).".to_string());
        }
        // NEVER merge into develop/main — the QA branch is one-way downstream.
        if matches!(branch.as_str(), "develop" | "main" | "master") {
            return Err(format!(
                "Lagi di branch '{branch}'. Checkout dulu branch QA (mis. qa/automation) — \
sync cuma boleh develop → branch QA, bukan sebaliknya."
            ));
        }
        if !git(&repo, &["status", "--porcelain"]).1.is_empty() {
            return Err("Working tree masih kotor (ada perubahan belum di-commit). \
Commit/stash dulu sebelum sync.".to_string());
        }
        let (fok, ferr) = fetch_develop(&repo, &cfg.github_token);
        if !fok {
            let hint = if cfg.github_token.trim().is_empty() {
                " (Set GitHub token di Pengaturan ⚙ — repo ini private.)"
            } else {
                ""
            };
            return Err(format!("git fetch gagal:{hint}\n{ferr}"));
        }
        let behind: i64 = git(&repo, &["rev-list", "--count", "HEAD..origin/develop"])
            .1.parse().unwrap_or(0);
        if behind == 0 {
            return Ok(format!("'{branch}' udah paling baru — nggak ada yang perlu di-merge."));
        }
        // merge, NOT rebase — the branch is shared/pulled by teammates.
        let (mok, mout, merr) = git(&repo, &["merge", "--no-edit", "origin/develop"]);
        if mok {
            return Ok(format!("✅ '{branch}' di-merge dari develop ({behind} commit). \
Jangan lupa push biar tim ikut ke-update."));
        }
        // Conflict → abort so the tree stays clean; tell the user to resolve manually.
        let combined = format!("{mout}\n{merr}");
        let _ = git(&repo, &["merge", "--abort"]);
        Err(format!(
            "Merge konflik — di-abort otomatis (tree balik bersih). Resolve manual:\n\
  cd {repo} && git merge origin/develop\n\n{}",
            combined.trim()
        ))
    })
    .await
    .map_err(|e| e.to_string())?
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
