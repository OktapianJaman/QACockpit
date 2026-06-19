//! Idle-time probe (macOS) via Core Graphics.
//!
//! core-graphics 0.23 ships the `CGEventSourceStateID` and `CGEventType` enums
//! but does **not** wrap `CGEventSourceSecondsSinceLastEventType`, so we declare
//! that one C function ourselves and reuse the crate's enums for type safety.
//! The CoreGraphics framework is already linked via the crate's `link` feature.

use core_graphics::event::CGEventType;
use core_graphics::event_source::CGEventSourceStateID;

#[cfg_attr(
    target_os = "macos",
    link(name = "CoreGraphics", kind = "framework")
)]
extern "C" {
    /// Seconds elapsed since the last event of `event_type` from `state_id`.
    /// `CGEventType::Null` means "any event type".
    fn CGEventSourceSecondsSinceLastEventType(
        state_id: CGEventSourceStateID,
        event_type: CGEventType,
    ) -> f64;
}

/// Seconds since the last user input (any keyboard/mouse event).
///
/// Returns 0 on any failure or a negative/NaN reading.
pub fn idle_seconds() -> u64 {
    let secs = unsafe {
        CGEventSourceSecondsSinceLastEventType(
            CGEventSourceStateID::CombinedSessionState,
            CGEventType::Null,
        )
    };
    if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    }
}
