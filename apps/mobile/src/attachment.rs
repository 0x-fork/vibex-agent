//! Picking an image attachment from the phone.
//!
//! The desktop reads attachments from paths it can open; a phone's picker never
//! hands out a path at all. The Android host therefore reads the picked item
//! itself, scales it down, and encodes it as a `data:` URL, which is the one
//! attachment URI the authoritative runtime can materialize on its own side.
//! This module is only the phone half of that: launch, and receive.

use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use vibex_backend::{BackendError, BackendResult};

/// One image the picker handed back, already encoded.
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
}

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
        "Attaching images is unavailable on this device",
    )
}

#[cfg(target_os = "android")]
mod android {
    use jni::{
        EnvUnowned,
        objects::{JClass, JString},
    };

    use super::{AttachmentPickEvent, BackendError, BackendResult, enqueue};

    pub fn launch() -> BackendResult<()> {
        crate::android_bridge::with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("gpuiPickImageAttachment"),
                jni::jni_sig!(() -> ()),
                &[],
            )?;
            Ok(())
        })
        .ok_or_else(|| {
            BackendError::failed(
                "mobile_attachment_picker_launch_failed",
                "The image picker could not be opened",
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
/// own picker bridge before the composer offers the button.
#[cfg(not(target_os = "android"))]
pub fn launch() -> BackendResult<()> {
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
    fn a_failure_is_reported_with_its_reason() {
        let mut receiver = subscribe();
        enqueue(AttachmentPickEvent::Failed("nope".to_string()));

        assert_eq!(
            receiver.try_recv().unwrap(),
            AttachmentPickEvent::Failed("nope".to_string())
        );
    }
}
