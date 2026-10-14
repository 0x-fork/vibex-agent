//! The Agent brand mark, drawn as a picture the terminal holds.
//!
//! A session row already names the Agent that answers for it, in the muted
//! sentence beside its title. That name is what a reader scans to tell two
//! similarly titled sessions apart, and a column of names is a column of words
//! that have to be read. The mark is the same answer without the reading.
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
//! So the mark is drawn when the terminal speaks an image protocol, and when it
//! does not, the row keeps the Agent's name, which is always known. There is no
//! half-block approximation in between: a mark that resolved to four pixels
//! would be a worse answer than the word it replaced.
//!
//! # Why the marks themselves live elsewhere
//!
//! The identity table and the mark bytes are [`vibex_brands`], shared with the
//! desktop and the phone. This module is only the terminal's half: which marks
//! are on screen this frame, where, and the escapes the run loop has to write
//! after the frame. Nothing here decides what an Agent's mark is.
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
use crate::terminal_image::ImageSupport;

/// How many cells wide one mark is.
///
/// Two cells is the narrowest box that is still square-ish under the cell
/// aspect the rest of this client assumes, and one row tall: a mark that covered
/// the echo line under it would be answering a question the row is not asking.
pub const AGENT_LOGO_COLS: u16 = 2;
/// How many cell rows one mark covers.
pub const AGENT_LOGO_ROWS: u16 = 1;

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
/// The raster is the expensive half and it depends only on the identity and the
/// colour, so it outlives the slot that used it. A reader scrolling a list of
/// Agents re-uses a handful; a bound keeps a long session from holding every
/// mark it ever saw.
const MAX_CACHED: usize = 64;

/// One placement the terminal is holding.
#[derive(Debug)]
struct Slot {
    /// The mark under that id.
    picture: PlacedPicture,
    /// The identity the picture is, and the colour it was drawn in: together
    /// they are the mark a row is asking for.
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
/// Frame-shaped: [`AgentLogos::begin_frame`] starts one, each row that wants a
/// mark calls [`AgentLogos::show`], and [`AgentLogos::settle`] takes down every
/// mark no row asked for — including all of them on a terminal that cannot hold
/// a picture, and on every frame of a page that has no rows.
#[derive(Debug, Default)]
pub struct AgentLogos {
    slots: Vec<Slot>,
    /// Marks already rastered, by identity and colour, oldest first.
    cache: Vec<(String, Option<u32>, Arc<Vec<u8>>)>,
}

impl AgentLogos {
    /// Begin a frame: no row has asked for a mark yet.
    pub fn begin_frame(&mut self) {
        for slot in &mut self.slots {
            slot.wanted = false;
        }
    }

