//! The light that crosses a label while work is in flight.
//!
//! A running turn is drawn on three surfaces: the desktop timeline's live row,
//! the desktop's session list, and this client's turn-status band. All three
//! say it with the same gesture — a soft band of light crossing the words, one
//! lap every 1200 ms — because a reader who has two of them open must not have
//! to learn that "in flight" looks like two different things.
//!
//! # What makes it read as a light
//!
//! * **it brightens the label; it does not dim it.** The band's centre walks
//!   the label's own ink toward the canvas's opposite — silver-white on a dark
//!   page, ink on a light one — which is the statement the landing mark's light
//!   already makes, and for the same reason ([`crate::logo`]). The ink the
//!   label wears at rest is therefore the *floor* of the ramp rather than a
//!   colour the band replaces, and the label has to rest below full strength to
//!   have anywhere left to brighten;
//! * **one lap is one crossing, and the light comes back where it left.** The
//!   centre starts a pad before the first column and ends a pad past the last
//!   one, so the top of a lap is the unlit label and the light that goes out at
//!   the right comes back in at the left. Unlike the landing mark's light there
//!   is no quiet stretch between laps: a turn in flight is in flight the whole
//!   time, and a light that paused would say the client had stopped watching;
//! * **the band is the label's own width.** Its half-width is a fraction of the
//!   text it crosses (the desktop's 0.42), so a four-character phase word and a
//!   runtime's long activity line are lit proportionally instead of the short
//!   one being swallowed by a fixed-width band.
//!
//! # What it costs, and where it stops
//!
//! The light is only drawn where the label is drawn, so the caller truncates
//! the text to the columns the band has room for: a lap then completes over
//! what the reader can see rather than spending half of itself off the edge.
//!
//! A palette that cannot blend has no ramp to spend, so the band is carried by
//! weight — `BOLD` where the light is strongest — and a terminal with colour
//! switched off entirely is drawn flat and still. That last case loses nothing
//! the reader needs: the activity mark beside the label keeps moving and
//! already says the client is alive, while a light with neither colour nor
//! weight to spend could only flicker between two identical frames. It is the
//! landing mark's contract, applied to a second light rather than answered a
//! second way.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::{ColorMode, TuiTheme};

/// One lap of the light, in milliseconds: the desktop's own sweep.
///
/// `TIMELINE_SHIMMER_SWEEP` in `apps/desktop` is 1200 ms, and every live label
/// on every surface laps at that pace. The number is named here rather than
/// copied into a frame count so that the parity is the thing being checked.
pub const SWEEP_MILLIS: u128 = 1_200;

/// One lap of the light, in frames of this client's animation clock.
///
/// Ten frames is the desktop's 1200 ms at the 120 ms tick the client runs while
/// a turn is streaming; that tick belongs to the event loop
/// ([`crate::run::ANIMATION_TICK`]) and this layer does not read it. The slow
/// tick the client falls back to when nothing else is moving stretches a lap to
/// two seconds, which is the same trade the landing mark's light makes. The
/// clock is the client's own rather than a wall clock read at draw time: that
/// is what keeps a frame a pure function of the phase, so the same phase draws
/// the same picture twice and a test can say what a frame of a lap looks like.
pub const SWEEP_FRAMES: u32 = 10;

/// The light's half-width, as a fraction of the label's own width.
///
/// The desktop's `TIMELINE_SHIMMER_SPREAD`. A relative spread is what keeps a
/// short phrase and a long one proportionally lit; an absolute one would leave
/// a four-character phase word either unlit or entirely white.
const SPREAD: f32 = 0.42;

/// How far outside the label the light's centre starts and ends, as a fraction
/// of the label's width.
///
/// The desktop's own margin. It is what puts the head of the band clear of the
/// first column when a lap opens and the tail clear of the last one when it
/// closes, rather than clipping a band against the edge of the text.
const REST_PAD: f32 = 0.05;

/// Above this much light a cell is drawn in weight instead of in colour.
pub(crate) const LIT: f32 = 0.45;

/// The colour a light takes.
///
/// The catalogue has no token named for a light, and the one it carries for the
/// job is `border`: authored as the canvas's opposite in every shipped theme —
/// `#ffffff` in all ten dark ones, `#000000` in all ten light ones — which is
/// what light is, and what makes a specular read as a specular rather than as a
/// slightly lighter grey. On a light page that inverts to ink, which is the
/// same statement: the strongest thing that can be drawn on the canvas it falls
/// on. The token is named for hairlines because that is what it was authored
/// for; its value is what a light needs, and minting a second white here would
/// put a colour outside the catalogue that every theme would then have to be
/// checked against.
pub(crate) fn silver(theme: &TuiTheme) -> Color {
    theme.roles.border
}

