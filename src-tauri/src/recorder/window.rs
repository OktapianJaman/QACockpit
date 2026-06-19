//! Active-window probe (macOS).
//!
//! Thin wrapper over `active-win-pos-rs`. Reading the window *title* on macOS
//! requires Screen Recording permission; the *app name* is always available.

use active_win_pos_rs::get_active_window;

/// Return the focused window as `(app_name, window_title)`, or `None` on error.
///
/// `title` may legitimately be empty (some apps don't expose one, or Screen
/// Recording permission is missing — see [`screen_recording_permission_ok`]).
pub fn current_window() -> Option<(String, String)> {
    match get_active_window() {
        Ok(w) => Some((w.app_name, w.title)),
        Err(_) => None,
    }
}

/// Heuristic check for whether Screen Recording permission appears granted.
///
/// We can't query the permission directly without extra entitlements, so we
/// infer it: if an app is focused but its title is empty, permission is
/// *likely* missing. This is only a heuristic — some apps legitimately report
/// an empty title even with permission granted (e.g. an empty desktop, or apps
/// that don't name their windows), so a `false` here is not proof of denial.
pub fn screen_recording_permission_ok() -> bool {
    current_window().map_or(false, |(_app, title)| !title.is_empty())
}
