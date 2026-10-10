//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar paints
//! the TUI's Braille spinner frames dot by dot. Both use synchronized, bounded
//! clocks and fixed, rem-sized canvases so motion never reflows adjacent text.

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
const TIMELINE_MAX_FPS: f32 = 30.0;
const SIDEBAR_FRAME_DURATION: Duration = Duration::from_millis(100);

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

    /// The TUI's Braille spinner in a fixed sidebar status slot.
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
                TIMELINE_MAX_FPS,
            ),
            ActivityStyle::Sidebar => (
                div().size_5(),
                cx.theme().primary,
                SIDEBAR_FRAME_DURATION * glyphs::spinner_frames(GlyphTier::Full).len() as u32,
                1.0 / SIDEBAR_FRAME_DURATION.as_secs_f32(),
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

/// The eight Braille dots of one spinner frame, as a bit mask.
///
/// The TUI's frames are the single source of truth for the sidebar mark, so the
/// dots are read back out of the glyph rather than copied into a second table:
/// an edit to the frame list moves both surfaces together.
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

fn sidebar_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    let frames = glyphs::spinner_frames(GlyphTier::Full);
    let step = (phase.unwrap_or(0.0) * frames.len() as f32) as u32;
    let dots = braille_dots(glyphs::frame_at(frames, step, 1));

    // The frame is painted, not shaped: the reader's code font is free to lack
    // the Braille block — JetBrains Mono, for one, draws its replacement box for
    // it — and then the running cue would be that box instead of a spinner. Each
    // frame occupies the same centered cell and holds at full contrast until the
    // next discrete step.
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let extent = bounds.size.width.min(bounds.size.height);
            let center = bounds.center();
            let diameter = extent * 0.16;
            let pitch = extent * 0.26;
            for index in 0..8 {
                if dots & (1 << index) == 0 {
                    continue;
                }
                let (column, row) = braille_dot_cell(index);
                let origin = point(
                    center.x + (column - 0.5) * pitch - diameter * 0.5,
                    center.y + (row - 1.5) * pitch - diameter * 0.5,
                );
                window.paint_quad(
                    fill(Bounds::new(origin, size(diameter, diameter)), color)
                        .corner_radii(diameter * 0.5),
                );
            }
        },
    )
    .size_full()
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
}
