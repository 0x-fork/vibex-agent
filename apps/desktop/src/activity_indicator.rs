//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar walks one
//! around the Braille spinner's dot ring. Both use a synchronized, bounded clock
//! and a fixed, rem-sized canvas so motion never reflows adjacent text.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;
use vibex_tui::glyphs::{self, GlyphTier};

use crate::motion;

const TIMELINE_CYCLE: Duration = Duration::from_millis(2100);

/// Frames per second both marks are sampled at.
///
/// The sidebar mark used to step through the TUI's eight spinner frames at
/// their 100ms cadence. That cap never saved the workbench a frame — a running
/// row already carries the title `ShimmerText`, which asks for one every display
/// refresh — but it made the one moving thing in the row arrive on a timer, and
/// a `with_max_fps` re-render lands some time *after* the interval it waits for.
/// The phase read on arrival therefore skipped frames, so the rotation dropped
/// dots and popped. A continuous sweep sampled at the timeline mark's budget
/// costs the same frames the row was already spending and no longer steps.
const ACTIVITY_MAX_FPS: f32 = 30.0;

/// How long the TUI holds one of its spinner frames.
const SIDEBAR_FRAME_DURATION: Duration = Duration::from_millis(100);

/// One lap of the sidebar highlight: the time the TUI takes to walk its frames.
fn sidebar_cycle() -> Duration {
    SIDEBAR_FRAME_DURATION * glyphs::spinner_frames(GlyphTier::Full).len() as u32
}

// Normalized cell centers follow the V from its left tip to its right tip.
// These are glyph coordinates; the enclosing slot owns the interface scale.
const TIMELINE_CELLS: [(f32, f32); 7] = [
    (0.14, 0.20),
    (0.26, 0.40),
    (0.38, 0.60),
    (0.50, 0.80),
    (0.62, 0.60),
    (0.74, 0.40),
    (0.86, 0.20),
];

/// The painted sidebar mark's dots, as indexes into the Braille cell.
///
/// Dots 1-3 run down the left column and 4-6 back up the right one, so the six
/// form a closed ring around the cell — the path the TUI's frames carry their
/// lit window along. The mark walks that ring instead of replaying the frames:
/// the same rotation, with the light moving continuously instead of jumping
/// 100ms at a time.
const SIDEBAR_RING: [usize; 6] = [0, 1, 2, 5, 4, 3];

#[derive(Clone, Copy)]
enum ActivityStyle {
    Timeline,
    Sidebar,
}

#[derive(IntoElement)]
pub(crate) struct ActivityIndicator {
    id: ElementId,
    style: ActivityStyle,
    color: Option<Hsla>,
}

impl ActivityIndicator {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: ActivityStyle::Timeline,
            color: None,
        }
    }

    /// The TUI's spinner dot ring in a fixed sidebar status slot.
    /// The owning row scopes `id` to its session, group, or workspace.
    pub(crate) fn sidebar(id: impl Into<ElementId>) -> Self {
        Self {
            style: ActivityStyle::Sidebar,
            ..Self::new(id)
        }
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for ActivityIndicator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, style, color } = self;
        let (slot, default_color, cycle, max_fps) = match style {
            ActivityStyle::Timeline => (
                div().size_4(),
                cx.theme().foreground,
                TIMELINE_CYCLE,
                ACTIVITY_MAX_FPS,
            ),
            ActivityStyle::Sidebar => (
                div().size_5(),
                cx.theme().primary,
                sidebar_cycle(),
                ACTIVITY_MAX_FPS,
            ),
        };
        let slot = slot.flex_none();
        let color = color.unwrap_or(default_color);
        let render_mark = move |phase| match style {
            ActivityStyle::Timeline => timeline_mark(color, phase).into_any_element(),
            ActivityStyle::Sidebar => sidebar_mark(color, phase).into_any_element(),
        };

        if motion::reduced_motion(cx) || motion::pauses_while_inactive(!window.is_window_active()) {
            return slot.child(render_mark(None)).into_any_element();
        }

        slot.with_animation(
            id,
            Animation::new(cycle).repeat_synced().with_max_fps(max_fps),
            move |this, phase| this.child(render_mark(Some(phase))),
        )
        .into_any_element()
    }
}

