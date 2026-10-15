//! The sheets the phone pulls up from the foot of the screen.
//!
//! Every bottom sheet on the native client is the same object seen from a
//! different page: a scrim over what the reader was doing, a panel rising from
//! the bottom edge with a grabber on top, and a finger that can push it back
//! down. This module owns that object once — the rise, the fall, the scrim's
//! own fade, and the pull that follows the finger — so a sheet's own code is
//! only what it has to say.
//!
//! A page keeps its sheet's own flag as the source of truth for what the sheet
//! shows. Closing a sheet does not clear that flag: the flag stays set for as
//! long as the panel is on screen, the fall included, and the page clears it
//! when [`BottomSheet::land`] says the fall has landed. That is what lets a
//! sheet travel away with its own content still in it, instead of emptying out
//! and leaving a blank card to slide down.
//!
//! Each sheet carries its own motion, so a page can stack one sheet over
//! another — the drawer's row menu under its folder's archive window — without
//! the two fighting over one animation.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, IntoElement, MouseButton,
    MouseUpEvent, Pixels, ScrollDelta, ScrollHandle, ScrollWheelEvent, Styled as _, TouchPhase,
    Window, div, prelude::*, px,
};

use crate::theme;

/// Which sheet a rise or fall belongs to.
///
/// The key is what tells one panel from another: a page may have more than one
/// sheet on screen, and each of them owns its own motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetKey {
    /// The pairing fallbacks: finding a runtime, or typing a code.
    PairingFallback,
    /// The sidebar row's own commands.
    SidebarRowMenu,
    /// The sidebar's own commands, which belong to no row.
    SidebarRootMenu,
    /// The archive window a folder files its sessions on.
    FolderArchiveMenu,
    /// When a waiting send should leave.
    SidebarRescheduleMenu,
    /// The guard in front of deleting the selected sessions.
    BatchDelete,
    /// The authority's directories, so a worktree path can be chosen.
    WorktreePaths,
    /// The members of a session group.
    GroupMembers,
    /// The guard in front of removing a project.
    ProjectRemove,
    /// The objective a goal control will issue.
    GoalObjective,
    /// One runtime's own actions.
    HostActions,
    /// The prompts already sent in this session.
    ComposerHistory,
    /// What an attach control is about to attach.
    ComposerAttach,
    /// The project and workspace a new session starts in.
    NewSessionPicker,
    /// The new-session page's own settings.
    NewSessionOptions,
    /// The palette picker.
    SettingsPalette,
    /// Go to a line in the file on screen.
    FileGoto,
    /// The branch a history filter is scoped to.
    GitBranches,
    /// The author or date a history filter is scoped to.
    GitFilter,
    /// The guard in front of merging or discarding a worktree.
    WorktreeConfirmation,
    /// The guard in front of rolling changes back.
    GitRevert,
    /// The patch for one file, read over the tree it belongs to.
    GitDiff,
}

/// How tall a sheet may rise before its body scrolls instead.
///
/// A sheet never takes the whole page: whatever it explains stays in sight
/// behind the scrim, so the sheet reads as a thing laid over the page rather
/// than a page of its own. It is also the distance a panel travels, which is
/// why the panel is capped here rather than by each sheet for itself.
pub fn max_height(window: &Window) -> Pixels {
    px(f32::from(window.viewport_size().height) * theme::SHEET_HEIGHT_RATIO)
}

/// One page's bottom sheets: every panel on screen, its rise or fall, and
/// whatever the finger is doing to it.
#[derive(Default)]
pub struct BottomSheet {
    panels: Vec<Panel>,
}

/// One sheet on screen.
struct Panel {
    key: SheetKey,
    /// The rise or fall carrying the panel, once one has started. A landed
    /// motion stays as the record of where the panel rests.
    motion: Option<Motion>,
    /// Whether the page has asked the sheet to close.
    closing: bool,
    /// How far the panel may travel, read from the window on the last frame.
    travel: f32,
    /// The handle the sheet's own body scrolls with, once it has one. The pan
    /// reads it: a finger that drags down inside a body which still has
    /// somewhere to scroll belongs to that body, not to the sheet.
    body_scroll: Option<ScrollHandle>,
    /// The finger pulling the panel down, while it is pulling.
    pull: Option<Pull>,
    /// When the last pull let go, so the lift that ended it is not read as a
    /// tap on the scrim it landed on.
    released_at: Option<Instant>,
    /// The identity the next motion takes, so a new one restarts the animation.
    next_id: u64,
}

impl Panel {
    /// A panel that has just been opened: it comes up from the foot of the
    /// screen, wherever the last sheet left it.
    fn opening(key: SheetKey, travel: f32) -> Self {
        let mut panel = Self {
            key,
            motion: None,
            closing: false,
            travel,
            body_scroll: None,
            pull: None,
            released_at: None,
            next_id: 0,
        };
        panel.start(travel, 0.0, theme::SHEET_RISE_MS, Easing::Rise, false);
        panel
    }

    /// Where the panel is now: under a finger, that is where the finger put it;
    /// otherwise it is wherever the motion carrying it has got to.
    fn offset_now(&self) -> f32 {
        match self.claimed_pull() {
            Some(pull) => pull.offset,
            None => self
                .motion
                .map_or(0.0, |motion| motion.offset_at(motion.elapsed())),
        }
    }

