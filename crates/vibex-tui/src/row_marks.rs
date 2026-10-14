//! The marks a row draws as a picture the terminal holds.
//!
//! Two things on a session list want a picture: the Agent that answers for a
//! row, and the icons the row's buttons wear. Both are small, both are looked
//! up by name, and both are drawn the same way — a PNG placed over the cells
//! the row reserved — so one pool serves them rather than two that would
//! duplicate the placement logic and compete for image ids.
//!
//! # Why a picture, and not a character
//!
//! A character grid cannot draw a brand mark. A cell holds two pixels of
//! half-block raster, so an icon the height of a row is four pixels tall: a
//! shape, not a logo. What *can* draw one is the terminal's own image protocol
//! ([`crate::terminal_image`]), which is handed the source picture and scales it
//! to the cells it was given — a two-cell box on an ordinary terminal is a
//! 16-by-16 icon, not a smear.
//!
//! So a mark is drawn when the terminal speaks an image protocol, and when it
//! does not, the caller draws the character the same vocabulary carries
//! ([`crate::sidebar_icons::Icon::glyph`], or the Agent's name). There is no
//! half-block approximation in between: a mark that resolved to four pixels
//! would be a worse answer than the word it replaced.
//!
//! # Why the marks themselves live elsewhere
//!
//! An Agent's identity table and mark bytes are [`vibex_brands`], shared with
//! the desktop and the phone. The chrome icons are the desktop asset pack, named
//! by [`crate::sidebar_icons`]. This module is only the terminal's half: which
//! marks are on screen this frame, where, and the escapes the run loop has to
//! write after the frame. Nothing here decides what a mark is.
//!
//! # The pool
//!
//! A terminal holds a picture under an image id, so two marks on screen at once
//! need two ids. Rows come and go with the scroll, so ids are pooled rather than
//! bound to a row: a slot is reused for whichever mark is wanted next, and a
//! slot no row wants is taken down and released for the next frame. A slot that
//! already holds the mark a row wants is *moved* rather than retransmitted — a
//! placement is a few dozen bytes against a picture's kilobytes — which is what
//! keeps a scroll from sending every mark again.
//!
//! The distinction that matters is placement, not identity: two visible rows can
//! show the same Agent, so two slots can hold the same mark at two rectangles.
//! The bytes behind them are shared through a small cache, because rastering and
//! encoding the same mark twice is the expensive half and the terminal only ever
//! needs one copy per id.

use std::sync::Arc;

use ratatui::layout::Rect;

use crate::preview::PlacedPicture;
use crate::sidebar_icons::Icon;
use crate::terminal_image::ImageSupport;

/// How many cells wide one mark is.
///
/// Two cells is the narrowest box that is still square-ish under the cell
/// aspect the rest of this client assumes, and one row tall: a mark that covered
/// the echo line under it would be answering a question the row is not asking.
pub const MARK_COLS: u16 = 2;
/// How many cell rows one mark covers.
pub const MARK_ROWS: u16 = 1;

/// The pixel edge a mark is rastered at before the terminal scales it.
///
/// The terminal scales the source into the cells it was placed in, and cell
/// geometry is its own secret, so this is deliberately larger than any likely
/// on-screen size: a downscale keeps the mark's edges, and the PNG stays a few
/// kilobytes either way.
const MARK_PIXELS: u32 = 64;

/// The first image id a mark may use.
///
/// Above the chip preview's and the pairing code's, which own 1 and 2. Distinct
/// ids are what let a mark leave the terminal without disturbing either.
const FIRST_IMAGE_ID: u32 = 100;

/// How many distinct marks are kept rastered.
///
/// The raster is the expensive half and it depends only on the mark and the
/// colour, so it outlives the slot that used it. A reader scrolling a list of
/// Agents re-uses a handful; a bound keeps a long session from holding every
/// mark it ever saw.
const MAX_CACHED: usize = 64;

/// What a row wants drawn in a picture slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Mark {
    /// An Agent's brand mark, by the identity string [`vibex_brands`] matches.
    Agent(String),
    /// One of the session list's own icons.
    Chrome(Icon),
}

impl Mark {
    /// The label the terminal's picture is cached and compared under.
    ///
    /// The two families are prefixed apart rather than merged: an Agent whose
    /// id happened to spell an icon's key would otherwise share its picture.
    fn label(&self) -> String {
        match self {
            Self::Agent(identity) => format!("agent:{identity}"),
            Self::Chrome(icon) => format!("chrome:{}", icon.key()),
        }
    }

