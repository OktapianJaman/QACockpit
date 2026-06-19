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
    pub gemma_model: String,
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
        gemma_model: get("gemma_model")?.unwrap_or_default(),
    })
}

fn save_config(conn: &Connection, cfg: &AppConfig) -> Result<(), String> {
    let set = |k: &str, v: &str| db::set_config(conn, k, v).map_err(|e| e.to_string());
    set("jira_base_url", &cfg.jira_base_url)?;
    set("jira_email", &cfg.jira_email)?;
    set("jira_token", &cfg.jira_token)?;
    set("jira_story_point_field", &cfg.jira_story_point_field)?;
    set("jira_project", &cfg.jira_project)?;
    set("jira_assignee", &cfg.jira_assignee)?;
    set("jira_status_category", &cfg.jira_status_category)?;
    set("jira_sprint_scope", &cfg.jira_sprint_scope)?;
    set("github_token", &cfg.github_token)?;
    set("gemma_model", &cfg.gemma_model)?;
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
    load_config(&conn)
}

#[tauri::command]
pub fn set_config(state: tauri::State<'_, AppState>, cfg: AppConfig) -> Result<(), String> {
    let conn = state.conn()?;
    save_config(&conn, &cfg)
}

#[tauri::command]
pub fn sync_now(state: tauri::State<'_, AppState>) -> Result<SyncResult, String> {
    let conn = state.conn()?;
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
pub fn generate_ai_summary(
    state: tauri::State<'_, AppState>,
    day: String,
) -> Result<String, String> {
    let conn = state.conn()?;
    let cfg = load_config(&conn)?;

    let blocks = db::list_blocks_for_day(&conn, &day).map_err(|e| e.to_string())?;

    // Tickets that have time logged today, hydrated from jira_tickets.
    let ticket_time = db::get_ticket_time(&conn, &day).map_err(|e| e.to_string())?;
    let mut tickets = Vec::new();
    for (key, _secs) in ticket_time {
        let (summary, status, story_points) = lookup_jira(&conn, &key);
        tickets.push(crate::integrations::jira::JiraTicket {
            key,
            summary,
            status,
            story_points,
            updated: String::new(),
        });
    }

    let summary = crate::ai::gemma::daily_summary(&cfg.gemma_model, &blocks, &tickets);
    db::set_ai_summary(&conn, &day, "daily", &summary).map_err(|e| e.to_string())?;
    Ok(summary)
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

#[tauri::command]
pub fn list_models() -> Result<Vec<String>, String> {
    Ok(crate::ai::gemma::list_models())
}

/// Reject when the three required Jira credentials aren't all present.
fn require_jira_creds(cfg: &AppConfig) -> Result<(), String> {
    if cfg.jira_base_url.is_empty() || cfg.jira_email.is_empty() || cfg.jira_token.is_empty() {
        return Err("Isi Base URL, Email, dan API token Jira dulu".into());
    }
    Ok(())
}

#[tauri::command]
pub fn list_jira_fields(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<integrations::jira::JiraField>, String> {
    let conn = state.conn()?;
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::fetch_fields(&cfg.jira_base_url, &cfg.jira_email, &cfg.jira_token)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_jira_projects(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<integrations::jira::JiraProject>, String> {
    let conn = state.conn()?;
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::fetch_projects(&cfg.jira_base_url, &cfg.jira_email, &cfg.jira_token)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_jira_assignees(
    state: tauri::State<'_, AppState>,
    project: String,
) -> Result<Vec<integrations::jira::JiraUser>, String> {
    let conn = state.conn()?;
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
}

/// List the workflow transitions available for a Jira issue (e.g. To Do →
/// In Progress → Done). Read-only.
#[tauri::command]
pub fn list_transitions(
    state: tauri::State<'_, AppState>,
    key: String,
) -> Result<Vec<integrations::jira::JiraTransition>, String> {
    let conn = state.conn()?;
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::fetch_transitions(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
    )
    .map_err(|e| e.to_string())
}

/// Move a Jira issue to a new status via `transition_id`. This is a WRITE to
/// Jira — the frontend gates it behind a confirmation dialog. After success the
/// frontend re-syncs, so this command does not re-sync itself.
#[tauri::command]
pub fn transition_issue(
    state: tauri::State<'_, AppState>,
    key: String,
    transition_id: String,
) -> Result<(), String> {
    let conn = state.conn()?;
    let cfg = load_config(&conn)?;
    require_jira_creds(&cfg)?;
    integrations::jira::do_transition(
        &cfg.jira_base_url,
        &cfg.jira_email,
        &cfg.jira_token,
        &key,
        &transition_id,
    )
    .map_err(|e| e.to_string())
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