    /// The finger's pull, if it has claimed the panel.
    fn claimed_pull(&self) -> Option<&Pull> {
        self.pull.as_ref().filter(|pull| pull.claimed)
    }

    /// How much of the motion on screen is left to run.
    fn remaining(&self) -> Duration {
        self.motion
            .map_or(Duration::ZERO, |motion| motion.remaining())
    }

    /// Starts a motion from wherever the panel is now.
    fn start(&mut self, from: f32, to: f32, base_ms: u64, easing: Easing, falling: bool) {
        self.next_id = self.next_id.wrapping_add(1);
        self.motion = Some(Motion {
            id: self.next_id,
            from,
            to,
            duration: flight(base_ms),
            easing,
            started: Instant::now(),
            falling,
        });
    }

    fn snapshot(&self) -> SheetPresentation {
        let offset = self.offset_now();
        SheetPresentation {
            offset,
            // A panel under a finger is placed by the finger, so the motion is
            // held at that offset rather than left to run. It is held rather
            // than dropped because the animation element carries the identity
            // of everything inside the panel: unmounting it mid-drag would
            // reset whatever the panel happens to be holding — a field's
            // cursor, a list's place.
            motion: match self.claimed_pull() {
                Some(_) => self.motion.map(|motion| motion.held_at(offset)),
                None => self.motion,
            },
            travel: self.travel,
        }
    }
}

/// What the shell draws for one frame of a sheet.
#[derive(Clone, Copy)]
pub struct SheetPresentation {
    /// Pixels the panel sits below its resting place: zero at rest, the whole
    /// travel once it has cleared the screen.
    offset: f32,
    /// The motion carrying the panel, when one is running.
    motion: Option<Motion>,
    /// How far the panel may travel.
    travel: f32,
}

impl SheetPresentation {
    /// The scrim's own strength: it fades with the panel it is behind, so a
    /// pull that is half way down is half way out of the page as well.
    fn scrim_opacity(&self) -> f32 {
        theme::SHEET_BACKDROP_OPACITY * reveal_of(self.offset, self.travel)
    }
}

/// The scrim's strength along a motion's own curve, as a value the animation
/// element can carry.
fn scrim_curve(motion: Option<Motion>, offset: f32, travel: f32) -> impl Fn(f32) -> f32 {
    move |delta| {
        let offset = match motion {
            Some(motion) => motion.offset_at_delta(delta),
            None => offset,
        };
        theme::SHEET_BACKDROP_OPACITY * reveal_of(offset, travel)
    }
}

/// What a page tells the sheet about the pan the platform reported.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SheetPan {
    /// A finger arrived, having already moved this far.
    Started { dy: f32 },
    /// A finger moved by this many pixels since the last report.
    Moved { dy: f32 },
    /// A finger let go.
    Ended,
    /// The platform took the pan away — a call, a system gesture.
    Cancelled,
}

/// What the sheet did with a pan, which is what the page has to act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetPull {
    /// The pan is not the sheet's: the lists under the finger keep it.
    Ignored,
    /// The panel is following the finger, so the pan must not reach anything
    /// else — the list inside the sheet would otherwise scroll with it.
    Pulling,
    /// The finger let go short of a dismissal: the panel is on its way back up.
    Restored,
    /// The finger let go far enough, or fast enough, that the sheet is leaving.
    Dismissed(SheetKey),
}

/// Reads one platform pan event.
///
/// Mobile platforms report a finger drag as a scroll wheel carrying a touch
/// phase; a mouse wheel carries none, and never pulls a sheet.
pub fn sheet_pan(event: &ScrollWheelEvent) -> Option<SheetPan> {
    let dy = |delta: ScrollDelta| match delta {
        ScrollDelta::Pixels(delta) => Some(f32::from(delta.y)),
        ScrollDelta::Lines(_) => None,
    };
    match event.touch_phase {
        TouchPhase::Started => Some(SheetPan::Started {
            dy: dy(event.delta)?,
        }),
        TouchPhase::Moved => Some(SheetPan::Moved {
            dy: dy(event.delta)?,
        }),
        TouchPhase::Ended => Some(SheetPan::Ended),
        TouchPhase::Cancelled => Some(SheetPan::Cancelled),
    }
}

impl BottomSheet {
    fn index_of(&self, key: SheetKey) -> Option<usize> {
        self.panels.iter().position(|panel| panel.key == key)
    }

    fn panel(&self, key: SheetKey) -> Option<&Panel> {
        self.panels.iter().find(|panel| panel.key == key)
    }

    fn panel_mut(&mut self, key: SheetKey) -> Option<&mut Panel> {
        self.panels.iter_mut().find(|panel| panel.key == key)
    }

    /// Whether this sheet is on its way out: the page has closed it, and the
    /// panel is still clearing the screen.
    pub fn is_leaving(&self, key: SheetKey) -> bool {
        self.panel(key).is_some_and(|panel| panel.closing)
    }

