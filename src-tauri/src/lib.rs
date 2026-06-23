mod ai;
mod commands;
mod core;
mod db;
mod integrations;
mod net;
mod recorder;

use commands::AppState;
use recorder::Recorder;
use std::path::PathBuf;

// Retained so the default template frontend (replaced in M7) still resolves.
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Resolve the on-disk database path, creating the parent directory.
/// Uses `~/Library/Application Support/site.hexalabs.qacockpit/qacockpit.db`.
fn resolve_db_path() -> PathBuf {
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base
        .join("Library")
        .join("Application Support")
        .join("site.hexalabs.qacockpit");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("qacockpit.db")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = resolve_db_path().to_string_lossy().to_string();

    // Ensure the schema exists before any command runs.
    if let Err(e) = db::open(&db_path) {
        eprintln!("failed to initialize database at {db_path}: {e}");
    }

    // Build the shared HTTP client eagerly on the main thread, so its internal
    // runtime is never created/dropped from within a Tauri async worker.
    let _ = net::client();

    let state = AppState {
        recorder: Recorder::new(db_path.clone()),
        db_path,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::recorder_start,
            commands::recorder_stop,
            commands::recorder_status,
            commands::screen_recording_ok,
            commands::get_config,
            commands::set_config,
            commands::test_jira_connection,
            commands::test_github_connection,
            commands::test_gemini_connection,
            commands::sync_now,
            commands::recompute,
            commands::save_note,
            commands::set_ticket_for_block,
            commands::generate_ai_summary,
            commands::get_daily_summary,
            commands::get_dashboard,
            commands::today,
            commands::list_jira_fields,
            commands::list_jira_projects,
            commands::list_jira_assignees,
            commands::list_transitions,
            commands::transition_issue,
            commands::list_board_tickets,
            commands::set_story_points,
            commands::list_test_cases,
            commands::add_test_case,
            commands::set_test_case_status,
            commands::set_test_case_notes,
            commands::update_test_case,
            commands::delete_test_case,
            commands::generate_test_cases,
            commands::generate_test_cases_from_pr,
            commands::generate_test_cases_from_prs,
            commands::post_test_results,
            commands::generate_bug_report,
            commands::create_jira_bug,
            commands::parse_ticket_blob,
            commands::create_story_tickets,
            commands::list_ticket_prs,
            commands::summarize_pr,
            commands::ask_pr,
            commands::get_pr_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
