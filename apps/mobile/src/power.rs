//! Battery-optimization allowlist awareness for reliable background delivery.
//!
//! Android's Doze and App Standby modes can suspend network access to a
//! backgrounded app even while its foreground service is running, so the app
//! asks the user to exempt it from battery optimizations. Whether the user
//! granted that exemption is only knowable on Android; on every other platform
//! the query reports `true` so the settings row stays hidden.

#[cfg(target_os = "android")]
mod platform {
    /// Holds the screen on, or lets it go.
    ///
    /// Android's own idle timer is the only thing that turns the display off
    /// while the app is in front of the user, and this is the flag that tells
    /// it not to. It is cleared whenever the app stops wanting it, so a phone
    /// left alone in the app still sleeps.
    pub fn keep_screen_on(keep_on: bool) {
        let _ = crate::android_bridge::with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("gpuiKeepScreenOn"),
                jni::jni_sig!((keep_on: jboolean) -> ()),
                &[jni::objects::JValue::Bool(u8::from(keep_on))],
            )?;
            Ok(())
        });
    }

    pub fn is_ignoring_battery_optimizations() -> bool {
        crate::android_bridge::with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("isIgnoringBatteryOptimizations"),
                jni::jni_sig!(() -> bool),
                &[],
            )?
            .z()
        })
        .unwrap_or(true)
    }

    pub fn request_ignore_battery_optimizations() {
        let _ = crate::android_bridge::with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("requestIgnoreBatteryOptimizations"),
                jni::jni_sig!(() -> ()),
                &[],
            )?;
            Ok(())
        });
    }
}

#[cfg(target_os = "android")]
pub use platform::{
    is_ignoring_battery_optimizations, keep_screen_on, request_ignore_battery_optimizations,
};

#[cfg(not(target_os = "android"))]
pub fn is_ignoring_battery_optimizations() -> bool {
    true
}

#[cfg(not(target_os = "android"))]
pub fn request_ignore_battery_optimizations() {}

/// Android is the platform this ships on first; another host has to hold its
/// own display awake before the setting does anything.
#[cfg(not(target_os = "android"))]
pub fn keep_screen_on(_keep_on: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_android_platforms_report_the_allowlist_as_granted() {
        // Non-Android platforms must not surface the allowlist prompt; the
        // query returning `true` is what keeps the settings row hidden.
        assert!(is_ignoring_battery_optimizations());
    }
}