    /// Whether a pull of this sheet let go recently enough that the lift it
    /// ended must not be read as a tap on the scrim it landed on.
    pub fn pull_just_released(&self, key: SheetKey) -> bool {
        self.panel(key).is_some_and(|panel| {
            panel.released_at.is_some_and(|at| {
                at.elapsed() < Duration::from_millis(theme::SHEET_PULL_TAP_GUARD_MS)
            })
        })
    }

    /// Announces that the sheet is closing, and reports how long the panel
    /// takes to clear the screen.
    ///
    /// The page keeps its own flag set until [`BottomSheet::land`] says the
    /// fall has landed: that is what keeps the sheet's content in the panel
    /// while it slides away.
    pub fn lower(&mut self, key: SheetKey) -> Duration {
        let Some(panel) = self.panel_mut(key) else {
            // Nothing of this sheet is on screen, so there is nothing to
            // animate away and the page can clear its flag at once.
            return Duration::ZERO;
        };
        if panel.closing {
            // Already on its way out: a second request is answered with the
            // rest of the fall it is in.
            return panel.remaining();
        }
        panel.closing = true;
        panel.pull = None;
        panel.start(
            panel.offset_now(),
            panel.travel,
            theme::SHEET_FALL_MS,
            Easing::Fall,
            true,
        );
        panel.remaining()
    }

    /// Clears the sheet once its fall has landed, and reports whether it did.
    ///
    /// The second landing — a page that closed the same sheet twice, or a
    /// timer that fires after the flag was already dropped — has nothing to
    /// clear and answers `false`, so the page's own state is touched once.
    pub fn land(&mut self, key: SheetKey) -> bool {
        let Some(index) = self.index_of(key) else {
            return false;
        };
        if !self.panels[index].closing {
            return false;
        }
        self.panels.remove(index);
        true
    }

    /// What the shell should draw for `key` this frame, or nothing at all when
    /// the sheet is not on screen.
    ///
    /// `up` is the page's own flag. A flag that is cleared without a
    /// [`BottomSheet::lower`] — a page reset that drops everything at once —
    /// has no fall to draw, so the panel leaves on this frame.
    pub fn presentation(
        &mut self,
        key: SheetKey,
        up: bool,
        travel: Pixels,
    ) -> Option<SheetPresentation> {
        let travel = f32::from(travel);
        let Some(index) = self.index_of(key) else {
            if !up {
                return None;
            }
            // The first frame of a sheet the page has opened: it comes up
            // from the foot of the screen.
            self.panels.push(Panel::opening(key, travel));
            return self.panels.last().map(Panel::snapshot);
        };
        let dropped = {
            let panel = &mut self.panels[index];
            panel.travel = travel;
            // A page that drops its flag without closing the sheet has no fall
            // to draw, so the panel leaves on this frame.
            !up && !panel.closing
        };
        if dropped {
            self.panels.remove(index);
            return None;
        }
        Some(self.panels[index].snapshot())
    }

    /// The handle a sheet's own body scrolls with.
    ///
    /// The sheet tracks its body with it, and the pan reads it: a finger that
    /// drags down inside a body which still has somewhere to scroll belongs to
    /// that body, and only a body with nowhere left to go hands the pan to the
    /// sheet. A sheet whose body cannot scroll never asks for one, and one that
    /// is not on screen gets a handle nothing reads.
    pub fn body_scroll(&mut self, key: SheetKey) -> ScrollHandle {
        match self.panel_mut(key) {
            Some(panel) => panel
                .body_scroll
                .get_or_insert_with(ScrollHandle::new)
                .clone(),
            None => ScrollHandle::new(),
        }
    }

    /// Feeds one sheet one platform pan.
    pub fn pull(&mut self, key: SheetKey, pan: SheetPan, travel: Pixels) -> SheetPull {
        let travel = f32::from(travel);
        let Some(index) = self.index_of(key) else {
            return SheetPull::Ignored;
        };
        let panel = &mut self.panels[index];
        panel.travel = travel;
        if panel.closing {
            // A sheet that is already leaving is not caught again.
            return SheetPull::Ignored;
        }
        match pan {
            SheetPan::Started { dy } => {
                panel.pull = Some(Pull::started(Instant::now()));
                panel.advance(dy)
            }
            SheetPan::Moved { dy } => panel.advance(dy),
            SheetPan::Ended => panel.release(),
            SheetPan::Cancelled => panel.cancelled(),
        }
    }
}

#[cfg(test)]
impl Panel {
    /// Runs the panel's motion to its end, the way a frame some milliseconds
    /// later would have seen it.
    fn settle(&mut self) {
        if let Some(motion) = &mut self.motion {
            motion.from = motion.to;
            motion.started = Instant::now();
        }
    }
}

#[cfg(test)]
impl BottomSheet {
    /// Lets one sheet's motion finish before the test reads it.
    fn settle(&mut self, key: SheetKey) {
        if let Some(panel) = self.panel_mut(key) {
            panel.settle();
        }
    }

    /// Puts every panel at the end of its flight.
    ///
    /// A test's clock is not the clock the animation element reads, so a test
    /// that needs to see a settled panel says so instead of waiting.
    pub fn settle_flights(&mut self) {
        for panel in &mut self.panels {
            panel.settle();
        }
    }
}