/// Whether this terminal can be shown a light at all.
///
/// With colour switched off there is nothing to spend: a light drawn in one
/// colour, with no weight to carry it, would only flicker between two identical
/// frames. The page is still drawn whole; only the movement is dropped.
pub(crate) fn lights_up(theme: &TuiTheme) -> bool {
    !matches!(theme.capability.mode, ColorMode::None)
}

/// How much light the cell centred on `column` carries at `phase`.
///
/// Zero at rest and one under the centre of the band. The centre crosses from a
/// pad before the first column to a pad past the last one, so a lap opens and
/// closes on the unlit label; the falloff either side of it is the smoothstep
/// the desktop's own wordmark light uses, which is what keeps the moving band a
/// band rather than a hard-edged block sliding across the words.
///
/// The desktop samples this continuously at display refresh and never draws the
/// far edge of the crossing. A lap here is ten frames, so the last of them
/// still catches a trace of the band's tail before the light comes back at the
/// left; that beat between laps is the seam this leaves, and it reads as the
/// light having left the words rather than as a cut.
fn light_at(column: f32, width: f32, phase: u32) -> f32 {
    let half = width * SPREAD;
    if half <= 0.0 {
        return 0.0;
    }
    let pad = half + width * REST_PAD;
    let crossed = (phase % SWEEP_FRAMES) as f32 / SWEEP_FRAMES as f32;
    let centre = crossed * (width + 2.0 * pad) - pad;
    let falloff = (1.0 - (column - centre).abs() / half).clamp(0.0, 1.0);
    falloff * falloff * (3.0 - 2.0 * falloff)
}

