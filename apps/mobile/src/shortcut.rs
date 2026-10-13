//! Launcher shortcuts.
//!
//! Android lets a long press on the app icon offer actions before the app is
//! even on screen. The shortcut's intent reaches `GpuiHostActivity`, which is
//! already running when the panel is warm, so the request is handed to the
//! panel through this channel rather than by restarting the activity.
//!
//! The desktop has no equivalent — a launcher has no icon to long-press — so
//! this is the phone's own way of skipping the session list.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// What a launcher shortcut asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchShortcut {
    /// Open the new-session page.
    NewSession,
}

impl LaunchShortcut {
    /// Parses the token the host sends. An unknown token is dropped rather
    /// than guessed at, so a future shortcut cannot open the wrong page on an
    /// older build.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "new_session" => Some(Self::NewSession),
            _ => None,
        }
    }
}

struct ShortcutInbox {
    sender: Option<futures_channel::mpsc::UnboundedSender<LaunchShortcut>>,
    /// Requests that arrived before the panel subscribed. A cold start
    /// delivers the intent before the app has drawn anything, so dropping it
    /// would lose the shortcut's whole purpose.
    pending: VecDeque<LaunchShortcut>,
}

fn inbox() -> &'static Mutex<ShortcutInbox> {
    static INBOX: OnceLock<Mutex<ShortcutInbox>> = OnceLock::new();
    INBOX.get_or_init(|| {
        Mutex::new(ShortcutInbox {
            sender: None,
            pending: VecDeque::new(),
        })
    })
}

/// Records a shortcut the host reported.
pub fn deliver(token: &str) {
    let Some(shortcut) = LaunchShortcut::parse(token) else {
        return;
    };
    if let Ok(mut inbox) = inbox().lock() {
        match inbox.sender.as_ref() {
            Some(sender) => {
                let _ = sender.unbounded_send(shortcut);
            }
            None => inbox.pending.push_back(shortcut),
        }
    }
}

/// Takes the shortcut channel, replaying anything that arrived first.
pub fn subscribe() -> futures_channel::mpsc::UnboundedReceiver<LaunchShortcut> {
    let (sender, receiver) = futures_channel::mpsc::unbounded();
    if let Ok(mut inbox) = inbox().lock() {
        inbox.sender = Some(sender.clone());
        for shortcut in inbox.pending.drain(..) {
            let _ = sender.unbounded_send(shortcut);
        }
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_shortcut_token_is_ignored() {
        assert_eq!(
            LaunchShortcut::parse("new_session"),
            Some(LaunchShortcut::NewSession)
        );
        assert_eq!(LaunchShortcut::parse("something_else"), None);
    }

    #[test]
    fn a_shortcut_delivered_before_subscribing_is_replayed() {
        deliver("new_session");
        let mut receiver = subscribe();
        assert_eq!(receiver.try_recv().ok(), Some(LaunchShortcut::NewSession));
    }
}
