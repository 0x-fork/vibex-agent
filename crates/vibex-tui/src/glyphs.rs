//! The chrome glyph vocabulary.
//!
//! Every glyph the interface draws for structure comes from here, for two
//! reasons: one place to change a symbol, and one place to decide what a
//! terminal can actually render.
//!
//! Terminals disagree about far more than colour. The conservative Windows
//! console does no font fallback at all, so a glyph outside its raster font
//! renders as tofu; a terminal in a non-UTF-8 locale cannot encode box drawing
//! either. Each entry therefore declares its fallback and the width invariant it
//! must keep, because a glyph that changes width shifts everything after it and
//! turns a cosmetic fallback into a layout bug.
//!
//! Widths are part of the contract: [`PROMPT_ARROW`] is always two columns, the
//! spinners are always one. That is what lets the renderer reserve a column
//! without asking which glyph was chosen.

use crate::theme::{GlyphMode, TuiTheme};

/// How much of the vocabulary the terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphTier {
    /// Geometric shapes, box drawing and arrows are all available.
    Full,
    /// Only characters a legacy console font has: ASCII and CP437.
    Legacy,
}

impl GlyphTier {
    pub const fn of(theme: &TuiTheme) -> Self {
        match theme.glyphs() {
            GlyphMode::Unicode => Self::Full,
            GlyphMode::Ascii => Self::Legacy,
        }
    }
}

/// The prompt's leading arrow. Always two columns.
pub fn prompt_arrow(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "❯ ",
        GlyphTier::Legacy => "> ",
    }
}

/// Width of [`prompt_arrow`] in columns.
pub const PROMPT_ARROW_WIDTH: usize = 2;

/// The heavy vertical used for an accent rail drawn as a line. One column.
pub fn accent_bar(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "┃",
        GlyphTier::Legacy => "|",
    }
}

/// A thin connector between a disclosure's children. Always one column.
pub fn connector(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "│",
        GlyphTier::Legacy => "|",
    }
}

/// Failure and close markers. One column.
pub fn ballot_x(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "✗",
        GlyphTier::Legacy => "x",
    }
}

/// Completion markers. One column.
pub fn check_mark(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "✓",
        GlyphTier::Legacy => "v",
    }
}

/// A filled diamond, used for the current item and for done steps. One column.
pub fn diamond_filled(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◆",
        GlyphTier::Legacy => "*",
    }
}

/// A hollow diamond, used for idle markers. One column.
pub fn diamond_hollow(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◇",
        GlyphTier::Legacy => "o",
    }
}

/// A dotted diamond, used for queued markers. One column.
pub fn diamond_dotted(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◈",
        GlyphTier::Legacy => "+",
    }
}

/// Disclosure markers for a collapsible region. One column.
pub fn disclosure(open: bool, tier: GlyphTier) -> &'static str {
    match (open, tier) {
        (true, GlyphTier::Full) => "▾",
        (false, GlyphTier::Full) => "▸",
        (true, GlyphTier::Legacy) => "v",
        (false, GlyphTier::Legacy) => ">",
    }
}

/// The marker beside a row the reader has pinned. One column.
///
/// A star rather than a flag: this column already carries the Agent's letter in
/// the next cell, and a flag beside a letter read as two pieces of the same
/// word. The star is the shape every list uses for "kept", it is one cell in
/// the same width class as the other marks, and at the legacy tier it degrades
/// to the ASCII star.
pub fn pin_marker(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "★",
        GlyphTier::Legacy => "*",
    }
}

/// A session with something new in it. One column.
///
/// Distinct from the pin in shape rather than only in colour: the two sit in
/// the same column, and a reader who cannot tell the colours apart still has to
/// tell a kept session from an unread one.
pub fn unread_marker(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "●",
        GlyphTier::Legacy => "o",
    }
}

/// The mark beside a session that is waiting for a scheduled send. One column.
///
/// A clock face rather than the arrow auto-continue wears: both marks mean
/// "something will happen here without you", and a reader has to be able to
/// tell a message that is already written from a turn that will be started
/// again. The legacy tier degrades to the ASCII clock every terminal has.
pub fn schedule_marker(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◷",
        GlyphTier::Legacy => "@",
    }
}