/// The label with the light crossing it, one span per grapheme.
///
/// Cells are measured in display columns rather than characters, so a CJK phase
/// word is lit across the columns it actually occupies. `ink` is the colour the
/// label wears at rest and the floor the light rises from; `animate` is whether
/// anything is in flight at all — a label that is not running is drawn as the
/// single flat span the caller would have written itself.
pub fn spans(
    text: &str,
    ink: Color,
    theme: &TuiTheme,
    phase: u32,
    animate: bool,
) -> Vec<Span<'static>> {
    let rest = Style::default().fg(ink);
    let width = crate::text::display_width(text) as f32;
    if !animate || !lights_up(theme) || width <= 0.0 {
        return vec![Span::styled(text.to_string(), rest)];
    }
    let blending = theme.capability.mode.blends();
    let mut spans = Vec::new();
    let mut column = 0.0f32;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme) as f32;
        let light = light_at(column + grapheme_width / 2.0, width, phase);
        let style = if blending {
            Style::default().fg(theme.blend(ink, silver(theme), light))
        } else if light > LIT {
            rest.add_modifier(Modifier::BOLD)
        } else {
            rest
        };
        spans.push(Span::styled(grapheme.to_string(), style));
        column += grapheme_width;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::display_width;
    use crate::theme::{ColorCapability, GlyphMode};
    use vibex_ui::GpuiThemeMode;

    /// An even number of columns, so the middle of a lap falls between two
    /// cells: what the sweep does between them is worth checking too.
    const LABEL: &str = "Calling tool";

    /// An odd number of columns, so the middle of a lap lands on a cell.
    const ODD: &str = "Reasoning";

    fn theme(mode: ColorMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn light_theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-light"),
            GpuiThemeMode::Light,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn painted(spans: &[Span<'static>]) -> String {
        format!("{spans:?}")
    }

    fn written(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn inks(text: &str, theme: &TuiTheme, phase: u32) -> Vec<Color> {
        spans(text, theme.roles.gray, theme, phase, true)
            .iter()
            .map(|span| span.style.fg.expect("a cell was not drawn"))
            .collect()
    }

    fn brightness(colour: Color) -> i32 {
        match colour {
            Color::Rgb(r, g, b) => i32::from(r) + i32::from(g) + i32::from(b),
            other => panic!("true colour produced {other:?}"),
        }
    }

    /// How far one ink has risen above another, in levels of one channel.
    ///
    /// The ramp walks all three channels together, so a channel is the whole
    /// reading a frame of the light needs.
    fn lift(colour: Color, base: Color) -> i32 {
        brightness(colour) / 3 - brightness(base) / 3
    }

    #[test]
    fn a_lap_crosses_the_label_without_ever_redrawing_it() {
        let dark = theme(ColorMode::TrueColor);
        let frame = |phase: u32| spans(LABEL, dark.roles.gray, &dark, phase, true);
        // Every frame of two whole laps spells the same words: the movement is
        // in the ink, not in the characters.
        for phase in 0..(2 * SWEEP_FRAMES) {
            assert_eq!(
                written(&frame(phase)),
                LABEL,
                "phase {phase} redrew the label"
            );
        }
        // The ink moves across the whole lap, from the first frame to the last
        // one rather than stopping halfway.
        assert_ne!(painted(&frame(0)), painted(&frame(SWEEP_FRAMES / 2)));
        assert_ne!(
            painted(&frame(SWEEP_FRAMES / 2)),
            painted(&frame(SWEEP_FRAMES - 1))
        );
    }

    #[test]
    fn a_lap_opens_on_the_unlit_label_and_fades_out_past_the_far_edge() {
        let dark = theme(ColorMode::TrueColor);
        // The band's centre starts a pad before the first column, so the top of
        // a lap is the plain label: nothing has been lit yet.
        let first = inks(LABEL, &dark, 0);
        assert!(
            first.iter().all(|ink| *ink == dark.roles.gray),
            "the lap did not open at rest: {first:?}"
        );
        // The last frame is the band on its way out: whatever still catches its
        // tail is a trace, nowhere near what the middle of the lap draws. The
        // desktop samples this continuously and never shows the tail at all; a
        // ten-frame lap does, and a trace is the seam that leaves.
        let last = inks(LABEL, &dark, SWEEP_FRAMES - 1);
        let middle = inks(LABEL, &dark, SWEEP_FRAMES / 2);
        let tail = last
            .iter()
            .map(|ink| lift(*ink, dark.roles.gray))
            .max()
            .unwrap();
        let peak = middle
            .iter()
            .map(|ink| lift(*ink, dark.roles.gray))
            .max()
            .unwrap();
        assert!(tail < peak / 3, "the lap did not fade out: {last:?}");
        // Halfway through the lap the light is on the label and brighter than
        // anything the ends of the lap drew.
        assert!(
            middle.iter().map(|ink| brightness(*ink)).max()
                > last.iter().map(|ink| brightness(*ink)).max(),
            "the light did not reach the label in the middle of a lap"
        );
        // A lap comes round exactly: what the client draws at a phase and at
        // that phase plus a lap is the same picture.
        for phase in 0..SWEEP_FRAMES {
            assert_eq!(
                painted(&spans(LABEL, dark.roles.gray, &dark, phase, true)),
                painted(&spans(
                    LABEL,
                    dark.roles.gray,
                    &dark,
                    phase + SWEEP_FRAMES,
                    true
                )),
                "phase {phase} does not repeat"
            );
        }
    }

    #[test]
    fn the_band_fades_away_either_side_of_its_centre() {
        // The light is a ramp rather than a block: it is brightest at the
        // centre of the band and falls away with distance, so a reader sees a
        // highlight travelling rather than a white cell jumping along.
        let width = display_width(LABEL) as f32;
        let middle = SWEEP_FRAMES / 2;
        let centre = width / 2.0;
        assert!(light_at(centre, width, middle) > light_at(centre - 2.0, width, middle));
        assert!(light_at(centre, width, middle) > light_at(centre + 2.0, width, middle));
        // Nothing outside the band is lit at all, however far into the lap.
        assert_eq!(light_at(-width, width, middle), 0.0);
        assert_eq!(light_at(width * 2.0, width, middle), 0.0);
        // A relative spread lights a short phrase and a long line alike. The
        // peak is a level or two short of the highlight where the band's centre
        // falls between two cells — which is what an even-width label gets at
        // the middle of a lap — so the claim is that both are almost all the
        // way up their own ramp, not that they land on the same level.
        let dark = theme(ColorMode::TrueColor);
        let lift_at_peak = |text: &str| {
            spans(text, dark.roles.gray, &dark, middle, true)
                .iter()
                .map(|span| lift(span.style.fg.unwrap(), dark.roles.gray))
                .max()
                .unwrap()
        };
        let full = lift(dark.roles.border, dark.roles.gray);
        assert!(lift_at_peak("Generating") > full * 4 / 5);
        assert!(lift_at_peak(LABEL) > full * 4 / 5);
    }

    #[test]
    fn the_light_is_the_canvas_s_opposite_and_the_ink_is_what_it_rises_from() {
        let at = |theme: &TuiTheme, text: &str| {
            spans(text, theme.roles.gray, theme, SWEEP_FRAMES / 2, true)
                .iter()
                .map(|span| span.style.fg.unwrap())
                .collect::<Vec<_>>()
        };
        // A label of an odd width puts the middle of the lap on a cell, and
        // that cell is the colour the catalogue keeps for a light — brighter
        // than the label's own grey, because the light brightens what it
        // crosses rather than replacing it.
        let dark = theme(ColorMode::TrueColor);
        let lit = at(&dark, ODD);
        let peak = lit
            .iter()
            .copied()
            .max_by_key(|ink| brightness(*ink))
            .unwrap();
        assert_eq!(peak, dark.roles.border);
        assert!(brightness(peak) > brightness(dark.roles.gray));

        // On a light page the same ramp is ink: a light cannot be brighter than
        // the paper it falls on, so the strongest thing to draw is the darkest.
        let light = light_theme();
        let lit = at(&light, ODD);
        let peak = lit
            .iter()
            .copied()
            .min_by_key(|ink| brightness(*ink))
            .unwrap();
        assert_eq!(peak, light.roles.border);
        assert!(brightness(peak) < brightness(light.roles.gray));
    }

    #[test]
    fn a_palette_that_cannot_blend_gets_weight_instead() {
        // An indexed palette hands back a slot rather than a colour, so there
        // is no ramp to draw. The band is carried by weight, which is the only
        // brightness a shallow palette has — and it still has to travel, or the
        // label would read as steady emphasis rather than as work in flight.
        for mode in [ColorMode::Ansi16, ColorMode::Ansi256] {
            let theme = theme(mode);
            let frame = |phase: u32| spans(LABEL, theme.roles.gray, &theme, phase, true);
            let bold = |phase: u32| {
                frame(phase)
                    .iter()
                    .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
                    .count()
            };
            assert!(
                (0..SWEEP_FRAMES).any(|phase| bold(phase) > 0),
                "{mode:?} never marked the light"
            );
            assert!(
                (0..SWEEP_FRAMES).any(|phase| bold(phase) == 0),
                "{mode:?} marked the whole label at every phase"
            );
            assert_ne!(
                painted(&frame(0)),
                painted(&frame(SWEEP_FRAMES / 2)),
                "{mode:?} did not move the band"
            );
            // Nothing is drawn as a blend the palette cannot take apart.
            assert!(
                frame(SWEEP_FRAMES / 2)
                    .iter()
                    .all(|span| !matches!(span.style.fg, Some(Color::Rgb(..)))),
                "{mode:?} blended a colour it cannot hold"
            );
        }
    }

    #[test]
    fn a_colourless_terminal_holds_the_label_still() {
        // Colour switched off is the floor: there is no second ink to spend, so
        // the label is drawn whole and flat. The mark beside it still moves,
        // and that is what says the client is alive.
        let plain = theme(ColorMode::None);
        let resting = spans(LABEL, plain.roles.gray, &plain, 0, true);
        assert_eq!(resting.len(), 1);
        for phase in [0, SWEEP_FRAMES / 2, SWEEP_FRAMES - 1] {
            assert_eq!(
                painted(&spans(LABEL, plain.roles.gray, &plain, phase, true)),
                painted(&resting),
                "a colourless terminal animated the label at phase {phase}"
            );
        }
    }

    #[test]
    fn a_label_that_is_not_running_is_the_flat_span_the_caller_would_draw() {
        let dark = theme(ColorMode::TrueColor);
        let flat = spans(LABEL, dark.roles.gray, &dark, SWEEP_FRAMES / 2, false);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].content.as_ref(), LABEL);
        assert_eq!(flat[0].style.fg, Some(dark.roles.gray));
        // A label with no columns has no light to draw and no seam to leave.
        let empty = spans("", dark.roles.gray, &dark, 3, true);
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].content.as_ref(), "");
    }

    #[test]
    fn a_wide_label_is_lit_across_the_columns_it_occupies() {
        let dark = theme(ColorMode::TrueColor);
        // Every grapheme keeps its own span so the ramp can fall between them,
        // and the drawn text is the label either way.
        let spans = spans("调用工具", dark.roles.gray, &dark, SWEEP_FRAMES / 2, true);
        assert_eq!(spans.len(), 4);
        assert_eq!(written(&spans), "调用工具");
        // Eight columns of label: the band's centre mid-lap is under the second
        // word, so the two cells of that word carry more light than the rest.
        let inks = inks("调用工具", &dark, SWEEP_FRAMES / 2);
        assert!(brightness(inks[1]) > brightness(inks[3]));
        assert!(brightness(inks[2]) > brightness(inks[0]));
    }
}