    /// The bytes the terminal is handed, or `None` when this mark cannot be
    /// drawn at all — the caller keeps its own fallback rather than a blank.
    fn png(&self, color: Option<u32>) -> Option<Vec<u8>> {
        match self {
            Self::Agent(identity) => {
                let mark = vibex_brands::agent_mark(identity)?;
                vibex_brands::mark_png(mark, MARK_PIXELS, color)
            }
            Self::Chrome(icon) => {
                let svg = std::str::from_utf8(icon.svg()).ok()?;
                let svg = vibex_brands::themed_svg(svg, color)?;
                vibex_brands::svg_png(&svg, MARK_PIXELS)
            }
        }
    }
}

/// One placement the terminal is holding.
#[derive(Debug)]
struct Slot {
    /// The mark under that id.
    picture: PlacedPicture,
    /// The mark the picture is, and the colour it was drawn in: together they
    /// are what a row is asking for.
    label: String,
    color: Option<u32>,
    /// Whether this frame asked for the slot.
    wanted: bool,
}

impl Slot {
    fn new(id: u32) -> Self {
        Self {
            picture: PlacedPicture::with_id(id),
            label: String::new(),
            color: None,
            wanted: false,
        }
    }

    /// Whether the terminal is holding this mark in this rectangle already.
    fn holds(&self, label: &str, color: Option<u32>, rect: Rect) -> bool {
        self.label == label && self.color == color && self.picture.holds(label, rect)
    }

    /// Whether this slot is holding the mark a row wants, anywhere.
    fn is(&self, label: &str, color: Option<u32>) -> bool {
        self.label == label && self.color == color
    }
}

/// The marks the terminal is holding for the frame being drawn.
///
/// Frame-shaped: [`RowMarks::begin_frame`] starts one, each row that wants a
/// mark calls [`RowMarks::show`], and [`RowMarks::settle`] takes down every mark
/// no row asked for — including all of them on a terminal that cannot hold a
/// picture, and on every frame of a page that has no rows.
#[derive(Debug, Default)]
pub struct RowMarks {
    slots: Vec<Slot>,
    /// Marks already rastered, by label and colour, oldest first.
    cache: Vec<(String, Option<u32>, Arc<Vec<u8>>)>,
}

impl RowMarks {
    /// Begin a frame: no row has asked for a mark yet.
    pub fn begin_frame(&mut self) {
        for slot in &mut self.slots {
            slot.wanted = false;
        }
    }

