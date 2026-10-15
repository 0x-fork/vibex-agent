//! The pairing offer as a picture the terminal draws itself.
//!
//! The code this runtime mints is an ordinary QR, but a text one is drawn from
//! half blocks, two module rows to a text row, and the offer is a link of about
//! a kilobyte — thirteen granted permissions in it — which lands near 173
//! columns by 87 rows. That is not a terminal anyone has, so the row that says
//! "Pair with a QR code" mostly answers "this terminal is too small".
//!
//! The code is not the problem: a terminal that speaks an image protocol
//! ([`crate::terminal_image`]) draws the picture at its own resolution in a
//! rectangle of cells a fraction of that size. So when the terminal can hold a
//! picture, this module hands it a PNG of the code and the panel reserves the
//! cells; when it cannot, the panel keeps the half-block code, and says why it
//! has none when the terminal is too small for that either.
//!
//! Three decisions shape it:
//!
//! * **The picture is the code, not a screenshot of one.** The modules are
//!   written at a fixed pixel size and the terminal scales the rectangle to the
//!   cells it was given, so the code is as sharp as the terminal can make it.
//! * **White is drawn, not left transparent.** A terminal's background is the
//!   reader's theme, and a code on a dark background is a code a camera reads
//!   backwards. The image carries its own quiet zone for the same reason.
//! * **The picture outlives the frame.** Like the chip preview, what the
//!   terminal is holding is remembered — which link, in which rectangle — so a
//!   frame that asks for the same thing sends nothing, and a frame that asks
//!   for nothing takes it down. It travels under an image id of its own
//!   ([`crate::terminal_image::PAIRING_IMAGE_ID`]), so the two pictures can
//!   leave the terminal independently.

use std::sync::Arc;

use ratatui::layout::Rect;

use crate::preview::PlacedPicture;
use crate::terminal_image::ImageSupport;

/// How many pixels one QR module is drawn at.
///
/// The terminal scales the picture into the cells it was placed in, so this is
/// the resolution of the source rather than the size on screen: enough that a
/// downscale keeps the module edges, and small enough that the PNG stays a few
/// kilobytes on its way to the terminal.
const MODULE_PIXELS: u32 = 4;

/// The quiet zone the standard asks for, in modules.
const QUIET_ZONE: u32 = 4;

/// The QR for `url` as a PNG, or `None` when no code can be built for it.
fn png(url: &str) -> Option<Arc<Vec<u8>>> {
    use image::ImageEncoder;

    let code =
        qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L).ok()?;
    let modules = u32::try_from(code.width()).ok()?;
    let side = (modules + 2 * QUIET_ZONE).checked_mul(MODULE_PIXELS)?;
    // White first: the quiet zone and the light modules are the paper, and a
    // code drawn on the terminal's own background is not a code.
    let mut pixels = vec![255u8; usize::try_from(side.checked_mul(side)?).ok()?];
    for (index, module) in code.to_colors().iter().enumerate() {
        if *module != qrcode::Color::Dark {
            continue;
        }
        let index = u32::try_from(index).ok()?;
        let column = index % modules;
        let row = index / modules;
        for y in 0..MODULE_PIXELS {
            for x in 0..MODULE_PIXELS {
                let px = (QUIET_ZONE + column) * MODULE_PIXELS + x;
                let py = (QUIET_ZONE + row) * MODULE_PIXELS + y;
                pixels[usize::try_from(py.checked_mul(side)?.checked_add(px)?).ok()?] = 0;
            }
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, side, side, image::ExtendedColorType::L8)
        .ok()?;
    Some(Arc::new(png))
}

/// The code the pairing page put on the terminal.
///
/// The panel that draws the QR row is the only thing that asks for it, and it
/// asks once per frame it is open. Everything else — the other half of the
/// page, every other page, a terminal that cannot hold a picture — is a frame
/// that does not ask, and [`PairingQr::settle`] takes the picture down for it.
#[derive(Debug)]
pub struct PairingQr {
    picture: PlacedPicture,
    /// The link the prepared bytes belong to, so a redraw re-encodes nothing.
    url: Option<String>,
    bytes: Option<Arc<Vec<u8>>>,
    /// Whether this frame asked for the picture.
    wanted: bool,
}

impl Default for PairingQr {
    fn default() -> Self {
        Self {
            // An image id of its own: a chip with a picture under the caret can
            // be on the terminal in the frame this page's code arrives in, and
            // one picture's clear must not delete the other's.
            picture: PlacedPicture::with_id(crate::terminal_image::PAIRING_IMAGE_ID),
            url: None,
            bytes: None,
            wanted: false,
        }
    }
}

impl PairingQr {
    /// Begin a frame: nothing has asked for the picture yet.
    pub fn begin_frame(&mut self) {
        self.wanted = false;
    }

    /// Put the code for `url` in `rect`.
    ///
    /// The same link in the same rectangle sends nothing, so this is cheap on
    /// the frames that change nothing; a frame that never calls it takes the
    /// picture down at [`PairingQr::settle`].
    pub fn show(&mut self, support: &ImageSupport, url: &str, rect: Rect) {
        self.wanted = true;
        if !support.supports_pixels() || rect.width == 0 || rect.height == 0 {
            return;
        }
        if self.picture.holds(url, rect) {
            return;
        }
        let Some(bytes) = self.bytes_for(url) else {
            // A link no code can be built for is not a picture: the panel says
            // so in words, and the terminal is left holding nothing.
            self.picture.clear(support);
            return;
        };
        self.picture.place(support, url, bytes, rect);
    }

    /// Take the picture down unless this frame asked for it.
    pub fn settle(&mut self, support: &ImageSupport) {
        if !self.wanted {
            self.picture.clear(support);
        }
    }