impl Panel {
    /// Whether the body under the finger has nowhere left to scroll up, which
    /// is what lets the pan become the sheet's.
    fn body_at_top(&self) -> bool {
        self.body_scroll
            .as_ref()
            .is_none_or(|handle| handle.offset().y >= px(0.0))
    }

    /// Advances the pull by one translation, and reports who owns the pan now.
    fn advance(&mut self, dy: f32) -> SheetPull {
        let at_top = self.body_at_top();
        let Some(pull) = self.pull.as_mut() else {
            return SheetPull::Ignored;
        };
        pull.record(dy);
        if !pull.claimed {
            if pull.offset <= 0.0 || !at_top {
                // The finger is on its way up, or the body under it still has
                // somewhere to scroll: either way the pan is not the sheet's.
                return SheetPull::Ignored;
            }
            if pull.offset < theme::SHEET_PULL_THRESHOLD {
                return SheetPull::Ignored;
            }
            pull.claimed = true;
        }
        pull.offset = pull.offset.min(self.travel.max(1.0));
        SheetPull::Pulling
    }

    /// Lets the panel go where the finger left it.
    fn release(&mut self) -> SheetPull {
        self.released_at = Some(Instant::now());
        let Some(pull) = self.pull.take() else {
            return SheetPull::Ignored;
        };
        if !pull.claimed {
            return SheetPull::Ignored;
        }
        if pull_dismisses(pull.offset, pull.velocity) {
            self.closing = true;
            self.start(
                pull.offset,
                self.travel,
                theme::SHEET_FALL_MS,
                Easing::Fall,
                true,
            );
            SheetPull::Dismissed(self.key)
        } else {
            self.start(pull.offset, 0.0, theme::SHEET_RISE_MS, Easing::Rise, false);
            SheetPull::Restored
        }
    }

    /// Puts the panel back when the platform takes the gesture away.
    ///
    /// The reader did not finish the pull, so the sheet does not leave on it.
    fn cancelled(&mut self) -> SheetPull {
        self.released_at = Some(Instant::now());
        let Some(pull) = self.pull.take() else {
            return SheetPull::Ignored;
        };
        if !pull.claimed {
            return SheetPull::Ignored;
        }
        self.start(pull.offset, 0.0, theme::SHEET_RISE_MS, Easing::Rise, false);
        SheetPull::Restored
    }
}

/// Whether a released pull takes the sheet with it.
///
/// A pull that came far enough, or that was still moving fast enough when it
/// let go, dismisses the sheet; anything else goes back up. The distance is a
/// fixed one rather than a share of the sheet, because what the reader judges
/// it against is the rows that slid up, not the panel as a whole.
fn pull_dismisses(offset: f32, velocity: f32) -> bool {
    offset >= theme::SHEET_PULL_DISMISS_TRAVEL
        || (offset > 0.0 && velocity >= theme::SHEET_PULL_DISMISS_VELOCITY)
}

/// How long a held motion's animation is given: long enough that the element
/// never divides by zero, short enough that no curve is ever read from it.
const HELD_FLIGHT: Duration = Duration::from_millis(1);

/// The shortest gap between two pan reports that can say anything about how
/// fast the finger is moving.
const VELOCITY_SAMPLE_FLOOR: Duration = Duration::from_millis(4);

/// How long a sheet's motion runs, never zero: the animation element divides
/// by the duration, so a preference that asks for no motion is served by a
/// flight short enough to read as one frame.
fn flight(base_ms: u64) -> Duration {
    theme::motion_ms(base_ms).max(Duration::from_millis(1))
}

/// The share of a panel that is up, from its own offset.
fn reveal_of(offset: f32, travel: f32) -> f32 {
    if travel <= 0.0 {
        return 1.0;
    }
    (1.0 - offset / travel).clamp(0.0, 1.0)
}

/// A rise or a fall, and where it is.
#[derive(Clone, Copy, Debug)]
struct Motion {
    /// The identity the animation element keys on: a fresh one restarts it.
    id: u64,
    from: f32,
    to: f32,
    duration: Duration,
    easing: Easing,
    started: Instant,
    /// Whether this is the panel leaving.
    falling: bool,
}

impl Motion {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// How much of the flight is left, with a frame of slack so the last frame
    /// is painted before the page clears the sheet.
    fn remaining(&self) -> Duration {
        self.duration
            .saturating_sub(self.elapsed())
            .saturating_add(Duration::from_millis(16))
    }

    /// Where the panel sits when the animation element reports this much of the
    /// curve. The element hands over an eased value, so the interpolation here
    /// is the straight line between the two ends.
    fn offset_at_delta(&self, delta: f32) -> f32 {
        self.from + (self.to - self.from) * delta
    }

    /// Where the panel sits now, read from the clock rather than from the
    /// element: a motion that is interrupted has to know where it got to.
    fn offset_at(&self, elapsed: Duration) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let delta = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.offset_at_delta((self.easing.curve())(delta))
    }

    /// The same motion, held where it is: it renders that offset and no other.
    fn held_at(mut self, offset: f32) -> Self {
        self.from = offset;
        self.to = offset;
        self.duration = HELD_FLIGHT;
        self.started = Instant::now();
        self
    }

    fn animation(&self) -> Animation {
        Animation::new(self.duration).with_easing(self.easing.curve())
    }

    fn element_id(&self, stem: &str) -> ElementId {
        ElementId::NamedInteger(stem.into(), self.id)
    }
}

