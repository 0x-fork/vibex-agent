//! The duel's pixel clock: the live run in the top-left corner and the pace to
//! beat beneath it.
//!
//! The digits are drawn from the same integer raster the guardians are: a 3 × 5
//! face painted as merged rectangles on whole pixels. Nothing here goes through
//! a font, so a running clock reads as part of the field instead of as chrome
//! floating over it.

use super::combat::{Arena, Phase};
use crate::locale::text;
use gpui::{
    AnyElement, App, Bounds, Hsla, IntoElement, ParentElement as _, Pixels, Point, Styled as _,
    Window, canvas, div, fill, hsla, point, prelude::*, px, size,
};
use gpui_component::ActiveTheme as _;

/// One raster pixel of the face, in logical pixels. The face sits in the
/// field's corner without covering play, and is still legible at a glance.
const CELL: f32 = 2.0;
/// Digits are three cells wide; the colon is one.
const DIGIT_WIDTH: usize = 3;
const COLON_WIDTH: usize = 1;
/// One clear cell between characters, so the face does not run together.
const GAP: usize = 1;

/// A 3 × 5 pixel face per character, top row first. `#` is ink, `.` is clear.
///
/// Only the characters a clock can show are authored; anything else draws as a
/// blank, so a corrupt or future value cannot paint noise onto the field.
const GLYPHS: [(char, [&str; 5]); 12] = [
    ('0', ["###", "#.#", "#.#", "#.#", "###"]),
    ('1', [".#.", "##.", ".#.", ".#.", "###"]),
    ('2', ["###", "..#", "###", "#..", "###"]),
    ('3', ["###", "..#", "###", "..#", "###"]),
    ('4', ["#.#", "#.#", "###", "..#", "..#"]),
    ('5', ["###", "#..", "###", "..#", "###"]),
    ('6', ["###", "#..", "###", "#.#", "###"]),
    ('7', ["###", "..#", "..#", ".#.", ".#."]),
    ('8', ["###", "#.#", "###", "#.#", "###"]),
    ('9', ["###", "#.#", "###", "..#", "###"]),
    (':', [".", "#", ".", "#", "."]),
    ('.', [".", ".", ".", ".", "#"]),
];

/// The clock's own ink and its one-cell drop shadow. They are authored here
/// rather than taken from a guardian's material ramp: the clock belongs to the
/// duel, not to one encounter, and keeps the same colors on every field.
const INK: Hsla = hsla(0.0, 0.0, 0.98, 1.0);
const SHADOW: Hsla = hsla(0.0, 0.0, 0.0, 0.55);
const NEW_RECORD: Hsla = hsla(0.12, 0.85, 0.68, 1.0);

/// The live run plus the pace to beat, resolved once per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ClockFace {
    /// The duel's own running time, in milliseconds. It freezes when the run
    /// ends, so the number a reader sees is the number being recorded.
    pub elapsed_ms: u64,
    /// The fastest recorded win for this guardian, in milliseconds.
    pub best_ms: Option<u64>,
    /// The run just ended in victory.
    pub victory: bool,
}

impl ClockFace {
    /// The record this frame displays: this run when it won faster, otherwise
    /// the pace it started with.
    pub(super) fn record_ms(&self) -> Option<u64> {
        if self.victory {
            // The winning run is the record when it beat the stored one. A
            // slower replay leaves the stored pace on display.
            Some(
                self.best_ms
                    .map_or(self.elapsed_ms, |best| best.min(self.elapsed_ms)),
            )
        } else {
            self.best_ms
        }
    }

    /// Whether this victory is strictly faster than the pace it started with,
    /// so only a real improvement claims "New best!".
    pub(super) fn improved(&self) -> bool {
        self.victory && self.best_ms.is_none_or(|best| self.elapsed_ms < best)
    }
}

/// What the clock shows, derived from one arena and the pace to beat.
///
/// The live number is the duel's own run clock, so it keeps moving through a
/// death and freezes on the victory it records. The pace line compares this run
/// with the stored record only after a win; during play it is the target.
pub(super) fn face(arena: &Arena, best_ms: Option<u64>) -> ClockFace {
    ClockFace {
        elapsed_ms: arena
            .clear_millis()
            .unwrap_or_else(|| (arena.run_time.max(0.0) * 1000.0).round() as u64),
        // The caller never rewrites this during a duel, so `record_ms` can tell
        // a faster win from a slower replay.
        best_ms,
        victory: arena.phase == Phase::Victory,
    }
}

