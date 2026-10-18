//! The system's back gesture, as the phone reports it.
//!
//! Android's gesture navigation owns the screen's side strips: a swipe from an
//! edge belongs to the system from its first pixel, so the page never sees the
//! touch at all (see `scroll_capture`, which is why a side page cannot be
//! dragged out by a touch pan anywhere on the edge). What the app *is* given is
//! the gesture itself, through the platform's back callback: from Android 14 on
//! it names the edge the finger came from and how far it has travelled, which
//! is what lets that same swipe pull the matching side page out under the
//! finger while the system keeps its own back semantics.
//!
//! Below Android 14 the callback carries no edge and no progress, so a back
//! gesture there is only ever the app's ordinary back: the host reports
//! [`BackGestureEvent::Invoked`] with [`BackGestureEdge::Unknown`] and nothing
//! is pulled open. iOS reports nothing at all; its own edge pan is a touch the
//! app receives like any other.

use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// Which edge the back gesture was swiped in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackGestureEdge {
    Left,
    Right,
    /// The platform did not say: a back button, or a version whose back
    /// callback reports neither edge nor progress.
    Unknown,
}

/// One phase of the system's back gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackGestureEvent {
    /// The gesture has been recognized after the touch moved past the system's
    /// own slop. `progress` is zero here.
    Started { edge: BackGestureEdge },
    /// The finger has travelled `progress` of the way to committing the
    /// gesture, in `0.0..=1.0`.
    Progressed {
        edge: BackGestureEdge,
        progress: f32,
    },
    /// The finger was lifted or reversed before the gesture committed, so the
    /// app's page must go back to where it was.
    Cancelled,
    /// The gesture committed, or the back button was pressed: the app's own
    /// back runs.
    Invoked,
}

/// The phases `GpuiHostActivity` numbers in its JNI report.
const PHASE_STARTED: i32 = 0;
const PHASE_PROGRESSED: i32 = 1;
const PHASE_CANCELLED: i32 = 2;
const PHASE_INVOKED: i32 = 3;

/// The edges `android.window.BackEvent` reports.
const EDGE_LEFT: i32 = 0;
const EDGE_RIGHT: i32 = 1;

fn event_sender() -> &'static Mutex<Option<UnboundedSender<BackGestureEvent>>> {
    static SENDER: OnceLock<Mutex<Option<UnboundedSender<BackGestureEvent>>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(None))
}

/// Publishes one phase of the system's back gesture.
pub fn notify(event: BackGestureEvent) {
    if let Ok(sender) = event_sender().lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.unbounded_send(event);
    }
}

/// Publishes a phase the Android host reported over JNI.
///
/// The host sends numbers rather than an enum so the JNI entry point carries
/// no Rust types: the meaning of each value is fixed by the constants above and
/// by the activity that sends them.
pub fn notify_from_host(phase: i32, edge: i32, progress: f32) {
    let edge = match edge {
        EDGE_LEFT => BackGestureEdge::Left,
        EDGE_RIGHT => BackGestureEdge::Right,
        _ => BackGestureEdge::Unknown,
    };
    let event = match phase {
        PHASE_STARTED => BackGestureEvent::Started { edge },
        PHASE_PROGRESSED => BackGestureEvent::Progressed {
            edge,
            // The platform reports the gesture's progress as a fraction, but
            // nothing in the contract promises its range: clamp it here rather
            // than letting a page's offset leave the screen.
            progress: progress.clamp(0.0, 1.0),
        },
        PHASE_CANCELLED => BackGestureEvent::Cancelled,
        PHASE_INVOKED => BackGestureEvent::Invoked,
        _ => return,
    };
    notify(event);
}

pub fn subscribe() -> UnboundedReceiver<BackGestureEvent> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut current) = event_sender().lock() {
        *current = Some(sender);
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One subscriber at a time, and the events it reads are the ones the host
    /// reported in order — a drawer that skipped a phase would either jump or
    /// never move at all.
    #[test]
    fn host_phases_arrive_in_order_with_their_edge() {
        let mut events = subscribe();
        notify_from_host(0, 0, 0.0);
        notify_from_host(1, 0, 0.25);
        notify_from_host(1, 0, 0.5);
        notify_from_host(3, 0, 1.0);

        let received: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(
            received,
            vec![
                BackGestureEvent::Started {
                    edge: BackGestureEdge::Left
                },
                BackGestureEvent::Progressed {
                    edge: BackGestureEdge::Left,
                    progress: 0.25
                },
                BackGestureEvent::Progressed {
                    edge: BackGestureEdge::Left,
                    progress: 0.5
                },
                BackGestureEvent::Invoked,
            ]
        );
    }

    #[test]
    fn unknown_edges_and_phases_never_reach_a_page() {
        let mut events = subscribe();
        // A version without the animation callback reports a bare invocation.
        notify_from_host(3, 2, 1.0);
        // A phase this build does not know is dropped rather than guessed at.
        notify_from_host(9, 0, 0.5);

        let received: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(received, vec![BackGestureEvent::Invoked]);
    }

    #[test]
    fn a_progress_outside_the_unit_range_is_clamped() {
        let mut events = subscribe();
        notify_from_host(1, 1, 4.5);
        notify_from_host(1, 1, -2.0);

        let received: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(
            received,
            vec![
                BackGestureEvent::Progressed {
                    edge: BackGestureEdge::Right,
                    progress: 1.0
                },
                BackGestureEvent::Progressed {
                    edge: BackGestureEdge::Right,
                    progress: 0.0
                },
            ]
        );
    }
}
