//! Picking an attachment from the phone.
//!
//! The desktop reads attachments from paths it can open; a phone's picker never
//! hands out a path at all. The Android host therefore reads the picked item
//! itself and encodes it as a `data:` URL, which is the one attachment URI the
//! authoritative runtime can materialize on its own side. An image is scaled
//! down first, because a camera-sized photograph is far past what the relay
//! carries; any other file travels whole up to the wire budget. This module is
//! only the phone half of that: launch, and receive.

use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use vibex_backend::{BackendError, BackendResult};

/// Which picker the composer asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// A photograph or screenshot, scaled down before it is encoded.
    Image,
    /// Any other file, carried whole.
    File,
    /// The camera, whose capture is scaled the way a picked image is.
    Camera,
}

/// One picked item the picker handed back, already encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedAttachment {
    pub label: String,
    pub mime_type: Option<String>,
    /// A `data:<mime>;base64,<payload>` URI.
    pub data_url: String,
}

/// What came back from one trip to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentPickEvent {
    Picked(PickedAttachment),
    /// The pick failed for a reason worth showing. A cancelled pick reports
    /// nothing at all, so the composer keeps what it had.
    Failed(String),
    /// Text another app shared into Vibex, which belongs in the composer.
    ///
    /// It travels on this channel because it arrives the same way a picked
    /// attachment does: out of band, from the host, whenever the user shares
    /// something with the app. An image shared in is reported as `Picked`.
    SharedText(String),
}

/// The wallpaper picker: one image, kept at full resolution.
///
/// A wallpaper is not an attachment — it is never scaled, never relayed, and it
/// lands in the app's own wallpaper directory rather than in a message — but
/// the host's job is the same one: grant the app read access to a single picked
/// item and hand its bytes back. It has its own channel because the bytes are
/// consumed by the appearance settings, not by the composer, and a pick that
/// lands after the settings page closed must not become a chat attachment.
mod wallpaper {
    use std::sync::{Mutex, OnceLock};

    use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
    use vibex_backend::{BackendError, BackendResult};

    /// What came back from one trip to the wallpaper picker.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BackgroundPickEvent {
        /// A `data:<mime>;base64,<payload>` URI holding the picked image.
        Picked(String),
        /// The pick failed for a reason worth showing. A cancelled pick reports
        /// nothing at all, so the current wallpaper stays.
        Failed(String),
    }

    fn event_sender() -> &'static Mutex<Option<UnboundedSender<BackgroundPickEvent>>> {
        static SENDER: OnceLock<Mutex<Option<UnboundedSender<BackgroundPickEvent>>>> =
            OnceLock::new();
        SENDER.get_or_init(|| Mutex::new(None))
    }

    fn enqueue(event: BackgroundPickEvent) {
        if let Ok(sender) = event_sender().lock()
            && let Some(sender) = sender.as_ref()
        {
            let _ = sender.unbounded_send(event);
        }
    }

    pub fn subscribe() -> UnboundedReceiver<BackgroundPickEvent> {
        let (sender, receiver) = mpsc::unbounded();
        if let Ok(mut current) = event_sender().lock() {
            *current = Some(sender);
        }
        receiver
    }

    pub fn launch() -> BackendResult<()> {
        platform_launch()
    }

    #[cfg(target_os = "android")]
    fn platform_launch() -> BackendResult<()> {
        crate::android_bridge::with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("gpuiPickBackgroundImage"),
                jni::jni_sig!(() -> ()),
                &[],
            )?;
            Ok(())
        })
        .ok_or_else(|| {
            BackendError::failed(
                "mobile_background_picker_launch_failed",
                "The image picker could not be opened",
            )
        })
    }

    /// Android is the platform this ships on first; another host has to provide
    /// its own picker bridge before the appearance page offers the control.
    #[cfg(not(target_os = "android"))]
    fn platform_launch() -> BackendResult<()> {
        Err(BackendError::unsupported(
            "mobile_background_picker_unavailable",
            "Choosing a background image is unavailable on this device",
        ))
    }

    /// Delivers a picked wallpaper to the waiting appearance page.
    ///
    /// The Android host calls this after reading the picked item whole; a
    /// cancelled pick calls nothing, which is what leaves the current image in
    /// place without an error.
    #[cfg(target_os = "android")]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnBackgroundImagePicked<
        'caller,
    >(
        mut unowned_env: jni::EnvUnowned<'caller>,
        _class: jni::objects::JClass<'caller>,
        data_url: jni::objects::JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue(BackgroundPickEvent::Picked(data_url.to_string()));
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }

    #[cfg(target_os = "android")]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnBackgroundImagePickFailed<
        'caller,
    >(
        mut unowned_env: jni::EnvUnowned<'caller>,
        _class: jni::objects::JClass<'caller>,
        message: jni::objects::JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue(BackgroundPickEvent::Failed(message.to_string()));
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_picked_wallpaper_reaches_the_subscriber() {
            let mut receiver = subscribe();
            enqueue(BackgroundPickEvent::Picked(
                "data:image/png;base64,aGVsbG8=".to_string(),
            ));
            assert_eq!(
                receiver.try_recv().unwrap(),
                BackgroundPickEvent::Picked("data:image/png;base64,aGVsbG8=".to_string())
            );
        }

        #[test]
        fn a_wallpaper_failure_is_reported_with_its_reason() {
            let mut receiver = subscribe();
            enqueue(BackgroundPickEvent::Failed("nope".to_string()));
            assert_eq!(
                receiver.try_recv().unwrap(),
                BackgroundPickEvent::Failed("nope".to_string())
            );
        }
    }
}

