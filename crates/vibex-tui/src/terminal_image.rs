//! The picture a terminal draws for itself.
//!
//! A half-block raster is a character grid's idea of a picture: two pixels to
//! a cell and no more detail than the box has cells, which is why a screenshot
//! previewed in one reads as a shape rather than as a screenshot. A terminal
//! that speaks an image protocol can do better — it has the pixels, and it
//! knows its own cell geometry and its own backing scale — so the panel hands
//! it the picture and lets it draw a real one.
//!
//! Two protocols are spoken here, chosen from the terminal the environment
//! identifies ([`crate::notify::TerminalProfile`] owns that table):
//!
//! | protocol | terminals | sequence |
//! | --- | --- | --- |
//! | [`ImageProtocol::Kitty`] | kitty, WezTerm, Ghostty | `APC G` |
//! | [`ImageProtocol::Iterm2`] | iTerm2 | `OSC 1337;File` |
//!
//! Neither is probed for, for the reason the notification layer does not probe
//! either: a query puts its answer in the input queue the interface is reading,
//! and a terminal that does not know a sequence ignores it — the raster
//! underneath is what the reader sees when one is ignored. A reader whose
//! terminal is unrecognised, or is recognised wrongly, names the protocol in
//! `VIBEX_TUI_IMAGE` (`kitty`, `iterm2`, or `off`).
//!
//! # What is where
//!
//! Every sequence is a pure function of the picture, the protocol and a
//! rectangle: nothing here draws, reads the terminal or holds state. The
//! panel decides *what* to show and *where* ([`crate::preview`]); this module
//! turns that into bytes; the run loop writes them after the frame, which is
//! the one moment a control sequence can be written without landing in the
//! middle of a row.

use std::sync::Arc;

use crate::notify::{Multiplexer, TerminalProfile};

/// The image protocols a terminal may speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageProtocol {
    /// The kitty graphics protocol: kitty, WezTerm, Ghostty and Warp.
    Kitty,
    /// iTerm2's inline images, which WezTerm also understands.
    Iterm2,
    /// No pixels: the panel's own raster is all there is.
    #[default]
    None,
}

impl ImageProtocol {
    /// The spelling [`ImageProtocol::from_id`] reads, and the one the reader
    /// writes in `VIBEX_TUI_IMAGE`.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Iterm2 => "iterm2",
            Self::None => "off",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "kitty" => Some(Self::Kitty),
            "iterm2" | "iterm" => Some(Self::Iterm2),
            "off" | "none" | "no" | "half" => Some(Self::None),
            _ => None,
        }
    }

    /// Whether the terminal draws a picture the client hands it.
    pub const fn supports_pixels(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// What this terminal can draw, and how the bytes reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSupport {
    /// The protocol to use, once the environment and the reader have spoken.
    pub protocol: ImageProtocol,
    /// The multiplexer the sequences travel through, when there is one.
    pub multiplexer: Option<Multiplexer>,
}

impl Default for ImageSupport {
    fn default() -> Self {
        Self {
            protocol: ImageProtocol::None,
            multiplexer: None,
        }
    }
}

impl ImageSupport {
    /// Read the process environment.
    pub fn detect() -> Self {
        Self::detect_from(|key| std::env::var(key).ok())
    }