    /// Put `mark` in `rect`.
    ///
    /// `color` is the packed sRGB a themed mark is drawn in; a mark that
    /// carries its own colours ignores it. Returns whether a mark was placed:
    /// `false` when the mark has no picture, when the terminal cannot hold one,
    /// or when the pool is full — the caller draws the character instead, which
    /// is not a degraded answer but the other one.
    pub fn show(
        &mut self,
        support: &ImageSupport,
        mark: &Mark,
        color: Option<u32>,
        rect: Rect,
    ) -> bool {
        if !support.supports_pixels() || rect.width == 0 || rect.height == 0 {
            return false;
        }
        let label = mark.label();
        // The same mark in the same rectangle is already on the terminal: the
        // frame asked for exactly what is there, so it sends nothing.
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.holds(&label, color, rect))
        {
            self.slots[index].wanted = true;
            return true;
        }
        let Some(prepared) = self.cached(&label, mark, color) else {
            return false;
        };
        // A slot holding this mark somewhere else is a move, which is a few
        // dozen bytes rather than the picture — but only a slot no other row has
        // asked for this frame. An id holds one rectangle, so a row that took a
        // slot another row is already drawn in would leave that row blank: two
        // rows showing one Agent take two slots, and the bytes behind them are
        // shared through the cache.
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.is(&label, color) && !slot.wanted)
        {
            let slot = &mut self.slots[index];
            slot.wanted = true;
            slot.picture.place(support, &label, prepared, rect);
            return true;
        }
        let Some(slot) = self.take_slot() else {
            return false;
        };
        slot.wanted = true;
        slot.color = color;
        slot.label = label.clone();
        slot.picture.place(support, &label, prepared, rect);
        true
    }

    /// Take down every mark no row asked for this frame.
    ///
    /// Run after the frame, like the chip preview's own settle: a mark is a
    /// layer the terminal keeps, so one that is no longer drawn has to be
    /// deleted rather than left standing over whatever replaced its row. The
    /// slot itself is kept and released, so the next frame reuses its id instead
    /// of churning through the pool.
    pub fn settle(&mut self, support: &ImageSupport) {
        for slot in &mut self.slots {
            if slot.wanted {
                continue;
            }
            slot.picture.clear(support);
            slot.label.clear();
            slot.color = None;
        }
    }

    /// Take the escapes the frame queued, leaving none behind.
    pub fn take_escapes(&mut self) -> Vec<u8> {
        let mut escapes = Vec::new();
        for slot in &mut self.slots {
            escapes.extend_from_slice(&slot.picture.take_escapes());
        }
        escapes
    }

    /// Whether the terminal is holding a mark this client put there.
    pub fn is_holding(&self) -> bool {
        self.slots.iter().any(|slot| slot.picture.is_holding())
    }

    /// The image ids currently in use, for tests.
    #[cfg(test)]
    fn ids(&self) -> Vec<u32> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.label.is_empty())
            .map(|(index, _)| FIRST_IMAGE_ID + index as u32)
            .collect()
    }

    /// The rastered bytes for a mark, rastering and caching them on first use.
    ///
    /// `None` when the mark cannot be drawn, which is the caller's signal to
    /// keep its character.
    fn cached(&mut self, label: &str, mark: &Mark, color: Option<u32>) -> Option<Arc<Vec<u8>>> {
        if let Some((_, _, bytes)) = self
            .cache
            .iter()
            .find(|(cached_label, cached_color, _)| cached_label == label && *cached_color == color)
        {
            return Some(bytes.clone());
        }
        let png = mark.png(color)?;
        let bytes = Arc::new(png);
        self.cache.push((label.to_string(), color, bytes.clone()));
        if self.cache.len() > MAX_CACHED {
            self.cache.remove(0);
        }
        Some(bytes)
    }

    /// A slot to transmit into: a released one, or a new one. A slot that still
    /// holds a mark another row wants this frame is never taken, so the pool
    /// grows only to the most marks that have ever been wanted in one frame —
    /// the rows on screen — and then stays there.
    fn take_slot(&mut self) -> Option<&mut Slot> {
        if let Some(index) = self.slots.iter().position(|slot| slot.label.is_empty()) {
            return Some(&mut self.slots[index]);
        }
        let id = FIRST_IMAGE_ID.saturating_add(self.slots.len() as u32);
        self.slots.push(Slot::new(id));
        self.slots.last_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_image::{ImageProtocol, PAIRING_IMAGE_ID, PREVIEW_IMAGE_ID};

    const DEEPSEEK: &str = "deepseek-harness DeepSeek Harness";
    const CURSOR: &str = "cursor Cursor";

    fn agent(identity: &str) -> Mark {
        Mark::Agent(identity.to_string())
    }

    fn rect(x: u16, y: u16) -> Rect {
        Rect::new(x, y, MARK_COLS, MARK_ROWS)
    }

    /// The image ids an escape names, read as whole numbers.
    ///
    /// A bare substring test is wrong here — `i=100` contains `i=1` — so the
    /// ids are parsed out of the `i=` parameters the sequences actually carry.
    fn image_ids(escapes: &str) -> Vec<u32> {
        escapes
            .split("i=")
            .skip(1)
            .filter_map(|tail| {
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .collect()
    }

    /// A mark is transmitted once and then moved, not re-sent, and it travels
    /// under its own image id rather than the preview's or the code's.
    #[test]
    fn a_mark_is_sent_once_and_then_moved() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();

        marks.begin_frame();
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6)));
        let sent = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        assert!(
            sent.contains("a=T,f=100"),
            "the mark was not sent: {sent:?}"
        );
        let id = marks.ids()[0];
        assert_ne!(
            id, PREVIEW_IMAGE_ID,
            "a mark must not take the preview's id"
        );
        assert_ne!(id, PAIRING_IMAGE_ID, "a mark must not take the code's id");
        assert!(
            image_ids(&sent).contains(&id),
            "the mark travelled under another id: {sent:?}"
        );

        // The same mark in the same rectangle sends nothing: the terminal
        // already holds exactly this.
        marks.begin_frame();
        marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6));
        assert!(
            marks.take_escapes().is_empty(),
            "an unchanged frame sent the mark again"
        );

        // The same mark in another rectangle is a *move*, not a transmission:
        // the picture is already on the terminal.
        marks.begin_frame();
        marks.show(&support, &agent(DEEPSEEK), None, rect(8, 6));
        let moved = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        assert!(moved.contains("a=p"), "the mark was not moved: {moved:?}");
        assert!(
            !moved.contains("a=T,f=100"),
            "moving a mark re-sent the picture: {moved:?}"
        );
    }

    /// Two rows showing one Agent wear two marks at once: an id holds one
    /// rectangle, so the second row takes a slot of its own rather than moving
    /// the picture off the row above it and leaving that row blank.
    #[test]
    fn one_agent_on_two_rows_holds_two_marks() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6)));
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 9)));
        let ids = marks.ids();
        assert_eq!(ids.len(), 2, "the second row took the first row's mark");

        // The frame after it asks for exactly the two rectangles again, so the
        // terminal is already holding both and nothing is sent.
        marks.take_escapes();
        marks.begin_frame();
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6)));
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 9)));
        assert!(
            marks.take_escapes().is_empty(),
            "an unchanged frame sent a mark again"
        );
    }

    /// A frame that stops asking takes its mark down, and only under its own id.
    #[test]
    fn a_mark_nobody_wants_is_taken_down() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6));
        let id = marks.ids()[0];
        marks.take_escapes();

        marks.begin_frame();
        marks.settle(&support);
        let cleared = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        assert!(
            cleared.contains(&format!("a=d,d=i,i={id}")),
            "the mark was not deleted: {cleared:?}"
        );
        // The ids are read as whole values, not as bare substrings: `i=100`
        // contains `i=1`, so a plain `contains` would pass while the code
        // cleared the preview's picture instead of the mark's.
        let named = image_ids(&cleared);
        assert!(
            !named.contains(&PREVIEW_IMAGE_ID) && !named.contains(&PAIRING_IMAGE_ID),
            "taking a mark down named another surface's picture: {cleared:?}"
        );
        assert!(!marks.is_holding());
    }

    /// Two rows showing two Agents hold two marks at once, and neither is lost
    /// when the other is taken down — the reason a pool exists at all.
    #[test]
    fn two_agents_hold_two_marks() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        assert!(marks.show(&support, &agent(DEEPSEEK), None, rect(4, 6)));
        assert!(marks.show(&support, &agent(CURSOR), Some(0xffffff), rect(4, 7)));
        assert_eq!(marks.ids().len(), 2);
        let ids = marks.ids();
        assert_ne!(ids[0], ids[1], "two marks shared one image id");
        marks.take_escapes();

        // Only the second is wanted, so only the second survives.
        marks.begin_frame();
        marks.show(&support, &agent(CURSOR), Some(0xffffff), rect(4, 7));
        marks.settle(&support);
        assert!(marks.is_holding());
        let cleared = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        // The deletion names an id, and the wanted mark's id 101 contains the
        // dropped one's 100 as a prefix — so the ids are read as numbers.
        let deleted = image_ids(&cleared);
        assert!(
            deleted.contains(&ids[0]),
            "the dropped mark was not deleted: {cleared:?}"
        );
        assert!(
            !deleted.contains(&ids[1]),
            "the wanted mark was deleted with it: {cleared:?}"
        );
    }

    /// An Agent with no mark, and a terminal with no pixels, are both answered
    /// with `false` so the caller draws the name rather than a blank column.
    #[test]
    fn no_mark_and_no_pixels_are_both_refusals() {
        let kitty = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        assert!(
            !marks.show(&kitty, &agent("custom acp"), None, rect(4, 6)),
            "an Agent with no mark must be refused"
        );
        assert!(marks.take_escapes().is_empty());

        let none = ImageSupport::forced(ImageProtocol::None);
        assert!(
            !marks.show(&none, &agent(DEEPSEEK), None, rect(4, 6)),
            "a terminal without pixels must be refused"
        );
        assert!(marks.take_escapes().is_empty());
        assert!(!marks.is_holding());
    }

    /// A themed mark needs a foreground colour; without one it is refused
    /// rather than drawn black, and the caller keeps the name.
    #[test]
    fn a_themed_mark_without_a_colour_is_refused() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        assert!(!marks.show(&support, &agent(CURSOR), None, rect(4, 6)));
        // The same mark with a colour is drawn.
        assert!(marks.show(&support, &agent(CURSOR), Some(0xdcdcdc), rect(4, 6)));
    }

    /// A chrome icon is drawn the same way an Agent's mark is, and the two
    /// families never share a slot: an Agent whose identity spelled an icon's
    /// key would otherwise take its picture.
    #[test]
    fn a_chrome_icon_is_drawn_like_a_brand_mark() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut marks = RowMarks::default();
        marks.begin_frame();
        assert!(marks.show(
            &support,
            &Mark::Chrome(Icon::Delete),
            Some(0xff5555),
            rect(10, 3)
        ));
        let sent = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        assert!(
            sent.contains("a=T,f=100"),
            "the icon was not sent: {sent:?}"
        );

        // The same icon in another colour is a different picture: the raster
        // depends on the colour, so the terminal has to be handed it again.
        marks.begin_frame();
        assert!(marks.show(
            &support,
            &Mark::Chrome(Icon::Delete),
            Some(0x00ff00),
            rect(10, 3)
        ));
        let resent = String::from_utf8(marks.take_escapes()).expect("escapes are text");
        assert!(
            resent.contains("a=T,f=100"),
            "a recoloured icon was not re-sent: {resent:?}"
        );

        // An icon is never mistaken for an Agent, even one whose identity
        // spells the icon's own key.
        marks.begin_frame();
        assert!(marks.show(
            &support,
            &agent("cursor Delete"),
            Some(0xff5555),
            rect(10, 4)
        ));
        assert_eq!(marks.ids().len(), 3, "the two families shared a slot");
    }
}
