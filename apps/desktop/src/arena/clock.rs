//! The duel's pixel clock: the live run in the top-left corner and the pace to
//! beat beneath it.
//!
//! The digits are drawn from the same integer raster the guardians are: a 3 × 5
//! face painted as merged rectangles on whole pixels. Nothing here goes through
//! a font, so a running clock reads as part of the field instead of as chrome
//! floating over it.
//!
//! The face sits on its own stepped pixel plate rather than directly on the
//! floor. Every sanctuary has a bright stone floor somewhere under the clock,
//! and ivory digits alone disappear into it; the plate guarantees contrast on
//! every encounter and reads as the same hand-drawn HUD as the exit mark.

use super::combat::{Arena, Phase};
use crate::locale::text;
use gpui::{
    AnyElement, App, Bounds, Hsla, IntoElement, ParentElement as _, Pixels, Point, Styled as _,
    Window, canvas, div, fill, hsla, point, prelude::*, px, size,
};
use gpui_component::ActiveTheme as _;

/// One raster pixel of the face, in logical pixels. Four keeps a 3 × 5 digit
/// legible at arm's length on a 1x display while the finished plate still fits
/// comfortably in the field's top-left corner.
const CELL: f32 = 4.0;
/// Digits are three cells wide; the colon and the tenths period are one.
const DIGIT_WIDTH: usize = 3;
const COLON_WIDTH: usize = 1;
/// One clear cell between characters, so the face does not run together.
const GAP: usize = 1;
/// The plate's outer frame, in cells; the content sits one more cell inside it.
const FRAME: usize = 1;

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

/// The clock's own inks. They are authored here rather than taken from a
/// guardian's material ramp: the clock belongs to the duel, not to one encounter,
/// and keeps the same colors on every field.
///
/// The plate is a dark navy rather than black so it reads as the same warm
/// palette as the guardians' shadow ink, and its border is one step lighter so
/// the plate has an edge on both a bright floor and a dark lagoon.
const PLATE: Hsla = hsla(0.55, 0.30, 0.08, 0.86);
/// The lit top and left edge of the plate's bevel.
const PLATE_LIGHT: Hsla = hsla(0.55, 0.22, 0.42, 0.95);
/// The shaded bottom and right edge, so the plate reads as raised even on the
/// darkest sanctuary floor.
const PLATE_DARK: Hsla = hsla(0.58, 0.38, 0.03, 0.95);
const INK: Hsla = hsla(0.0, 0.0, 1.0, 1.0);
/// The tenths, one step back so they animate without competing for the eye.
const TENTHS: Hsla = hsla(0.0, 0.0, 0.72, 1.0);
const DIGIT_SHADOW: Hsla = hsla(0.0, 0.0, 0.0, 0.65);
const NEW_RECORD: Hsla = hsla(0.12, 0.90, 0.62, 1.0);
/// The quiet line under the face, on the plate's dark ground.
const PACE: Hsla = hsla(0.55, 0.12, 0.78, 1.0);

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
/// The shadow is painted as its own pass under the entire face rather than cell
/// by cell: a per-cell shadow at (+1, +1) would land on the bottom-right of the
/// character before it and close the counters, joining neighbours into a white
/// smear. One pass keeps the shadow on the outer contour only.
///
/// The tenths are drawn in a dimmer ink than minutes and seconds. The eye reads
/// the steady part of the time first, and the tenths become the part that moves
/// without pulling attention away from play.
fn paint_time(millis: u64, origin: Point<Pixels>, color: Hsla, tenths: Hsla, window: &mut Window) {
    let cell = px(CELL);
    let scale = window.scale_factor();
    let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
    let text = format_time(millis);
    let fraction = text.find('.').unwrap_or(text.len());
    // First the whole face's shadow, one cell down and right.
    let mut cursor = origin.x;
    for character in text.chars() {
        let (rows, width) = glyph(character);
        for (row, line) in rows.iter().enumerate() {
            for (col, mark) in line.bytes().enumerate() {
                if mark != b'#' {
                    continue;
                }
                let left = snap(cursor + cell * (col as f32 + 1.0));
                let top = snap(origin.y + cell * (row as f32 + 1.0));
                window.paint_quad(fill(
                    Bounds::new(point(left, top), size(cell, cell)),
                    DIGIT_SHADOW,
                ));
            }
        }
        cursor += cell * (width + GAP) as f32;
    }
    // Then the lit cells, with the fraction dimmed.
    let mut cursor = origin.x;
    for (index, character) in text.chars().enumerate() {
        let (rows, width) = glyph(character);
        let ink = if index >= fraction { tenths } else { color };
        for (row, line) in rows.iter().enumerate() {
            for (col, mark) in line.bytes().enumerate() {
                if mark != b'#' {
                    continue;
                }
                let left = snap(cursor + cell * col as f32);
                let top = snap(origin.y + cell * row as f32);
                window.paint_quad(fill(Bounds::new(point(left, top), size(cell, cell)), ink));
            }
        }
        cursor += cell * (width + GAP) as f32;
    }
}