    /// Take the escapes this frame queued, leaving none behind.
    pub fn take_escapes(&mut self) -> Vec<u8> {
        self.picture.take_escapes()
    }

    /// Whether the terminal is holding a code this client put there.
    pub fn is_holding(&self) -> bool {
        self.picture.is_holding()
    }

    /// Forget the code: the terminal has already dropped it.
    ///
    /// The same door [`crate::row_marks::RowMarks::forget`] is for, and for the
    /// same reason: a resize or a handover clears the screen, a clear takes the
    /// picture with it, and the record this client keeps is what would stop the
    /// next frame from sending the code again. The encoded PNG stays: the link
    /// has not changed, only the terminal's hold on it.
    pub fn forget(&mut self) {
        self.picture.forget();
        self.wanted = false;
    }

    /// The PNG for `url`, encoded once per link rather than once per frame.
    fn bytes_for(&mut self, url: &str) -> Option<Arc<Vec<u8>>> {
        if self.url.as_deref() == Some(url)
            && let Some(bytes) = &self.bytes
        {
            return Some(bytes.clone());
        }
        let bytes = png(url)?;
        self.url = Some(url.to_string());
        self.bytes = Some(bytes.clone());
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_image::{ImageProtocol, PAIRING_IMAGE_ID, PREVIEW_IMAGE_ID};

    /// A link shaped like the one the runtime mints: long enough that the text
    /// renderer cannot hold it on any ordinary terminal.
    const URL: &str = "vibex://open/direct#/pair/eyJvZmZlcklkIjoiMDFKOUYwMDAwMDAwMDAwMDAwMDAwMDAwMDAiLCJwZXJtaXNzaW9ucyI6WyJkZXZpY2UucmVhZCIsImRldmljZS5wYWlyIiwiZGV2aWNlLnJldm9rZSIsInNlc3Npb24ucmVhZCIsInNlc3Npb24ud3JpdGUiLCJzZXNzaW9uLmZvcmsiLCJ0dXJuLnNlbmQiLCJ0dXJuLmludGVycnVwdCIsImFwcHJvdmFsLnJlYWQiLCJhcHByb3ZhbC5hbnN3ZXIiLCJ0aW1lbGluZS5yZWFkIiwid29ya3NwYWNlLnJlYWQiLCJ3b3Jrc3BhY2Uud3JpdGUiXX0";

    fn rect() -> Rect {
        Rect::new(4, 6, 54, 27)
    }

    #[test]
    fn the_code_is_a_png_with_a_quiet_zone_on_both_sides() {
        let bytes = png(URL).expect("the link is a code");
        assert!(
            crate::terminal_image::is_png(&bytes),
            "the terminal is handed something that is not a PNG"
        );
        let decoded = image::load_from_memory(&bytes).expect("the PNG decodes");
        let side = decoded.width();
        assert_eq!(side, decoded.height(), "a QR code is square");
        assert!(
            side.is_multiple_of(MODULE_PIXELS),
            "the modules are whole pixels"
        );
        // The quiet zone is white, and the code inside it is not all white:
        // both ends of the range are drawn, which is what a camera needs.
        let luma = decoded.to_luma8();
        assert_eq!(
            luma.get_pixel(0, 0).0[0],
            255,
            "the quiet zone is not white"
        );
        assert!(
            luma.pixels().any(|pixel| pixel.0[0] == 0),
            "no dark module was drawn"
        );
    }

    #[test]
    fn a_link_no_code_can_hold_is_no_picture() {
        // The QR standard stops at 2953 bytes for this error correction level,
        // and the panel falls back to words rather than to a broken code.
        assert!(png(&"x".repeat(4_000)).is_none());
    }

    #[test]
    fn a_placed_code_is_sent_once_and_taken_down_when_it_is_not_asked_for() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut qr = PairingQr::default();

        // The frame that opens the row sends the picture.
        qr.begin_frame();
        qr.show(&support, URL, rect());
        let sent = String::from_utf8(qr.take_escapes()).expect("escapes are text");
        assert!(
            sent.contains("a=T,f=100"),
            "the code was not sent: {sent:?}"
        );
        assert!(
            sent.contains(&format!("i={PAIRING_IMAGE_ID}")),
            "the code did not travel under its own image id: {sent:?}"
        );
        assert!(qr.is_holding());

        // The frame after it, showing the same code in the same rectangle,
        // sends nothing: the terminal already has it.
        qr.begin_frame();
        qr.show(&support, URL, rect());
        assert!(
            qr.take_escapes().is_empty(),
            "an unchanged frame sent the code again"
        );

        // A frame that does not ask — the panel closed, the tab changed, the
        // reader left the page — takes it down, and only what it put there.
        qr.begin_frame();
        qr.settle(&support);
        let cleared = String::from_utf8(qr.take_escapes()).expect("escapes are text");
        assert!(
            cleared.contains(&format!("a=d,d=i,i={PAIRING_IMAGE_ID}")),
            "the code was not deleted under its own id: {cleared:?}"
        );
        assert!(
            !cleared.contains(&format!("i={PREVIEW_IMAGE_ID}")),
            "clearing the code named the preview's picture: {cleared:?}"
        );
        assert!(!qr.is_holding());

        // And nothing left to take down is nothing sent.
        qr.begin_frame();
        qr.settle(&support);
        assert!(qr.take_escapes().is_empty());
    }

    #[test]
    fn a_terminal_without_pixels_is_never_sent_a_picture() {
        let support = ImageSupport::forced(ImageProtocol::None);
        let mut qr = PairingQr::default();
        qr.begin_frame();
        qr.show(&support, URL, rect());
        assert!(qr.take_escapes().is_empty());
        assert!(!qr.is_holding());
    }
}