/// How a sheet travels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Easing {
    /// A panel coming up: it covers most of its travel at once and settles, so
    /// the rise reads as the sheet arriving rather than sliding.
    Rise,
    /// A panel going away: it leaves slowly and accelerates off the screen.
    Fall,
}

impl Easing {
    /// The curve, as a plain function: the shell hands it to the animation
    /// element, and the sheet reads the same curve to know where an interrupted
    /// panel got to. Both halves have to be the same shape for the two to
    /// agree — hence `ease_out_quint` written out, since it is needed as a
    /// value here and only as a closure in the toolkit.
    fn curve(self) -> fn(f32) -> f32 {
        match self {
            Easing::Rise => |delta| 1.0 - (1.0 - delta).powi(5),
            Easing::Fall => |delta| delta * delta * delta,
        }
    }
}

/// The finger's own pull, from the moment it starts to the moment it lets go.
#[derive(Clone, Copy, Debug)]
struct Pull {
    /// How far the finger has come down since the panel was still.
    offset: f32,
    /// The finger's speed, in pixels per second, as a moving average.
    velocity: f32,
    /// How many samples the average is built from.
    samples: u32,
    /// When the last sample arrived.
    sampled_at: Instant,
    /// Whether the panel is following this finger. Until it is, the pan belongs
    /// to whatever the finger is actually on.
    claimed: bool,
}

impl Pull {
    fn started(now: Instant) -> Self {
        Self {
            offset: 0.0,
            velocity: 0.0,
            samples: 0,
            sampled_at: now,
            claimed: false,
        }
    }

    /// Blends one more translation into the pull.
    ///
    /// Each platform report carries the translation since the last one, so the
    /// speed is folded in with a fixed blend rather than accumulated from raw
    /// distances: a fast flick right before the finger lifts still dominates
    /// the estimate, which is what tells a flick from a drag.
    fn record(&mut self, dy: f32) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.sampled_at);
        self.sampled_at = now;
        self.offset = (self.offset + dy).max(0.0);
        if elapsed < VELOCITY_SAMPLE_FLOOR {
            // Reports that arrive back to back describe coalesced input: the
            // gap between them is the platform's own jitter, and dividing by it
            // would report a speed no finger has.
            return;
        }
        let seconds = elapsed.as_secs_f32();
        let instant = dy / seconds;
        let blend = if self.samples == 0 { 1.0 } else { 0.55 };
        self.velocity += (instant - self.velocity) * blend;
        self.samples = self.samples.saturating_add(1);
    }
}

/// What a tap on a sheet's scrim does.
type DismissListener = Box<dyn Fn(&MouseUpEvent, &mut Window, &mut App)>;

/// What the platform's pan stream does while a finger is on a sheet's panel.
type PanListener = Box<dyn Fn(&ScrollWheelEvent, &mut Window, &mut App)>;

/// The shell every bottom sheet is drawn in: the scrim, the panel, and the
/// finger that pulls it.
pub struct BottomSheetShell {
    /// The element id stem, and the name a test finds the panel by.
    id: String,
    presentation: SheetPresentation,
    /// What a tap on the scrim does.
    dismiss: Option<DismissListener>,
    /// What the platform's pan stream does while the finger is on the panel.
    pan: Option<PanListener>,
    /// The sheet's own content, under the grabber.
    content: Option<AnyElement>,
}

/// Builds the shell for one frame of a sheet.
pub fn bottom_sheet_shell(
    id: impl Into<String>,
    presentation: SheetPresentation,
) -> BottomSheetShell {
    BottomSheetShell {
        id: id.into(),
        presentation,
        dismiss: None,
        pan: None,
        content: None,
    }
}

impl BottomSheetShell {
    /// What a tap outside the panel does.
    pub fn on_dismiss(
        mut self,
        dismiss: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.dismiss = Some(Box::new(dismiss));
        self
    }

    /// What the platform's pan stream does while the finger is on the panel.
    ///
    /// The listener is installed in the capture phase, ahead of the lists
    /// inside the sheet: a pull the sheet claims has to reach it before the
    /// body under the finger has scrolled with the same gesture.
    pub fn on_pan(
        mut self,
        pan: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.pan = Some(Box::new(pan));
        self
    }