/// The mark beside a session's state. One column.
///
/// Colour carries the same distinction, but the shape does not depend on it: a
/// failed session and a running one are told apart on a monochrome terminal.
pub fn state_marker(state: &str, tier: GlyphTier) -> &'static str {
    let unicode = tier == GlyphTier::Full;
    match state {
        "running" => {
            if unicode {
                "▶"
            } else {
                ">"
            }
        }
        "failed" => {
            if unicode {
                "✗"
            } else {
                "x"
            }
        }
        "waiting" => {
            if unicode {
                "◆"
            } else {
                "*"
            }
        }
        "archived" => {
            if unicode {
                "▤"
            } else {
                "#"
            }
        }
        _ => {
            if unicode {
                "·"
            } else {
                "."
            }
        }
    }
}

/// The rule a section heading trails to the edge of its list. One column.
///
/// It is a box-drawing character, so a legacy console gets the hyphen rather
/// than a blank: the rule is what turns a stack of rows into sections, and a
/// heading with no rule is one more line of text.
pub fn heading_rule(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "─",
        GlyphTier::Legacy => "-",
    }
}

/// The dot between two facts written on one row. One column.
///
/// A hyphen rather than a middle dot on a console that lacks one: the row it
/// separates is a caption, and a caption is read as a sentence.
pub fn separator(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "·",
        GlyphTier::Legacy => "-",
    }
}

/// The multiplication sign a pair of dimensions is written with. One column.
pub fn times(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "×",
        GlyphTier::Legacy => "x",
    }
}

/// The mark that stands for the lines a preview left out. One column.
pub fn elision(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "⋮",
        GlyphTier::Legacy => ":",
    }
}

/// The upper half block a picture is drawn with, two pixels to a cell.
///
/// A console without it gets a solid mark and one colour per cell: a coarse
/// picture rather than no picture, which is the same trade every other fallback
/// in this module makes.
pub fn half_block(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "▀",
        GlyphTier::Legacy => "#",
    }
}

/// A chevron pointing at what is not shown yet. One column.
///
/// It survives the turn rail it was written for, which no longer jumps a turn
/// at a time: the dock's "and N more" row points down with it.
pub fn chevron(up: bool, tier: GlyphTier) -> &'static str {
    match (up, tier) {
        (true, GlyphTier::Full) => "▴",
        (false, GlyphTier::Full) => "▾",
        (true, GlyphTier::Legacy) => "^",
        (false, GlyphTier::Legacy) => "v",
    }
}

/// The turn marker on the timeline rail. One column.
pub fn timeline_tick(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "•",
        GlyphTier::Legacy => ".",
    }
}

/// The heavier tick marking the turn the viewport starts on. One column.
pub fn timeline_tick_active(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "▪",
        GlyphTier::Legacy => "#",
    }
}

/// Braille spinner frames, one column each.
///
/// Braille is absent from a legacy console font, so the fallback is the classic
/// four-step ASCII spinner rather than a substitute shape.
pub fn spinner_frames(tier: GlyphTier) -> &'static [&'static str] {
    const FULL: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    const LEGACY: &[&str] = &["|", "/", "-", "\\"];
    match tier {
        GlyphTier::Full => FULL,
        GlyphTier::Legacy => LEGACY,
    }
}

/// The dots of the activity ring, as indexes into the Braille cell.
///
/// The order is the walk: down the left column (dots 1-3) and back up the right
/// one (dots 6-4), so consecutive entries are neighbours in the cell and the
/// last closes onto the first. The desktop's sidebar paints a highlight around
/// this ring; the turn band steps the same ring on the character grid. One
/// table rather than two, so the surfaces cannot drift apart by a dot.
pub const ACTIVITY_RING: [usize; 6] = [0, 1, 2, 5, 4, 3];

/// The activity ring's frames: the highlight, one step per frame. One column
/// each.
///
/// A cell cannot paint one dot brighter than its neighbours, so the highlight
/// is the *window* of dots a frame lights — the ring position it is on, the one
/// it came from and the one it is going to — and each frame is the Braille cell
/// that lights those three. A console with no Braille falls back to the ASCII
/// rotation [`spinner_frames`] uses, which is the same cue in the only alphabet
/// it has.
pub fn activity_ring_frames(tier: GlyphTier) -> &'static [&'static str] {
    const FULL: &[&str] = &["⠋", "⠇", "⠦", "⠴", "⠸", "⠙"];
    const LEGACY: &[&str] = &["|", "/", "-", "\\"];
    match tier {
        GlyphTier::Full => FULL,
        GlyphTier::Legacy => LEGACY,
    }
}