fn timeline_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let head = phase.map(|phase| 0.5 - 0.5 * (phase * TAU).cos());
            let cell_size = bounds.size.width.min(bounds.size.height) * 0.15;
            for (ix, (x, y)) in TIMELINE_CELLS.into_iter().enumerate() {
                let intensity = head.map_or(1.0, |head| {
                    let position = ix as f32 / (TIMELINE_CELLS.len() - 1) as f32;
                    let distance = ((position - head).abs() / 0.42).min(1.0);
                    0.5 + 0.5 * (distance * PI).cos()
                });
                let origin = point(
                    bounds.origin.x + bounds.size.width * x - cell_size * 0.5,
                    bounds.origin.y + bounds.size.height * y - cell_size * 0.5,
                );
                window.paint_quad(
                    fill(
                        Bounds::new(origin, size(cell_size, cell_size)),
                        color.opacity(0.28 + 0.72 * intensity),
                    )
                    .corner_radii(cell_size * 0.22),
                );
            }
        },
    )
    .size_full()
}

/// How far apart two ring positions are, the short way around the loop.
fn ring_distance(index: f32, head: f32, lap: f32) -> f32 {
    let offset = (index - head).rem_euclid(lap);
    offset.min(lap - offset)
}

/// The sidebar mark: the spinner's dot ring with one highlight walking it.
///
/// `phase` is the fraction of a lap travelled by the head, or `None` for the
/// fully visible still mark reduced motion and the opt-in inactive-window pause
/// fall back to — the head parked at the lap's start, the pose gpui's own
/// reduced-motion handling leaves a repeating animation in.
fn sidebar_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    let head = phase.unwrap_or(0.0) * SIDEBAR_RING.len() as f32;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let extent = bounds.size.width.min(bounds.size.height);
            let center = bounds.center();
            let diameter = extent * 0.16;
            let pitch = extent * 0.26;
            let lap = SIDEBAR_RING.len() as f32;
            // The frame is painted, not shaped: the reader's code font is free
            // to lack the Braille block — JetBrains Mono, for one, draws its
            // replacement box for it — and then the running cue would be that
            // box instead of a spinner. Each dot sits at its Braille cell
            // center, so the ring renders in whatever font the workbench is set
            // to, and only its brightness carries the motion.
            for (index, dot) in SIDEBAR_RING.iter().copied().enumerate() {
                let (column, row) = braille_dot_cell(dot);
                // Brightness is the cosine of the dot's distance from the head
                // around the ring: the dot under the head is at full contrast,
                // the one opposite it is at rest, and every dot between rides a
                // smooth ramp. The same falloff the timeline sweep uses, wrapped
                // around a circle so one lap closes seamlessly.
                let distance = ring_distance(index as f32, head, lap);
                let intensity = 0.5 + 0.5 * (distance / lap * TAU).cos();
                let origin = point(
                    center.x + (column - 0.5) * pitch - diameter * 0.5,
                    center.y + (row - 1.5) * pitch - diameter * 0.5,
                );
                window.paint_quad(
                    fill(
                        Bounds::new(origin, size(diameter, diameter)),
                        color.opacity(0.28 + 0.72 * intensity),
                    )
                    .corner_radii(diameter * 0.5),
                );
            }
        },
    )
    .size_full()
}

/// The eight Braille dots of one spinner frame, as a bit mask.
///
/// The TUI's frames still name the dots the sidebar ring paints and the pace it
/// walks them; only the frame-by-frame replay is gone, so the read-back lives
/// with the tests that hold the two tables together.
#[cfg(test)]
fn braille_dots(frame: &str) -> u8 {
    frame
        .chars()
        .next()
        .and_then(|glyph| (glyph as u32).checked_sub('\u{2800}' as u32))
        .filter(|dots| *dots <= u8::MAX as u32)
        .map_or(0, |dots| dots as u8)
}

