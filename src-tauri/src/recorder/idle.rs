//! Idle-time probe (macOS) via Core Graphics.
//!
//! core-graphics 0.23 ships the `CGEventSourceStateID` and `CGEventType` enums
//! but does **not** wrap `CGEventSourceSecondsSinceLastEventType`, so we declare
//! that one C function ourselves and reuse the crate's enums for type safety.
//! The CoreGraphics framework is already linked via the crate's `link` feature.

use core_graphics::event_source::CGEventSourceStateID;

/// Sentinel event type meaning "any user input event" (keyboard, mouse, etc.).
/// This is `kCGAnyInputEventType` from CoreGraphics — NOT `CGEventType::Null` (0),
/// which would measure time since the (almost never emitted) null event and so
/// always report the session as idle. `CGEventType` is a `uint32_t` in C, so we
/// pass the raw value rather than going through the crate's enum.
const ANY_INPUT_EVENT_TYPE: u32 = 0xFFFF_FFFF;

#[cfg_attr(
    target_os = "macos",
    link(name = "CoreGraphics", kind = "framework")
)]
extern "C" {
    /// Seconds elapsed since the last event of `event_type` from `state_id`.
    fn CGEventSourceSecondsSinceLastEventType(
        state_id: CGEventSourceStateID,
        event_type: u32,
    ) -> f64;
}

/// Seconds since the last user input (any keyboard/mouse event).
///
/// Returns 0 on any failure or a negative/NaN reading.
pub fn idle_seconds() -> u64 {
    let secs = unsafe {
        CGEventSourceSecondsSinceLastEventType(
            CGEventSourceStateID::CombinedSessionState,
            ANY_INPUT_EVENT_TYPE,
        )
    };
    if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    }
}