/// How many ticks of the animation clock each activity-ring frame is held.
///
/// The working spinner holds a frame for three ticks because a shape that
/// changes every 120ms is a flicker. The ring is not that shape: it is the
/// desktop sidebar's mark, and one step per tick is what makes it read as a
/// light travelling around the ring rather than as a glyph being replaced. The
/// desktop sweeps a lap in its working spinner's 8 × 100ms; this client's 120ms
/// clock takes the same six steps in 720ms.
pub const ACTIVITY_RING_TICKS_PER_FRAME: u32 = 1;

/// The idle pulse: a ring that fills and empties. One column each.
///
/// Used for a session that is alive but not working, so the interface still
/// reads as connected without claiming activity that is not happening.
pub fn idle_pulse_frames(tier: GlyphTier) -> &'static [&'static str] {
    const FULL: &[&str] = &["○", "◎", "◉", "◎"];
    const LEGACY: &[&str] = &["o", "O", "0", "O"];
    match tier {
        GlyphTier::Full => FULL,
        GlyphTier::Legacy => LEGACY,
    }
}

/// How many ticks of the animation clock each spinner frame is held.
///
/// A spinner that changes every frame at 120ms is a flicker; holding each frame
/// for a few ticks reads as rotation.
pub const SPINNER_TICKS_PER_FRAME: u32 = 3;

/// How many ticks each idle-pulse frame is held.
///
/// The idle pulse is deliberately slower than the working spinner: it says
/// "alive", not "busy".
pub const IDLE_PULSE_TICKS_PER_FRAME: u32 = 6;

