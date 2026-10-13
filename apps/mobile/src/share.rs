//! Handing something to another app.
//!
//! The desktop copies to a clipboard the user then pastes wherever they like.
//! A phone has a second, more direct route: the system share sheet, which is
//! how a transcript gets into a chat or a note. Only text travels this way —
//! Android's chooser takes the payload, and an image is already reachable
//! through the photo viewer's own copy.
//!
//! The host owns the intent; this module is only the launch and the reason it
//! might not happen.

use vibex_backend::{BackendError, BackendResult};

fn unavailable() -> BackendError {
    BackendError::unsupported(
        "mobile_share_unavailable",
        "Sharing is unavailable on this device",
    )
}

#[cfg(target_os = "android")]
mod android {
    use super::{BackendError, BackendResult};

    pub fn share_text(text: &str) -> BackendResult<()> {
        crate::android_bridge::with_activity(|env, activity| {
            let text = env.new_string(text)?;
            env.call_method(
                activity,
                jni::jni_str!("gpuiShareText"),
                jni::jni_sig!((text: JString) -> ()),
                &[(&text).into()],
            )?;
            Ok(())
        })
        .ok_or_else(|| {
            BackendError::failed(
                "mobile_share_launch_failed",
                "The share sheet could not be opened",
            )
        })
    }
}

#[cfg(target_os = "android")]
pub use android::share_text;

/// Android is the platform this ships on first; another host has to provide its
/// own share sheet before a Share control does anything.
#[cfg(not(target_os = "android"))]
pub fn share_text(_text: &str) -> BackendResult<()> {
    Err(unavailable())
}