    /// The sheet's own content: its header and its body, under the grabber.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// Puts the shell together.
    fn compose(mut self) -> AnyElement {
        let presentation = self.presentation;
        let travel = presentation.travel;
        let offset = presentation.offset;
        let motion = presentation.motion;
        let content = self.content.take();

        // The panel's own name is the sheet's, so the bounds a test measures
        // are the whole surface the reader sees — grabber, header and all.
        let panel = div()
            .debug_selector({
                let selector = self.id.clone();
                move || selector.clone()
            })
            .w_full()
            .max_h(px(travel))
            .flex()
            .flex_col()
            .rounded_t(px(theme::RADIUS_SHEET))
            .border_t_1()
            .border_color(theme::border_default())
            .bg(theme::bg_card())
            .overflow_hidden()
            // The panel owns its own taps: only the scrim around it dismisses
            // what it is showing.
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(sheet_grabber())
            // The content takes what is left of the panel and no more, so a
            // long list inside it scrolls instead of growing the panel past the
            // height a sheet is allowed.
            .child(div().flex_1().min_h_0().flex().flex_col().children(content));

        let positioner = div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .bottom(px(-offset));
        let positioner: AnyElement = match motion {
            Some(motion) => positioner
                .with_animation(
                    motion.element_id(&format!("{}-panel", self.id)),
                    motion.animation(),
                    move |element, delta| element.bottom(px(-motion.offset_at_delta(delta))),
                )
                .child(panel)
                .into_any_element(),
            None => positioner
                .bottom(px(-offset))
                .child(panel)
                .into_any_element(),
        };
        let positioner: AnyElement = match self.pan.take() {
            Some(pan) => crate::scroll_capture::capture_scroll_wheel(
                ElementId::Name(format!("{}-pull", self.id).into()),
                positioner,
                pan,
            )
            .into_any_element(),
            None => positioner,
        };

        let mut scrim = div()
            .id(ElementId::Name(format!("{}-scrim", self.id).into()))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .flex_col()
            .justify_end();
        if let Some(dismiss) = self.dismiss.take() {
            scrim = scrim.on_mouse_up(MouseButton::Left, dismiss);
        }
        match motion {
            Some(motion) => {
                let opacity = scrim_curve(Some(motion), offset, travel);
                scrim
                    .with_animation(
                        motion.element_id(&format!("{}-scrim", self.id)),
                        motion.animation(),
                        move |element, delta| element.bg(theme::backdrop(opacity(delta))),
                    )
                    .child(positioner)
                    .into_any_element()
            }
            None => scrim
                .bg(theme::backdrop(presentation.scrim_opacity()))
                .child(positioner)
                .into_any_element(),
        }
    }
}

impl IntoElement for BottomSheetShell {
    type Element = AnyElement;

    fn into_element(self) -> Self::Element {
        self.compose()
    }
}