/// Where each Braille dot sits in its two-by-four cell.
///
/// Dots 1-3 run down the left column and 4-6 down the right one, then the
/// bottom row holds dots 7 and 8 — the arrangement a terminal cell uses, which
/// is what makes the painted mark read as the same spinner the TUI draws.
fn braille_dot_cell(index: usize) -> (f32, f32) {
    match index {
        0 => (0.0, 0.0),
        1 => (0.0, 1.0),
        2 => (0.0, 2.0),
        3 => (1.0, 0.0),
        4 => (1.0, 1.0),
        5 => (1.0, 2.0),
        6 => (0.0, 3.0),
        _ => (1.0, 3.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_frames_read_back_as_braille_dots() {
        let frames = glyphs::spinner_frames(GlyphTier::Full);
        assert!(!frames.is_empty());
        let mut masks = Vec::new();
        for frame in frames {
            let dots = braille_dots(frame);
            // Every frame of the working spinner is a partial cell, so an empty
            // mask would paint a mark that says nothing.
            assert_ne!(dots, 0, "{frame} carries no dots");
            assert!(!masks.contains(&dots), "{frame} repeats a frame");
            masks.push(dots);
        }
    }

    #[test]
    fn non_braille_frames_have_no_dots_to_paint() {
        assert_eq!(braille_dots("|"), 0);
        assert_eq!(braille_dots(""), 0);
    }

    /// The painted ring carries exactly the dots the TUI's spinner cycles, at
    /// the pace it cycles them.
    ///
    /// The frames are no longer replayed, so this test is what keeps the two
    /// surfaces the same cue: the desktop may not quietly drop a dot the spinner
    /// lights, paint one twice, or turn at a speed of its own.
    #[test]
    fn the_sidebar_ring_matches_the_tui_spinner() {
        let frames = glyphs::spinner_frames(GlyphTier::Full);
        let lit = frames
            .iter()
            .map(|frame| braille_dots(frame))
            .fold(0, |all, dots| all | dots);
        let ring = SIDEBAR_RING
            .iter()
            .map(|index| 1u8 << index)
            .fold(0, |all, dot| all | dot);
        assert_eq!(
            ring, lit,
            "the ring paints a different dot set than the spinner cycles"
        );
        assert_eq!(
            ring.count_ones() as usize,
            SIDEBAR_RING.len(),
            "a dot is painted twice"
        );
        assert_eq!(
            sidebar_cycle(),
            SIDEBAR_FRAME_DURATION * frames.len() as u32
        );
    }

    /// Consecutive ring positions are neighbours in the Braille cell.
    ///
    /// This is the "steps missing" regression. The highlight used to be a frame
    /// index, so a late re-render dropped whole frames and the light jumped
    /// across the cell; a head that walks neighbouring dots cannot jump.
    #[test]
    fn the_sidebar_ring_walks_neighbouring_dots_and_closes() {
        for step in 0..SIDEBAR_RING.len() {
            let (column, row) = braille_dot_cell(SIDEBAR_RING[step]);
            let next = SIDEBAR_RING[(step + 1) % SIDEBAR_RING.len()];
            let (next_column, next_row) = braille_dot_cell(next);
            let distance = (column - next_column).abs() + (row - next_row).abs();
            assert_eq!(distance, 1.0, "ring step {step} jumps");
        }
    }

    /// Every dot takes its turn at full brightness at the mark's real sample
    /// rate, and no frame-to-frame change is large enough to read as a pop.
    #[test]
    fn the_sidebar_highlight_lights_every_dot_smoothly() {
        let lap = SIDEBAR_RING.len() as f32;
        let brightness = |index: usize, phase: f32| {
            let distance = ring_distance(index as f32, phase * lap, lap);
            0.5 + 0.5 * (distance / lap * TAU).cos()
        };
        // One lap lasts `sidebar_cycle()` and the animation is sampled at
        // `ACTIVITY_MAX_FPS`, so this is the sequence a reader actually sees.
        let samples = (sidebar_cycle().as_secs_f32() * ACTIVITY_MAX_FPS).round() as usize;
        assert!(
            samples > glyphs::spinner_frames(GlyphTier::Full).len(),
            "the mark must be sampled faster than the frames it replaced"
        );
        let mut peaks = [0.0_f32; SIDEBAR_RING.len()];
        let mut previous = [0.0_f32; SIDEBAR_RING.len()];
        let mut worst_step = 0.0_f32;
        for sample in 0..=samples {
            let phase = sample as f32 / samples as f32;
            for index in 0..SIDEBAR_RING.len() {
                let value = brightness(index, phase);
                peaks[index] = peaks[index].max(value);
                if sample > 0 {
                    worst_step = worst_step.max((value - previous[index]).abs());
                }
                previous[index] = value;
            }
        }
        for (index, peak) in peaks.iter().enumerate() {
            assert!(*peak > 0.98, "dot {index} only reached {peak}");
        }
        // A step under a fifth of the ramp is a ramp, not a flicker.
        assert!(
            worst_step < 0.2,
            "brightness stepped {worst_step} in one frame"
        );
    }
}