pub use wallpaper::{
    BackgroundPickEvent, launch as launch_background_picker,
    subscribe as subscribe_background_picker,
};

fn event_sender() -> &'static Mutex<Option<UnboundedSender<AttachmentPickEvent>>> {
    static SENDER: OnceLock<Mutex<Option<UnboundedSender<AttachmentPickEvent>>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(None))
}

fn enqueue(event: AttachmentPickEvent) {
    if let Ok(sender) = event_sender().lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.unbounded_send(event);
    }
}

pub fn subscribe() -> UnboundedReceiver<AttachmentPickEvent> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut current) = event_sender().lock() {
        *current = Some(sender);
    }
    receiver
}

fn unavailable() -> BackendError {
    BackendError::unsupported(
        "mobile_attachment_picker_unavailable",
        "Attaching files is unavailable on this device",
    )
}

#[cfg(target_os = "android")]
mod android {
    use jni::{
        EnvUnowned,
        objects::{JClass, JString},
    };

    use super::{AttachmentKind, AttachmentPickEvent, BackendError, BackendResult, enqueue};

    pub fn launch(kind: AttachmentKind) -> BackendResult<()> {
        let method = match kind {
            AttachmentKind::Image => jni::jni_str!("gpuiPickImageAttachment"),
            AttachmentKind::File => jni::jni_str!("gpuiPickFileAttachment"),
            AttachmentKind::Camera => jni::jni_str!("gpuiCapturePhotoAttachment"),
            AttachmentKind::Background => jni::jni_str!("gpuiPickBackgroundImage"),
        };
        crate::android_bridge::with_activity(|env, activity| {
            env.call_method(activity, method, jni::jni_sig!(() -> ()), &[])?;
            Ok(())
        })
        .ok_or_else(|| {
            BackendError::failed(
                "mobile_attachment_picker_launch_failed",
                "The file picker could not be opened",
            )
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAttachmentPicked<
        'caller,
    >(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        label: JString<'caller>,
        mime_type: JString<'caller>,
        data_url: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                let label = label.to_string();
                let mime_type = mime_type.to_string();
                let data_url = data_url.to_string();
                enqueue(AttachmentPickEvent::Picked(super::PickedAttachment {
                    label,
                    mime_type: (!mime_type.is_empty()).then_some(mime_type),
                    data_url,
                }));
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }

    /// Text shared into Vibex from another app.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnSharedText<'caller>(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        text: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue(AttachmentPickEvent::SharedText(text.to_string()));
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnLaunchShortcut<'caller>(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        shortcut: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                crate::shortcut::deliver(&shortcut.to_string());
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAttachmentPickFailed<
        'caller,
    >(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        message: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue(AttachmentPickEvent::Failed(message.to_string()));
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }
}

#[cfg(target_os = "android")]
pub use android::launch;

/// Android is the platform this ships on first; another host has to provide its
/// own picker bridge before the composer offers the buttons.
#[cfg(not(target_os = "android"))]
pub fn launch(_kind: AttachmentKind) -> BackendResult<()> {
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_attachment_reaches_the_subscriber() {
        let mut receiver = subscribe();
        enqueue(AttachmentPickEvent::Picked(PickedAttachment {
            label: "shot.png".to_string(),
            mime_type: Some("image/png".to_string()),
            data_url: "data:image/png;base64,aGVsbG8=".to_string(),
        }));

        let event = receiver.try_recv().expect("the event is delivered");
        assert_eq!(
            event,
            AttachmentPickEvent::Picked(PickedAttachment {
                label: "shot.png".to_string(),
                mime_type: Some("image/png".to_string()),
                data_url: "data:image/png;base64,aGVsbG8=".to_string(),
            })
        );
    }

    #[test]
    fn shared_text_reaches_the_subscriber() {
        let mut receiver = subscribe();
        enqueue(AttachmentPickEvent::SharedText("look at this".to_string()));

        assert_eq!(
            receiver.try_recv().unwrap(),
            AttachmentPickEvent::SharedText("look at this".to_string())
        );
    }

    #[test]
    fn a_failure_is_reported_with_its_reason() {
        let mut receiver = subscribe();
        enqueue(AttachmentPickEvent::Failed("nope".to_string()));

        assert_eq!(
            receiver.try_recv().unwrap(),
            AttachmentPickEvent::Failed("nope".to_string())
        );
    }
}