    /// Read the support from a lookup, so the table is a test and not a
    /// comment. The lookup is called at most once per variable.
    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let named = lookup("VIBEX_TUI_IMAGE").filter(|value| !value.trim().is_empty());
        let profile = TerminalProfile::detect_from(lookup);
        let mut support = Self::from_profile(&profile);
        // The reader's own answer is the last word: detection is a guess about
        // the terminal, and the reader is the one looking at it.
        if let Some(named) = named.as_deref().and_then(ImageProtocol::from_id) {
            support.protocol = named;
        }
        support
    }

    /// What a terminal the environment identified can draw.
    pub fn from_profile(profile: &TerminalProfile) -> Self {
        // A Windows console is reached through ConPTY, which strips the APC and
        // OSC a picture travels in before the host terminal ever sees them.
        let protocol = if cfg!(windows) {
            ImageProtocol::None
        } else {
            match profile.terminal.as_deref() {
                Some("kitty") | Some("WezTerm") | Some("Ghostty") => ImageProtocol::Kitty,
                Some("iTerm2") => ImageProtocol::Iterm2,
                _ => ImageProtocol::None,
            }
        };
        Self {
            protocol,
            multiplexer: profile.multiplexer,
        }
    }

    /// Force a protocol, for a caller that knows better than the environment
    /// (a test) or for the settings surface to read back.
    pub fn forced(protocol: ImageProtocol) -> Self {
        Self {
            protocol,
            multiplexer: None,
        }
    }

    /// Whether the terminal draws pictures at all.
    pub const fn supports_pixels(&self) -> bool {
        self.protocol.supports_pixels()
    }

    /// `bytes` in the form this terminal's protocol carries them, or `None`
    /// when there is no protocol to carry them.
    ///
    /// The conversion is the expensive half of showing a picture that is not
    /// already in the protocol's format, so it happens once, when the picture
    /// is attached — never on the frame it is drawn.
    pub fn prepare(&self, bytes: &[u8]) -> Option<Arc<Vec<u8>>> {
        match self.protocol {
            // `f=100` is PNG and only PNG, so anything else is decoded and
            // re-encoded here rather than sent as raw pixels, which would be
            // an order of magnitude more bytes for the same picture.
            ImageProtocol::Kitty => as_png(bytes).map(Arc::new),
            // The inline-image protocol names the file it carries, so what the
            // reader attached is what goes out.
            ImageProtocol::Iterm2 => Some(Arc::new(bytes.to_vec())),
            ImageProtocol::None => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// The cell rectangle a picture fills.
///
/// `x` and `y` are the cell the picture's top-left corner sits on, and the
/// terminal scales the picture to `cols` by `rows`; the caller sizes the
/// rectangle to the picture's own shape (see `preview::picture_box`), because
/// a rectangle of another shape is a picture of another shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// The image id every placement uses.
///
/// One id for the one picture the panel shows: transmitting under an id the
/// terminal already holds replaces it, which is what makes moving the caret
/// between two chips a replacement rather than a new picture on top of the
/// old one.
const PREVIEW_IMAGE_ID: u32 = 1;

/// Above the text, not below it.
///
/// The panel paints a raster into the same cells the picture is placed over,
/// and a picture under the text layer would be hidden by it: the raster is the
/// fallback, not the thing in front.
const PICTURE_LAYER: i32 = 1;

/// How much of a base64 payload one kitty escape may carry.
///
/// The protocol's own limit is 4096 bytes, and a larger chunk is dropped by
/// the terminal rather than split.
const KITTY_CHUNK_BYTES: usize = 4096;

/// The PNG magic, which is the one format kitty takes directly.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

impl ImageSupport {
    /// Transmit `bytes` and show them at `placement`, replacing the picture
    /// this client sent before.
    ///
    /// `bytes` are the prepared ones ([`ImageSupport::prepare`]), so a caller
    /// that is re-placing a picture it has already sent should use
    /// [`ImageSupport::place`] instead: this carries the whole picture.
    pub fn transmit(&self, bytes: &[u8], placement: Placement) -> Option<String> {
        if !self.supports_pixels() || placement.cols == 0 || placement.rows == 0 {
            return None;
        }
        let escape = match self.protocol {
            ImageProtocol::Kitty => {
                let mut escape = cursor_to(placement);
                escape.push_str(&kitty_transmit(bytes, placement));
                escape
            }
            ImageProtocol::Iterm2 => {
                let mut escape = cursor_to(placement);
                escape.push_str(&iterm2_transmit(bytes, placement));
                escape
            }
            ImageProtocol::None => return None,
        };
        Some(self.wrap(&escape))
    }

    /// Move the picture the terminal already holds to `placement`.
    ///
    /// Nothing is transmitted: the terminal is told where to put what it has,
    /// which is a few dozen bytes against the picture's hundreds of kilobytes.
    /// Only the kitty protocol can do it — an iTerm2 picture *is* the cells it
    /// was written into, so moving one means sending it again.
    pub fn place(&self, placement: Placement) -> Option<String> {
        if self.protocol != ImageProtocol::Kitty || placement.cols == 0 || placement.rows == 0 {
            return None;
        }
        let mut escape = cursor_to(placement);
        escape.push_str(&kitty_place(placement));
        Some(self.wrap(&escape))
    }

    /// Take the picture off the terminal.
    ///
    /// A kitty picture is a layer of its own and outlives the frame that put it
    /// there, so it has to be deleted. An iTerm2 picture lives in the cells it
    /// was written into, and the next frame paints over them anyway.
    pub fn clear(&self) -> Option<String> {
        if self.protocol != ImageProtocol::Kitty {
            return None;
        }
        Some(self.wrap(&format!("\x1b_Ga=d,d=i,i={PREVIEW_IMAGE_ID},q=2\x1b\\")))
    }

    /// Wrap a sequence for the multiplexer it has to travel through.
    ///
    /// tmux passes a sequence through only inside its own envelope and only
    /// with `allow-passthrough on`; without it the sequence is dropped, which
    /// the reader sees as the raster rather than as stray text.
    fn wrap(&self, escape: &str) -> String {
        match self.multiplexer {
            Some(Multiplexer::Tmux) => crate::notify::tmux_passthrough(escape),
            // screen has no envelope, and a sequence it does not implement is
            // ignored.
            _ => escape.to_string(),
        }
    }
}

/// Park the cursor on the placement's own cell, and put it back afterwards.
///
/// The escapes are written after the frame, whose last act is to place the
/// reader's caret; a picture that left the cursor where it drew would move it.
fn cursor_to(placement: Placement) -> String {
    format!("\x1b7\x1b[{};{}H", placement.y + 1, placement.x + 1)
}

/// The end of a sequence that saved the cursor with [`cursor_to`].
const CURSOR_BACK: &str = "\x1b8";

/// Kitty: transmit a PNG and display it, as one image (`a=T`) the id owns.
fn kitty_transmit(png: &[u8], placement: Placement) -> String {
    let header = format!(
        "a=T,f=100,t=d,q=2,C=1,z={PICTURE_LAYER},i={PREVIEW_IMAGE_ID},p={PREVIEW_IMAGE_ID},c={},r={}",
        placement.cols, placement.rows
    );
    let mut escape = kitty_chunked(png, &header);
    escape.push_str(CURSOR_BACK);
    escape
}

/// Kitty: put the image the terminal already holds at this placement.
fn kitty_place(placement: Placement) -> String {
    format!(
        "\x1b_Ga=p,i={PREVIEW_IMAGE_ID},p={PREVIEW_IMAGE_ID},c={},r={},z={PICTURE_LAYER},C=1,q=2\x1b\\{CURSOR_BACK}",
        placement.cols, placement.rows
    )
}

/// One kitty escape, split into the chunks the protocol accepts.
///
/// The first chunk carries the command and the payload; every chunk after it
/// carries `m` alone, because the terminal has already read the command.
fn kitty_chunked(bytes: &[u8], header: &str) -> String {
    let encoded = crate::terminal::encode_base64(bytes);
    let payload = encoded.as_bytes();
    let chunks = payload.chunks(KITTY_CHUNK_BYTES).count().max(1);
    let mut escape = String::with_capacity(encoded.len() + chunks * 24);
    for index in 0..chunks {
        let start = index * KITTY_CHUNK_BYTES;
        let end = (start + KITTY_CHUNK_BYTES).min(payload.len());
        let chunk = std::str::from_utf8(&payload[start..end]).unwrap_or_default();
        let more = u8::from(index + 1 < chunks);
        if index == 0 {
            escape.push_str(&format!("\x1b_G{header},m={more};{chunk}\x1b\\"));
        } else {
            escape.push_str(&format!("\x1b_Gq=2,m={more};{chunk}\x1b\\"));
        }
    }
    escape
}

/// iTerm2: one OSC carrying the file, sized to the cells it fills.
///
/// `preserveAspectRatio=0` because the caller sized the rectangle to the
/// picture's own shape: letting the terminal fit it again would letterbox a
/// box that already fits.
fn iterm2_transmit(bytes: &[u8], placement: Placement) -> String {
    let encoded = crate::terminal::encode_base64(bytes);
    format!(
        "\x1b]1337;File=inline=1;width={}cells;height={}cells;preserveAspectRatio=0:{encoded}\x07{CURSOR_BACK}",
        placement.cols, placement.rows
    )
}

/// Whether the bytes are already a PNG.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_MAGIC)
}

/// `bytes` as a PNG, or `None` when they are not a picture this client reads.
fn as_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if is_png(bytes) {
        return Some(bytes.to_vec());
    }
    let decoded = image::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    // The picture is on its way to a terminal, which has to wait for the whole
    // frame: fast beats small here, and the reader's own pause is the cost of
    // getting it wrong.
    image::ImageEncoder::write_image(
        encoder,
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
        image::ExtendedColorType::Rgba8,
    )
    .ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(terminal: Option<&str>, multiplexer: Option<Multiplexer>) -> TerminalProfile {
        TerminalProfile {
            transport: crate::notify::Transport::Title,
            multiplexer,
            terminal: terminal.map(str::to_string),
            remote: false,
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut picture = image::RgbImage::new(width, height);
        for (_, _, pixel) in picture.enumerate_pixels_mut() {
            *pixel = image::Rgb([10, 20, 30]);
        }
        let mut bytes = Vec::new();
        picture
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("the fixture encodes");
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(width, height, image::Rgb([200, 100, 50]));
        let mut bytes = Vec::new();
        picture
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .expect("the fixture encodes");
        bytes
    }

    fn placement() -> Placement {
        Placement {
            x: 3,
            y: 5,
            cols: 40,
            rows: 12,
        }
    }

    #[test]
    fn the_terminals_that_speak_a_protocol_are_the_ones_that_get_one() {
        for (name, expected) in [
            (Some("kitty"), ImageProtocol::Kitty),
            (Some("WezTerm"), ImageProtocol::Kitty),
            (Some("Ghostty"), ImageProtocol::Kitty),
            (Some("iTerm2"), ImageProtocol::Iterm2),
            (Some("Windows Terminal"), ImageProtocol::None),
            (None, ImageProtocol::None),
        ] {
            let support = ImageSupport::from_profile(&profile(name, None));
            assert_eq!(support.protocol, expected, "{name:?}");
        }
    }

    #[test]
    fn the_readers_answer_outranks_the_environments() {
        let support = ImageSupport::detect_from(|key| match key {
            "VIBEX_TUI_IMAGE" => Some("off".to_string()),
            "TERM" => Some("xterm-kitty".to_string()),
            "KITTY_WINDOW_ID" => Some("1".to_string()),
            _ => None,
        });
        assert_eq!(support.protocol, ImageProtocol::None);

        let forced = ImageSupport::detect_from(|key| match key {
            "VIBEX_TUI_IMAGE" => Some("iTerm2".to_string()),
            "TERM" => Some("xterm-256color".to_string()),
            _ => None,
        });
        assert_eq!(forced.protocol, ImageProtocol::Iterm2);

        // A spelling nobody knows is not an instruction to turn pixels off:
        // detection still answers.
        let unknown = ImageSupport::detect_from(|key| match key {
            "VIBEX_TUI_IMAGE" => Some("sixel".to_string()),
            "TERM" => Some("xterm-kitty".to_string()),
            _ => None,
        });
        assert_eq!(unknown.protocol, ImageProtocol::Kitty);
    }

    #[test]
    fn a_multiplexer_is_remembered_because_the_sequence_has_to_travel_through_it() {
        let support = ImageSupport::detect_from(|key| match key {
            "TMUX" => Some("/tmp/tmux-1000/default,1,0".to_string()),
            "TERM" => Some("xterm-kitty".to_string()),
            _ => None,
        });
        assert_eq!(support.multiplexer, Some(Multiplexer::Tmux));

        let escape = support
            .transmit(&png(4, 4), placement())
            .expect("a sequence");
        assert!(
            escape.starts_with("\x1bPtmux;"),
            "the sequence is not wrapped for tmux: {escape:?}"
        );
    }

    /// A picture with enough entropy that its PNG is over one chunk.
    fn noisy_png(size: u32) -> Vec<u8> {
        let mut picture = image::RgbImage::new(size, size);
        for (x, y, pixel) in picture.enumerate_pixels_mut() {
            let seed = x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503));
            *pixel = image::Rgb([(seed >> 8) as u8, (seed >> 16) as u8, (seed >> 24) as u8]);
        }
        let mut bytes = Vec::new();
        picture
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("the fixture encodes");
        bytes
    }

    /// Every `APC G` sequence in `escape`, as `(header, payload)`.
    fn kitty_chunks(escape: &str) -> Vec<(String, String)> {
        escape
            .split("\x1b_G")
            .skip(1)
            .filter_map(|part| {
                let (body, _) = part.split_once("\x1b\\")?;
                let (header, payload) = body.split_once(';')?;
                Some((header.to_string(), payload.to_string()))
            })
            .collect()
    }

    #[test]
    fn a_kitty_picture_is_transmitted_in_chunks_the_protocol_accepts() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let picture = noisy_png(96);
        let escape = support.transmit(&picture, placement()).expect("a sequence");
        assert!(
            escape.contains("a=T,f=100,t=d,q=2,C=1,z=1,i=1,p=1,c=40,r=12"),
            "the command is not the one that displays a png: {escape:?}"
        );
        assert!(
            escape.starts_with("\x1b7\x1b[6;4H"),
            "the picture is not placed on its own cell: {escape:?}"
        );
        assert!(escape.ends_with("\x1b8"), "the cursor is not put back");

        // The protocol reads at most 4096 payload bytes per escape and drops a
        // longer one, so a picture of any size has to arrive in pieces: every
        // piece but the last says there is more, the first carries the command,
        // and the ones after it carry nothing but their own length.
        let chunks = kitty_chunks(&escape);
        assert!(chunks.len() > 1, "a 96x96 noise png is more than one chunk");
        for (index, (header, payload)) in chunks.iter().enumerate() {
            assert!(
                payload.len() <= KITTY_CHUNK_BYTES,
                "chunk {index} carries {} bytes and would be dropped",
                payload.len()
            );
            let last = index + 1 == chunks.len();
            assert_eq!(
                header.contains("m=1"),
                !last,
                "chunk {index} of {} has the wrong more-flag: {header:?}",
                chunks.len()
            );
            if index > 0 {
                assert!(
                    header.starts_with("q=2,m="),
                    "a continuation chunk carries more than its length: {header:?}"
                );
            }
        }
    }

    #[test]
    fn moving_a_kitty_picture_transmits_nothing() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let escape = support.place(placement()).expect("a sequence");
        assert!(escape.contains("a=p,i=1,p=1,c=40,r=12,z=1"), "{escape:?}");
        assert!(
            escape.len() < 200,
            "a placement carried {} bytes of picture",
            escape.len()
        );

        // iTerm2 has no such thing: its picture is the cells it was written
        // into, so the caller has to send it again.
        let iterm = ImageSupport::forced(ImageProtocol::Iterm2);
        assert!(iterm.place(placement()).is_none());
        assert!(iterm.clear().is_none());
    }

    #[test]
    fn a_picture_that_is_taken_down_is_deleted_by_id() {
        let support = ImageSupport::forced(ImageProtocol::Kitty);
        assert_eq!(
            support.clear().as_deref(),
            Some("\x1b_Ga=d,d=i,i=1,q=2\x1b\\")
        );
    }

    #[test]
    fn iterm2_carries_the_file_it_was_given() {
        let support = ImageSupport::forced(ImageProtocol::Iterm2);
        let picture = png(8, 8);
        let escape = support.transmit(&picture, placement()).expect("a sequence");
        assert!(
            escape.contains(
                "\x1b]1337;File=inline=1;width=40cells;height=12cells;preserveAspectRatio=0:"
            ),
            "{escape:?}"
        );
        assert!(escape.ends_with("\x07\x1b8"), "{escape:?}");
    }

    #[test]
    fn only_png_reaches_kitty_as_png() {
        let kitty = ImageSupport::forced(ImageProtocol::Kitty);
        let png = png(16, 16);
        let prepared = kitty.prepare(&png).expect("a png is ready as it is");
        assert_eq!(prepared.as_slice(), png.as_slice());

        // A JPEG is decoded and re-encoded: `f=100` takes PNG and nothing else.
        let prepared = kitty.prepare(&jpeg(16, 16)).expect("a jpeg converts");
        assert!(is_png(&prepared), "the converted picture is not a png");
        let decoded = image::load_from_memory(&prepared).expect("it decodes");
        assert_eq!((decoded.width(), decoded.height()), (16, 16));

        // iTerm2 names the file it carries, so nothing is converted.
        let iterm = ImageSupport::forced(ImageProtocol::Iterm2);
        let jpeg = jpeg(4, 4);
        assert_eq!(
            iterm.prepare(&jpeg).expect("the bytes travel").as_slice(),
            jpeg.as_slice()
        );

        // And a terminal with no protocol prepares nothing, because there is
        // nothing it could do with the bytes but decode them for the raster.
        let none = ImageSupport::forced(ImageProtocol::None);
        assert!(none.prepare(&png).is_none());
        assert!(none.transmit(&png, placement()).is_none());
    }

    #[test]
    fn bytes_that_are_not_a_picture_are_not_prepared() {
        let kitty = ImageSupport::forced(ImageProtocol::Kitty);
        assert!(kitty.prepare(b"not a picture").is_none());
    }
}