/// Paints one clock string as merged rectangles on whole pixels.
///
/// Every cell is snapped to the device grid before painting, so the face stays
/// crisp at fractional scale factors instead of dissolving into grey seams.
fn paint_time(millis: u64, origin: Point<Pixels>, color: Hsla, window: &mut Window) {
    let cell = px(CELL);
    let scale = window.scale_factor();
    let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
    let mut cursor = origin.x;
    for character in format_time(millis).chars() {
        let (rows, width) = glyph(character);
        for (row, line) in rows.iter().enumerate() {
            for (col, mark) in line.bytes().enumerate() {
                if mark != b'#' {
                    continue;
                }
                let left = snap(cursor + cell * col as f32);
                let top = snap(origin.y + cell * row as f32);
                window.paint_quad(fill(
                    Bounds::new(point(left, top), size(cell, cell)),
                    SHADOW,
                ));
                let left = snap(cursor + cell * (col as f32 + 1.0));
                let top = snap(origin.y + cell * (row as f32 + 1.0));
                window.paint_quad(fill(Bounds::new(point(left, top), size(cell, cell)), color));
            }
        }
        cursor += cell * (width + GAP) as f32;
    }
}

/// One cell wide are the marks that punctuate a time; every digit is three.
fn character_width(character: char) -> usize {
    match character {
        ':' | '.' => COLON_WIDTH,
        _ => DIGIT_WIDTH,
    }
}

fn glyph(character: char) -> (&'static [&'static str; 5], usize) {
    for (candidate, rows) in &GLYPHS {
        if *candidate == character {
            return (rows, character_width(*candidate));
        }
    }
    (&["...", "...", "...", "...", "..."], DIGIT_WIDTH)
}

/// The width the face needs, in logical pixels, without its shadow.
fn face_width(millis: u64) -> Pixels {
    let live = format_time(millis);
    let cells: usize = live.chars().map(|character| glyph(character).1).sum();
    let gaps = live.chars().count().saturating_sub(1) * GAP;
    px(CELL * (cells + gaps) as f32)
}

/// `mm:ss.t`: whole minutes and seconds, then tenths. An encounter lasts
/// seconds, so minutes are unbounded rather than wrapped at an hour.
pub(super) fn format_time(millis: u64) -> String {
    let tenths = (millis / 100) % 10;
    let seconds = (millis / 1_000) % 60;
    let minutes = millis / 60_000;
    format!("{minutes:02}:{seconds:02}.{tenths}")
}

/// The localized line under the live clock: the pace to beat, or the fact that
/// this run has just set one.
fn pace_copy(best_ms: Option<u64>, new_record: bool) -> String {
    if new_record {
        return format!(
            "{} {}",
            text("New best!", "新纪录！", "新紀錄！"),
            format_time(best_ms.unwrap_or_default())
        );
    }
    match best_ms {
        Some(best) => format!("{} {}", text("Best", "最快", "最快"), format_time(best)),
        None => text("No best yet", "暂无最快", "暫無最快").to_string(),
    }
}

