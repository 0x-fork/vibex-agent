//! The phone's lightweight hints.
//!
//! Every notice the mobile client shows is a toast: it appears over whatever
//! screen is up, holds for a few seconds, and leaves on its own or under a tap.
//! Nothing here takes a place in a page's layout. A phone has one narrow column
//! of content, and a status line pinned under a list or above the composer both
//! pushes that content around and stays there until some later state change
//! happens to clear it — which is how a stale transport message ends up sitting
//! on the pairing screen long after the connection it describes is gone.
//!
//! The surface is the kit's `Notification` on the window's notification layer,
//! the same one the desktop workbench delivers its hints through, so tone,
//! autohide and replace-by-id behave identically on both clients. `theme.rs`
//! moves the layer to the phone's own corner and width.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px,
};
use gpui_component::WindowExt as _;
use gpui_component::notification::{Notification, NotificationType};

use crate::theme;

/// The weight a hint carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// A completed step worth naming — a rename, an added project.
    Success,
    /// The app could not finish what was asked.
    Error,
    /// The connection changed under the user.
    Warning,
    /// Anything else worth saying out loud.
    Info,
}

impl Tone {
    fn notification_type(self) -> NotificationType {
        match self {
            Self::Success => NotificationType::Success,
            Self::Error => NotificationType::Error,
            Self::Warning => NotificationType::Warning,
            Self::Info => NotificationType::Info,
        }
    }

    /// The debug selector for a hint of this weight.
    ///
    /// The key is the tone rather than the message because a laid-out element's
    /// selector has to be a `&'static str`: a message is not known until the
    /// hint is built, a tone is known at compile time. A test can therefore
    /// assert the weight of what is on screen, and how many hints there are,
    /// which together say whether an announcement happened at all.
    pub fn selector(self) -> &'static str {
        match self {
            Self::Success => "mobile-hint-success",
            Self::Error => "mobile-hint-error",
            Self::Warning => "mobile-hint-warning",
            Self::Info => "mobile-hint-info",
        }
    }
}

/// One hint waiting for the next frame, or already on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub tone: Tone,
    pub message: String,
}

impl Hint {
    pub fn new(tone: Tone, message: impl Into<String>) -> Self {
        Self {
            tone,
            message: message.into(),
        }
    }
}

/// Distinct marker so the kit keys mobile hints apart from any other
/// notification family that ever lands on this window.
struct MobileHint;

/// Puts one hint on the window's notification layer.
///
/// The hint's text is its identity: asking for the same thing twice replaces
/// the hint on screen instead of stacking a second copy of it, which is what
/// keeps a repeating event — a connection that keeps dropping — from filling
/// the corner with identical cards. Different messages stack, newest at the
/// bottom, and the layer caps how many are visible.
pub fn push(window: &mut Window, cx: &mut App, hint: &Hint) {
    // A window that never got the kit's root — a bare test window — has no
    // notification layer to push to, and `WindowExt::push_notification` panics
    // on the missing window state instead of doing nothing. Every window the
    // application opens is built by `open_root_window`, so this only decides
    // what a test window without a layer does with a hint: drop it.
    if window.root::<gpui_component::Root>().flatten().is_none() {
        return;
    }
    let message: SharedString = hint.message.clone().into();
    let key = hint.tone.selector();
    // The kit's own close button only appears under a pointer, and a phone has
    // none, so the card itself is the dismiss target: the kit takes the hint
    // down before it runs this handler, which therefore has nothing left to do.
    let notification = Notification::new()
        .with_type(hint.tone.notification_type())
        .id1::<MobileHint>(hint.message.clone())
        .on_click(|_, _, _| {})
        .content(move |_, _, _| {
            div()
                .w_full()
                .min_w_0()
                .text_size(px(theme::FONT_BODY))
                .debug_selector({
                    let key = key.to_string();
                    move || key.clone()
                })
                .child(message.clone())
                .into_any_element()
        });
    window.push_notification(notification, cx);
}