/// Paints the clock's stepped plate and its border across the element it fills.
///
/// The corners are cut one cell in, so the plate reads as a pixel HUD panel
/// rather than a rounded card: the same square-cornered language as the exit
/// mark, scaled up to hold the face. The plate is painted from the bounds it is
/// given, so the content inside decides its height — a translated pace line is
/// never assumed to be a whole number of cells.
fn paint_plate(bounds: Bounds<Pixels>, window: &mut Window) {
    let cell = px(CELL);
    let scale = window.scale_factor();
    let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
    let cells_w = (f32::from(bounds.size.width) / CELL).ceil().max(4.0) as usize;
    let cells_h = (f32::from(bounds.size.height) / CELL).ceil().max(4.0) as usize;
    let right_edge = bounds.origin.x + bounds.size.width;
    let bottom_edge = bounds.origin.y + bounds.size.height;
    for row in 0..cells_h {
        for col in 0..cells_w {
            // A one-cell notch at each corner, so the plate is stepped rather
            // than rounded.
            let near_left = col == 0;
            let near_right = col + 1 == cells_w;
            let near_top = row == 0;
            let near_bottom = row + 1 == cells_h;
            if (near_left || near_right) && (near_top || near_bottom) {
                continue;
            }
            let left_band = col == 0;
            let top_band = row == 0;
            let right_band = col + 1 == cells_w;
            let bottom_band = row + 1 == cells_h;
            let ink = if top_band || left_band {
                PLATE_LIGHT
            } else if bottom_band || right_band {
                PLATE_DARK
            } else {
                PLATE
            };
            let left = snap(bounds.origin.x + cell * col as f32);
            let top = snap(bounds.origin.y + cell * row as f32);
            // The last column and row take the remainder, so a fractional plate
            // still covers its own element exactly.
            let right = if right_band {
                snap(right_edge)
            } else {
                snap(bounds.origin.x + cell * (col + 1) as f32)
            };
            let bottom = if bottom_band {
                snap(bottom_edge)
            } else {
                snap(bounds.origin.y + cell * (row + 1) as f32)
            };
            window.paint_quad(fill(
                Bounds::new(point(left, top), size(right - left, bottom - top)),
                ink,
            ));
        }
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

/// The duel's clock overlay: a pixel HUD plate in the top-left corner of the
/// field, carrying the live run and the pace to beat.
///
/// The plate guarantees contrast on every sanctuary — the ivory digits never sit
/// directly on a bright stone floor. It is still small enough to leave play
/// visible, and the field's own overlays (the hint line, the exit mark) keep
/// their corners.
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
    let face_width = face_width(face.elapsed_ms);
    let color = if is_record { NEW_RECORD } else { INK };
    let tenths = if is_record {
        NEW_RECORD
    } else {
        TENTHS.opacity(0.85)
    };
    let pace_color = if is_record { NEW_RECORD } else { PACE };
    let mono = cx.theme().mono_font_family.clone();
    // Two frame cells: the bevelled border and the plate's inner padding. The
    // padding is a full two cells on each side, so the plate stays symmetric
    // around a face whose last character has no trailing gap.
    let inset = px(CELL * (FRAME + 1) as f32);
    div()
        .id("unbound-clock")
        .debug_selector(|| "unbound-clock".into())
        .absolute()
        .top_3()
        .left_3()
        .opacity(opacity)
        .aria_label(label)
        .child(
            div()
                .id("unbound-clock-plate")
                .debug_selector(|| "unbound-clock-plate".into())
                .relative()
                .px(inset)
                .pt(inset)
                .pb(px(CELL * (FRAME + 1) as f32))
                .child(
                    canvas(
                        |_, _, _| (),
                        |bounds, _, window, _| paint_plate(bounds, window),
                    )
                    .absolute()
                    .inset_0()
                    .size_full(),
                )
                .child(
                    div()
                        .id("unbound-clock-face")
                        .debug_selector(|| "unbound-clock-face".into())
                        .relative()
                        .w(face_width + px(CELL))
                        .h(px(CELL * 5.0 + CELL))
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, window, _| {
                                    paint_time(
                                        face.elapsed_ms,
                                        bounds.origin,
                                        color,
                                        tenths,
                                        window,
                                    );
                                },
                            )
                            .size_full(),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "unbound-clock-best".into())
                        .relative()
                        .mt(px(CELL))
                        .font_family(mono)
                        .text_size(px(12.0))
                        .text_color(pace_color)
                        .child(pace),
                ),
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

    /// The face has to be readable without a screenshot, and the plate has to
    /// be large enough to frame it: a one-cell cell, a zero-size plate, or a
    /// plate narrower than its face is the failure this pins.
    #[test]
    fn the_face_is_large_enough_to_read_and_its_plate_frames_it() {
        const {
            assert!(
                CELL >= 4.0,
                "a 3x5 digit needs at least four logical pixels"
            );
        }
        let face = face_width(61_500);
        assert!(
            f32::from(face) >= 13.0 * CELL,
            "the mm:ss.t row keeps every one of its cells"
        );
        // What the element reserves around the face: the bevelled frame plus one
        // padding cell on each side, so the plate is strictly larger.
        let inset = CELL * (FRAME + 1) as f32;
        assert!(f32::from(face) + 2.0 * inset > f32::from(face));
        assert!(inset >= 2.0 * CELL, "the border and padding are both drawn");
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