/// The duel's clock overlay: a small pixel face in the top-left corner of the
/// field, with the pace to beat on a quiet line beneath it.
///
/// The overlay never takes an arrow or a click from the field. Its numbers are
/// exposed as one accessible label, so a reader who cannot see the face still
/// hears the run and its record.
pub(super) fn clock_overlay(face: ClockFace, opacity: f32, cx: &mut App) -> AnyElement {
    let live = format_time(face.elapsed_ms);
    let record = face.record_ms();
    let is_record = face.improved();
    let pace = pace_copy(record, is_record);
    let label = format!("{live} · {pace}");
    let width = face_width(face.elapsed_ms);
    let color = if is_record { NEW_RECORD } else { INK };
    let mono = cx.theme().mono_font_family.clone();
    let pace_color = if is_record {
        NEW_RECORD
    } else {
        cx.theme().muted_foreground
    };
    div()
        .id("unbound-clock")
        .debug_selector(|| "unbound-clock".into())
        .absolute()
        .top_3()
        .left_3()
        .flex()
        .flex_col()
        .items_start()
        .gap_1()
        .opacity(opacity)
        .aria_label(label)
        .child(
            div()
                .id("unbound-clock-face")
                .debug_selector(|| "unbound-clock-face".into())
                .w(width + px(CELL))
                .h(px(CELL * 5.0 + CELL))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            paint_time(face.elapsed_ms, bounds.origin, color, window);
                        },
                    )
                    .size_full(),
                ),
        )
        .child(
            div()
                .debug_selector(|| "unbound-clock-best".into())
                .font_family(mono)
                .text_size(px(10.0))
                .text_color(pace_color)
                .child(pace),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_reads_as_minutes_seconds_and_tenths() {
        assert_eq!(format_time(0), "00:00.0");
        assert_eq!(format_time(1_234), "00:01.2");
        assert_eq!(format_time(59_999), "00:59.9");
        assert_eq!(format_time(61_500), "01:01.5");
        assert_eq!(format_time(6_000_000), "100:00.0");
    }

    #[test]
    fn every_character_a_clock_can_show_has_a_square_face() {
        for (character, rows) in GLYPHS {
            assert_eq!(rows.len(), 5, "{character}");
            let width = character_width(character);
            assert!(
                rows.iter().all(|row| row.len() == width),
                "{character} is not {width} cells wide"
            );
        }
        for character in format_time(3_723_400).chars() {
            assert!(
                GLYPHS.iter().any(|(candidate, _)| *candidate == character),
                "{character} has no face"
            );
        }
    }

    #[test]
    fn the_reserved_row_is_exactly_the_digits_it_paints() {
        for millis in [0, 999, 61_500, 3_723_400, 60_000_000] {
            let live = format_time(millis);
            let cells: usize = live.chars().map(|character| glyph(character).1).sum();
            let gaps = live.chars().count().saturating_sub(1);
            assert_eq!(face_width(millis), px(CELL * (cells + gaps) as f32));
        }
    }

    #[test]
    fn the_displayed_record_follows_only_a_faster_win() {
        let face = |elapsed_ms, best_ms, victory| ClockFace {
            elapsed_ms,
            best_ms,
            victory,
        };
        // A win with no record yet becomes the record.
        assert_eq!(face(9_000, None, true).record_ms(), Some(9_000));
        assert!(face(9_000, None, true).improved());
        // A faster win replaces the pace; a slower or equal one keeps it.
        assert_eq!(face(9_000, Some(9_500), true).record_ms(), Some(9_000));
        assert!(face(9_000, Some(9_500), true).improved());
        assert_eq!(face(9_000, Some(8_000), true).record_ms(), Some(8_000));
        assert!(!face(9_000, Some(8_000), true).improved());
        assert_eq!(face(9_000, Some(9_000), true).record_ms(), Some(9_000));
        assert!(!face(9_000, Some(9_000), true).improved());
        // While the run is live, the stored pace is the target.
        assert_eq!(face(4_000, Some(9_500), false).record_ms(), Some(9_500));
        assert!(!face(4_000, Some(9_500), false).improved());
    }

    #[test]
    fn the_pace_line_names_the_record_a_new_one_and_an_empty_one() {
        assert!(pace_copy(None, false).contains("No best"));
        assert!(pace_copy(Some(12_300), false).contains("00:12.3"));
        assert!(pace_copy(Some(12_300), false).contains("Best"));
        assert!(pace_copy(Some(12_300), true).contains('!'));
        assert!(pace_copy(Some(12_300), true).contains("00:12.3"));
    }

    #[test]
    fn the_clock_freezes_on_the_victory_it_records() {
        let mut arena = Arena::new(super::super::combat::Guardian::Claude, 0.0);
        assert!(!face(&arena, None).victory);
        arena.run_time = 12.34;
        arena.phase = Phase::Victory;
        assert_eq!(arena.clear_millis(), Some(12_340));
        let settled = face(&arena, Some(12_340));
        assert!(settled.victory);
        assert_eq!(settled.elapsed_ms, 12_340);
        // A run clock that only moves forward is what the record compares.
        arena.run_time = 9.99;
        assert_eq!(face(&arena, Some(12_340)).elapsed_ms, 9_990);
    }
}