/// The bar at the top of every sheet.
///
/// It is a control, not a border: it is the one mark that says the panel can be
/// pulled back down, so it is drawn in the muted foreground rather than the
/// panel's own edge, which is a tenth of a black and disappears against the
/// surface it is drawn on.
fn sheet_grabber() -> gpui::Div {
    div()
        .flex_shrink_0()
        .w_full()
        .pt(px(theme::SPACING_SM))
        .pb(px(theme::SPACING_XS))
        .flex()
        .justify_center()
        .child(
            div()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .rounded_full()
                .bg(theme::sheet_grabber()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// The shell is the one object every bottom sheet wears, so what it owes a
    /// sheet is checked here rather than once per page: the surface, the scrim
    /// under it, the grabber on top, and the two gestures.
    #[test]
    fn the_shell_keeps_the_surface_every_sheet_wears() {
        let source = include_str!("bottom_sheet.rs");
        let (_, shell) = source
            .split_once("    fn compose(mut self) -> AnyElement {")
            .expect("the shell's own composition should remain inspectable");
        for shared in [
            ".absolute()",
            ".occlude()",
            ".justify_end()",
            "max_h(px(travel))",
            "RADIUS_SHEET",
            "sheet_grabber()",
            "capture_scroll_wheel(",
            "on_mouse_up(MouseButton::Left, dismiss)",
        ] {
            assert!(shell.contains(shared), "the shell should keep {shared}");
        }
    }

    const TRAVEL: f32 = 600.0;
    const TRAVEL_PX: Pixels = px(TRAVEL);

    fn up(sheet: &mut BottomSheet, key: SheetKey) -> SheetPresentation {
        sheet
            .presentation(key, true, TRAVEL_PX)
            .expect("the sheet should be on screen")
    }

    /// Two offsets are the same place within a pixel: a panel starts its
    /// flight a few microseconds before the test gets to read it.
    fn assert_place(actual: f32, expected: f32, context: &str) {
        assert!(
            (actual - expected).abs() < 1.0,
            "{context}: {actual} is not {expected}"
        );
    }

    /// A sheet that is up, with its rise already run: where the reader sees it
    /// once the panel has arrived.
    fn settled(sheet: &mut BottomSheet, key: SheetKey) -> SheetPresentation {
        // Opening it first is what puts the panel on screen; settling is what
        // lets its rise finish.
        up(sheet, key);
        sheet.settle(key);
        up(sheet, key)
    }

    /// A sheet is only drawn while its page says it is up, and a page that
    /// clears the flag without closing the sheet takes it off screen at once.
    #[test]
    fn a_sheet_is_only_drawn_while_its_page_says_it_is_up() {
        let mut sheet = BottomSheet::default();
        assert!(
            sheet
                .presentation(SheetKey::PairingFallback, false, TRAVEL_PX)
                .is_none(),
            "a sheet nobody opened is not drawn"
        );

        assert_place(
            up(&mut sheet, SheetKey::PairingFallback).offset,
            TRAVEL,
            "a sheet that is opened rises from the foot",
        );
        assert_place(
            settled(&mut sheet, SheetKey::PairingFallback).offset,
            0.0,
            "the sheet settles where it rests",
        );
        assert!(
            sheet
                .presentation(SheetKey::GitBranches, false, TRAVEL_PX)
                .is_none(),
            "another page's sheet is not this one's to draw"
        );
        assert!(
            sheet
                .presentation(SheetKey::PairingFallback, false, TRAVEL_PX)
                .is_none(),
            "a reset that drops the flag leaves no fall behind it"
        );
    }

    /// A closing sheet keeps its flag set through the fall, and lands where the
    /// page can clear it.
    #[test]
    fn a_closing_sheet_lands_only_when_its_fall_is_done() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::SidebarRowMenu);
        let flight = sheet.lower(SheetKey::SidebarRowMenu);
        assert!(flight > Duration::ZERO, "the fall has to take some time");

        // Still drawn while it falls, even though the page is closing it.
        let falling = up(&mut sheet, SheetKey::SidebarRowMenu);
        assert!(falling.offset > 0.0, "the panel is on its way down");
        assert!(sheet.is_leaving(SheetKey::SidebarRowMenu));
        assert!(!sheet.land(SheetKey::PairingFallback));

        assert!(sheet.land(SheetKey::SidebarRowMenu));
        assert!(
            sheet
                .presentation(SheetKey::SidebarRowMenu, false, TRAVEL_PX)
                .is_none(),
            "a landed sheet leaves the screen"
        );
    }

    /// Two sheets of one page carry their own motion, so the one underneath
    /// keeps its place while the one on top rises over it.
    #[test]
    fn a_stacked_sheet_rises_without_moving_the_one_under_it() {
        let mut sheet = BottomSheet::default();
        let resting = settled(&mut sheet, SheetKey::SidebarRowMenu);
        assert_place(resting.offset, 0.0, "the first sheet has settled");

        let stacked = up(&mut sheet, SheetKey::FolderArchiveMenu);
        assert_place(
            stacked.offset,
            TRAVEL,
            "the sheet on top rises from the foot",
        );
        assert_place(
            settled(&mut sheet, SheetKey::SidebarRowMenu).offset,
            0.0,
            "the sheet underneath is where it was",
        );

        // Closing the one on top leaves the one underneath alone.
        sheet.lower(SheetKey::FolderArchiveMenu);
        sheet.land(SheetKey::FolderArchiveMenu);
        assert_place(
            settled(&mut sheet, SheetKey::SidebarRowMenu).offset,
            0.0,
            "the sheet underneath never moved",
        );
    }

    /// The scrim follows the panel: a pull that is half way down has taken half
    /// the dimming with it.
    #[test]
    fn the_scrim_fades_with_the_panel_it_is_behind() {
        let mut sheet = BottomSheet::default();
        let opened = settled(&mut sheet, SheetKey::GitFilter);
        assert!(
            (opened.scrim_opacity() - theme::SHEET_BACKDROP_OPACITY).abs() < 0.01,
            "a sheet at rest dims the page by its own scrim"
        );

        let half = SheetPresentation {
            offset: TRAVEL / 2.0,
            motion: None,
            travel: TRAVEL,
        };
        assert!(half.scrim_opacity() > 0.0);
        assert!(half.scrim_opacity() < theme::SHEET_BACKDROP_OPACITY);
        assert_eq!(
            reveal_of(TRAVEL, TRAVEL),
            0.0,
            "a panel that has cleared the screen leaves no scrim"
        );
    }

    /// A drag that has not come far enough is still a tap, and a drag inside a
    /// body that can scroll belongs to that body.
    #[test]
    fn a_pull_is_only_claimed_once_the_finger_means_it() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::GitBranches);
        let body = sheet.body_scroll(SheetKey::GitBranches);

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dy: 2.0 },
                TRAVEL_PX
            ),
            SheetPull::Ignored,
            "a tap that wobbles is a tap"
        );

        // A body with room left to scroll keeps the pan for itself.
        body.set_offset(point(px(0.0), px(-240.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started {
                    dy: theme::SHEET_PULL_THRESHOLD
                },
                TRAVEL_PX
            ),
            SheetPull::Ignored,
            "a body that can still scroll up owns the pan"
        );

        // The same finger on a body that is already at its top pulls the sheet.
        body.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved {
                    dy: theme::SHEET_PULL_THRESHOLD
                },
                TRAVEL_PX
            ),
            SheetPull::Pulling
        );
        // Once the panel is following the finger it keeps it, even if the body
        // would have had somewhere to scroll.
        body.set_offset(point(px(0.0), px(-240.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved { dy: 10.0 },
                TRAVEL_PX
            ),
            SheetPull::Pulling
        );
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, TRAVEL_PX),
            SheetPull::Restored
        );
    }

    /// A sheet whose body cannot scroll — a menu of three rows — never keeps a
    /// downward pull for itself.
    #[test]
    fn a_body_that_cannot_scroll_hands_every_pull_to_the_sheet() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::ComposerAttach);
        assert_eq!(
            sheet.pull(
                SheetKey::ComposerAttach,
                SheetPan::Started { dy: 40.0 },
                TRAVEL_PX
            ),
            SheetPull::Pulling
        );
    }

    /// A panel that a finger owns is where the finger put it: the shell is
    /// handed the pull's own offset, and the motion is held there so the
    /// animation element cannot paint its last frame over the finger.
    #[test]
    fn a_pulled_panel_follows_the_finger() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dy: 40.0 },
                TRAVEL_PX
            ),
            SheetPull::Pulling
        );
        let pulled = sheet
            .presentation(SheetKey::GitBranches, true, TRAVEL_PX)
            .expect("a pulled sheet is on screen");
        assert!(
            (pulled.offset - 40.0).abs() < 1.0,
            "the panel should be where the finger is: {}",
            pulled.offset
        );
        let held = pulled.motion.expect("a held panel keeps its motion");
        assert_eq!(held.from, held.to, "nothing should be animating it");
        assert!(pulled.scrim_opacity() < theme::SHEET_BACKDROP_OPACITY);

        // And it keeps following the finger, rather than only the first sample.
        sheet.pull(
            SheetKey::GitBranches,
            SheetPan::Moved { dy: 20.0 },
            TRAVEL_PX,
        );
        let further = sheet
            .presentation(SheetKey::GitBranches, true, TRAVEL_PX)
            .expect("a pulled sheet is on screen");
        assert!(
            (further.offset - 60.0).abs() < 1.0,
            "the panel should still be under the finger: {}",
            further.offset
        );

        // Letting go hands the panel back to a curve, from where the finger
        // left it.
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, TRAVEL_PX),
            SheetPull::Restored
        );
        let released = sheet
            .presentation(SheetKey::GitBranches, true, TRAVEL_PX)
            .expect("a released sheet is on screen");
        let motion = released.motion.expect("a released panel carries a motion");
        assert!(motion.from > motion.to, "the panel is going back up");
        assert!(
            (motion.from - 60.0).abs() < 1.0,
            "the release starts where the finger left the panel: {}",
            motion.from
        );
    }

    /// A pull that comes far enough takes the sheet with it, and the page is
    /// told which sheet is leaving.
    #[test]
    fn a_long_pull_dismisses_the_sheet() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::NewSessionPicker);
        assert_eq!(
            sheet.pull(
                SheetKey::NewSessionPicker,
                SheetPan::Started { dy: 120.0 },
                TRAVEL_PX
            ),
            SheetPull::Pulling
        );
        assert_eq!(
            sheet.pull(SheetKey::NewSessionPicker, SheetPan::Ended, TRAVEL_PX),
            SheetPull::Dismissed(SheetKey::NewSessionPicker)
        );
        assert!(sheet.is_leaving(SheetKey::NewSessionPicker));
    }

    /// A pan that belongs to no sheet on screen is left to the page.
    #[test]
    fn a_pan_on_a_page_with_no_sheet_is_ignored() {
        let mut sheet = BottomSheet::default();
        assert_eq!(
            sheet.pull(
                SheetKey::FileGoto,
                SheetPan::Started { dy: 40.0 },
                TRAVEL_PX
            ),
            SheetPull::Ignored
        );
        assert_eq!(
            sheet.pull(SheetKey::FileGoto, SheetPan::Ended, TRAVEL_PX),
            SheetPull::Ignored
        );
    }

    /// A gesture the platform takes away puts the panel back: the reader never
    /// finished the pull, so the sheet does not leave on it.
    #[test]
    fn a_cancelled_pull_goes_back_up() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::WorktreePaths);
        sheet.pull(
            SheetKey::WorktreePaths,
            SheetPan::Started { dy: 120.0 },
            TRAVEL_PX,
        );
        assert_eq!(
            sheet.pull(SheetKey::WorktreePaths, SheetPan::Cancelled, TRAVEL_PX),
            SheetPull::Restored
        );
        assert!(!sheet.is_leaving(SheetKey::WorktreePaths));
    }

    /// What the reader judges a dismissal against is how far the panel came,
    /// not how fast the platform happened to report it.
    #[test]
    fn a_dismissal_is_a_distance_or_a_flick() {
        assert!(!pull_dismisses(0.0, 0.0));
        assert!(!pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL - 1.0, 0.0));
        assert!(pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL, 0.0));
        assert!(pull_dismisses(
            theme::SHEET_PULL_DISMISS_TRAVEL / 4.0,
            theme::SHEET_PULL_DISMISS_VELOCITY
        ));
        assert!(
            !pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL / 4.0, 0.0),
            "a slow short pull goes back up"
        );
    }

    /// A panel that has just opened is at the foot of the screen, and a pull
    /// that is let go short of a dismissal brings it back to its rest.
    #[test]
    fn a_panel_rises_from_the_foot_and_settles_at_its_rest() {
        let mut sheet = BottomSheet::default();
        let rising = up(&mut sheet, SheetKey::SettingsPalette);
        assert_place(rising.offset, TRAVEL, "a fresh panel starts below the edge");
        assert!(rising.motion.is_some_and(|motion| !motion.falling));

        // A motion that is given time reaches its end.
        let motion = rising.motion.expect("a rising panel carries a motion");
        assert_eq!(motion.offset_at(motion.duration), 0.0);
        assert_eq!(motion.offset_at(Duration::ZERO), TRAVEL);
    }
}