    /// Put `identity`'s mark in `rect`.
    ///
    /// `color` is the packed sRGB a themed mark is drawn in; a colored mark
    /// ignores it. Returns whether a mark was placed: `false` when the Agent has
    /// no mark, when the terminal cannot hold a picture, or when the pool is
    /// full — the caller draws the Agent's name instead, which is not a degraded
    /// answer but the other one.
    pub fn show(
        &mut self,
        support: &ImageSupport,
        identity: &str,
        color: Option<u32>,
        rect: Rect,
    ) -> bool {
        if !support.supports_pixels() || rect.width == 0 || rect.height == 0 {
            return false;
        }
        if vibex_brands::agent_mark(identity).is_none() {
            return false;
        }
        // The same mark in the same rectangle is already on the terminal: the
        // frame asked for exactly what is there, so it sends nothing.
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.holds(identity, color, rect))
        {
            self.slots[index].wanted = true;
            return true;
        }
        let Some(prepared) = self.cached(identity, color) else {
            return false;
        };
        // A slot holding this mark somewhere else is a move, which is a few
        // dozen bytes rather than the picture. A slot already asked for by
        // another row this frame is never taken.
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.is(identity, color) && !slot.wanted)
            .or_else(|| self.slots.iter().position(|slot| slot.is(identity, color)))
        {
            let slot = &mut self.slots[index];
            slot.wanted = true;
            slot.picture.place(support, identity, prepared, rect);
            return true;
        }
        let Some(slot) = self.take_slot() else {
            return false;
        };
        slot.wanted = true;
        slot.color = color;
        slot.label = identity.to_string();
        slot.picture.place(support, identity, prepared, rect);
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
    /// `None` when the Agent has no mark or the mark cannot be rastered, which
    /// is the caller's signal to keep the name.
    fn cached(&mut self, identity: &str, color: Option<u32>) -> Option<Arc<Vec<u8>>> {
        if let Some((_, _, bytes)) = self
            .cache
            .iter()
            .find(|(label, cached_color, _)| label == identity && *cached_color == color)
        {
            return Some(bytes.clone());
        }
        let mark = vibex_brands::agent_mark(identity)?;
        let png = vibex_brands::mark_png(mark, MARK_PIXELS, color)?;
        let bytes = Arc::new(png);
        self.cache
            .push((identity.to_string(), color, bytes.clone()));
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

    fn rect(x: u16, y: u16) -> Rect {
        Rect::new(x, y, AGENT_LOGO_COLS, AGENT_LOGO_ROWS)
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
        let mut logos = AgentLogos::default();

        logos.begin_frame();
        assert!(logos.show(&support, DEEPSEEK, None, rect(4, 6)));
        let sent = String::from_utf8(logos.take_escapes()).expect("escapes are text");
        assert!(
            sent.contains("a=T,f=100"),
            "the mark was not sent: {sent:?}"
        );
        let id = logos.ids()[0];
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
        logos.begin_frame();
        logos.show(&support, DEEPSEEK, None, rect(4, 6));
        assert!(
            logos.take_escapes().is_empty(),
            "an unchanged frame sent the mark again"
        );

        // The same mark in another rectangle is a *move*, not a transmission:
        // the picture is already on the terminal.
        logos.begin_frame();
        logos.show(&support, DEEPSEEK, None, rect(8, 6));
        let moved = String::from_utf8(logos.take_escapes()).expect("escapes are text");
        assert!(moved.contains("a=p"), "the mark was not moved: {moved:?}");
        assert!(
            !moved.contains("a=T,f=100"),
            "moving a mark re-sent the picture: {moved:?}"
        );
    }

    /// A frame that stops asking takes its mark down, and only under its own id.
    #[test]
    fn a_mark_nobody_wants_is_taken_down() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut logos = AgentLogos::default();
        logos.begin_frame();
        logos.show(&support, DEEPSEEK, None, rect(4, 6));
        let id = logos.ids()[0];
        logos.take_escapes();

        logos.begin_frame();
        logos.settle(&support);
        let cleared = String::from_utf8(logos.take_escapes()).expect("escapes are text");
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
        assert!(!logos.is_holding());
    }

    /// Two rows showing two Agents hold two marks at once, and neither is lost
    /// when the other is taken down — the reason a pool exists at all.
    #[test]
    fn two_agents_hold_two_marks() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut logos = AgentLogos::default();
        logos.begin_frame();
        assert!(logos.show(&support, DEEPSEEK, None, rect(4, 6)));
        assert!(logos.show(&support, CURSOR, Some(0xffffff), rect(4, 7)));
        assert_eq!(logos.ids().len(), 2);
        let ids = logos.ids();
        assert_ne!(ids[0], ids[1], "two marks shared one image id");
        logos.take_escapes();

        // Only the second is wanted, so only the second survives.
        logos.begin_frame();
        logos.show(&support, CURSOR, Some(0xffffff), rect(4, 7));
        logos.settle(&support);
        assert!(logos.is_holding());
        let cleared = String::from_utf8(logos.take_escapes()).expect("escapes are text");
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
        let mut logos = AgentLogos::default();
        logos.begin_frame();
        assert!(
            !logos.show(&kitty, "custom acp", None, rect(4, 6)),
            "an Agent with no mark must be refused"
        );
        assert!(logos.take_escapes().is_empty());

        let none = ImageSupport::forced(ImageProtocol::None);
        assert!(
            !logos.show(&none, DEEPSEEK, None, rect(4, 6)),
            "a terminal without pixels must be refused"
        );
        assert!(logos.take_escapes().is_empty());
        assert!(!logos.is_holding());
    }

    /// A themed mark needs a foreground colour; without one it is refused
    /// rather than drawn black, and the caller keeps the name.
    #[test]
    fn a_themed_mark_without_a_colour_is_refused() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut logos = AgentLogos::default();
        logos.begin_frame();
        assert!(!logos.show(&support, CURSOR, None, rect(4, 6)));
        // The same mark with a colour is drawn.
        assert!(logos.show(&support, CURSOR, Some(0xdcdcdc), rect(4, 6)));
    }
}