/// Pick a frame from a cycle held for `ticks_per_frame`.
pub fn frame_at<'a>(frames: &'a [&'a str], phase: u32, ticks_per_frame: u32) -> &'a str {
    if frames.is_empty() {
        return " ";
    }
    let index = (phase / ticks_per_frame.max(1)) as usize % frames.len();
    frames[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorCapability, ColorMode, TuiTheme};
    use vibex_ui::GpuiThemeMode;

    fn theme(glyphs: GlyphMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs,
            },
        )
    }

    fn full() -> GlyphTier {
        GlyphTier::of(&theme(GlyphMode::Unicode))
    }

    fn legacy() -> GlyphTier {
        GlyphTier::of(&theme(GlyphMode::Ascii))
    }

    #[test]
    fn the_prompt_arrow_is_always_two_columns() {
        // The renderer reserves this many columns without asking which glyph
        // was chosen, so a fallback that changed width would shift the layout.
        for tier in [full(), legacy()] {
            assert_eq!(
                crate::text::display_width(prompt_arrow(tier)),
                PROMPT_ARROW_WIDTH
            );
        }
    }

    #[test]
    fn single_column_glyphs_stay_single_column() {
        for tier in [full(), legacy()] {
            for glyph in [
                accent_bar(tier),
                connector(tier),
                ballot_x(tier),
                check_mark(tier),
                diamond_filled(tier),
                diamond_hollow(tier),
                diamond_dotted(tier),
                disclosure(true, tier),
                disclosure(false, tier),
                chevron(true, tier),
                chevron(false, tier),
                timeline_tick(tier),
                timeline_tick_active(tier),
                heading_rule(tier),
                schedule_marker(tier),
            ] {
                assert_eq!(
                    crate::text::display_width(glyph),
                    1,
                    "{glyph:?} is not one column"
                );
            }
        }
    }

    #[test]
    fn every_spinner_frame_is_one_column_in_both_tiers() {
        // A spinner frame wider than its neighbours would jitter the label.
        for tier in [full(), legacy()] {
            for frame in spinner_frames(tier)
                .iter()
                .chain(activity_ring_frames(tier))
                .chain(idle_pulse_frames(tier))
            {
                assert_eq!(
                    crate::text::display_width(frame),
                    1,
                    "{frame:?} is not one column"
                );
            }
        }
    }

    /// The dot mask a frame lights, as a bit per Braille dot.
    fn dots(frame: &str) -> u8 {
        frame
            .chars()
            .next()
            .and_then(|glyph| (glyph as u32).checked_sub('\u{2800}' as u32))
            .filter(|dots| *dots <= u32::from(u8::MAX))
            .map_or(0, |dots| dots as u8)
    }

    /// Every ring frame lights the window around one ring position, and the
    /// frames step the ring in order.
    ///
    /// This is the whole contract with the desktop's sidebar: it paints a
    /// highlight walking [`ACTIVITY_RING`], and a terminal cell can only draw
    /// which dots the highlight covers — so a frame that lit some other set
    /// would be a different mark on the two surfaces.
    #[test]
    fn the_activity_ring_frames_walk_the_ring() {
        let frames = activity_ring_frames(full());
        assert_eq!(
            frames.len(),
            ACTIVITY_RING.len(),
            "a lap is one frame per dot of the ring"
        );
        let lap = ACTIVITY_RING.len();
        for (step, frame) in frames.iter().enumerate() {
            let window = [step + lap - 1, step, step + 1]
                .into_iter()
                .map(|index| 1u8 << ACTIVITY_RING[index % lap])
                .fold(0u8, |all, dot| all | dot);
            assert_eq!(
                dots(frame),
                window,
                "{frame:?} at step {step} is not the ring's own window"
            );
        }
    }

    /// The ring lights the working spinner's own dots.
    ///
    /// The desktop holds its painted ring to this same dot set, so the two
    /// marks are one mark rather than two that happen to look alike.
    #[test]
    fn the_activity_ring_lights_the_spinner_dots() {
        let lit = spinner_frames(full())
            .iter()
            .map(|frame| dots(frame))
            .fold(0u8, |all, dots| all | dots);
        let ring = ACTIVITY_RING
            .iter()
            .map(|index| 1u8 << index)
            .fold(0u8, |all, dot| all | dot);
        assert_eq!(ring, lit, "the ring and the spinner light different dots");
        assert_eq!(
            ring.count_ones() as usize,
            ACTIVITY_RING.len(),
            "a dot is on the ring twice"
        );
    }

    /// The ring steps faster than the working spinner holds its frames.
    ///
    /// The desktop's sidebar mark turns a lap in 800ms where the spinner takes
    /// 8 × 360ms; a ring that held its frames for the spinner's three ticks
    /// would be the slower one, which is not the mark the band is asked for.
    #[test]
    fn the_activity_ring_turns_faster_than_the_working_spinner() {
        const { assert!(ACTIVITY_RING_TICKS_PER_FRAME < SPINNER_TICKS_PER_FRAME) };
    }

    #[test]
    fn the_legacy_tier_avoids_characters_its_font_lacks() {
        // A legacy console font has ASCII and some CP437; box drawing, Braille
        // and the small geometric shapes are not in it.
        let forbidden = [
            "│", "┃", "▏", "▌", "▾", "▴", "•", "▪", "⠋", "⠙", "○", "◎", "◉", "◆", "◇", "◈", "✗",
            "✓", "⇣", "❯", "╭", "╰", "─",
        ];
        let tier = legacy();
        let mut drawn = vec![
            prompt_arrow(tier),
            accent_bar(tier),
            ballot_x(tier),
            check_mark(tier),
            diamond_filled(tier),
            diamond_hollow(tier),
            diamond_dotted(tier),
            disclosure(true, tier),
            disclosure(false, tier),
            chevron(true, tier),
            chevron(false, tier),
            timeline_tick(tier),
            timeline_tick_active(tier),
            heading_rule(tier),
        ];
        drawn.extend_from_slice(spinner_frames(tier));
        drawn.extend_from_slice(activity_ring_frames(tier));
        drawn.extend_from_slice(idle_pulse_frames(tier));
        for glyph in drawn {
            for character in forbidden {
                assert!(
                    !glyph.contains(character),
                    "{glyph:?} contains {character:?}, which a legacy console cannot render"
                );
            }
        }
    }

    #[test]
    fn frame_selection_cycles_and_holds() {
        let frames = &["a", "b", "c"];
        assert_eq!(frame_at(frames, 0, 2), "a");
        assert_eq!(frame_at(frames, 1, 2), "a");
        assert_eq!(frame_at(frames, 2, 2), "b");
        assert_eq!(frame_at(frames, 6, 2), "a");
        // A zero or absurd hold must still terminate rather than divide by zero.
        assert_eq!(frame_at(frames, 5, 0), "c");
        assert_eq!(frame_at(&[], 5, 2), " ");
    }

    #[test]
    fn the_idle_pulse_is_slower_than_the_working_spinner() {
        // Idle says "alive"; working says "busy". The same cadence for both
        // would make an idle session look like it was doing something.
        const { assert!(IDLE_PULSE_TICKS_PER_FRAME > SPINNER_TICKS_PER_FRAME) };
    }
}
