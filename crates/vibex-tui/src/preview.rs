//! What a collapsed block holds, drawn before it is opened.
//!
//! The composer keeps a pasted paragraph and an attached picture one label
//! wide each — `[Pasted: 40 lines]`, `[Image #1]` — and that is what keeps a
//! draft readable. It is also the one thing the reader cannot check: a label
//! says how much was pasted, never what. So while the caret stands on a chip,
//! the rows above the composer explain it: the head and tail of a paste with
//! the middle elided, and the picture itself as a half-block raster with what
//! is known about it underneath.
//!
//! Nothing here opens anything. The panel is a reading surface — it appears
//! under the caret and leaves with it — and the one action it names, `Enter`
//! on a paste, is performed by the composer. A preview that could be entered
//! would be a second place a draft can be edited from.
//!
//! Two decisions shape the module:
//!
//! * **The panel is anchored, not centred.** It grows upward from the top of
//!   whatever the composer stack is drawing, so it never covers the draft it
//!   is explaining, and its bottom edge is in the same place whatever it is
//!   showing.
//! * **The pictures are already decoded.** A thumbnail is built when an image
//!   is attached (see [`thumbnail`]) because the panel is drawn on every frame
//!   the caret rests on the chip, and decoding a screenshot is not a thing to
//!   do at that rate. What is left here is drawing.
//! * **The raster is a floor, not a picture.** A terminal that speaks an image
//!   protocol is handed the picture itself ([`crate::terminal_image`]) and
//!   draws it in the panel's rectangle, at its own resolution; the raster is
//!   painted under it for the terminal that does not, or that drops a sequence
//!   it never received. A picture is then sharp wherever it can be, and a
//!   picture rather than a hole wherever it cannot.

use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Focus};
use crate::composer::{ChipPeek, ChipPeekPayload, ImageAttachment, ImageThumbnail};
use crate::glyphs::{self, GlyphTier};
use crate::locale::Strings;
use crate::modal::border_block;
use crate::text::truncate_to_width;
use crate::theme::{ColorMode, TuiTheme};

/// The lines a paste preview shows at each end before eliding the middle.
pub const PASTE_PREVIEW_LINES: usize = 3;

/// The cell box a picture is decoded into.
///
/// It is the largest raster the panel is ever asked to draw, so one decode
/// when the image is attached covers every frame it is previewed on — and a
/// smaller panel only ever samples what is already there.
pub const THUMBNAIL_COLS: u16 = 160;
/// How many cell rows the decoded box holds. See [`THUMBNAIL_COLS`].
pub const THUMBNAIL_ROWS: u16 = 24;

/// How much taller a terminal cell is than it is wide.
///
/// A cell rectangle is not a pixel rectangle, and a picture has to be scaled
/// into one: this is the ratio that turns a picture's shape into the cells it
/// needs. The terminal knows its own geometry and the client cannot ask, so
/// both the raster and the placement are sized from this one assumption.
const CELL_ASPECT: f32 = 2.0;

/// The width the panel asks for, as a share of the room above the composer.
const PANEL_WIDTH_SHARE_NUMERATOR: u16 = 3;
const PANEL_WIDTH_SHARE_DENOMINATOR: u16 = 4;
/// The narrowest panel worth drawing a border around.
const PANEL_MIN_WIDTH: u16 = 28;
/// The shortest room above the composer the panel is drawn in at all.
const PANEL_MIN_AREA_HEIGHT: u16 = 5;

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decode `bytes` into the raster an attachment carries.
///
/// The picture is scaled to fit [`THUMBNAIL_COLS`] by `2 *`[`THUMBNAIL_ROWS`]
/// pixels, which is the pixel box the cell box stands for, so the aspect ratio
/// survives the trip through a character grid. `None` when the bytes are not a
/// picture this client reads: an attachment whose preview says so is still an
/// attachment the Agent will be shown.
pub fn thumbnail(bytes: &[u8]) -> Option<ImageThumbnail> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return None;
    }
    let scaled = decoded.thumbnail(u32::from(THUMBNAIL_COLS), u32::from(THUMBNAIL_ROWS) * 2);
    let cols = u16::try_from(scaled.width()).ok()?.min(THUMBNAIL_COLS);
    let rows = u16::try_from(scaled.height().div_ceil(2))
        .ok()?
        .min(THUMBNAIL_ROWS);
    if cols == 0 || rows == 0 {
        return None;
    }
    let pixels = scaled.to_rgb8();
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for row in 0..rows {
        for column in 0..cols {
            // The lower half of the last cell repeats the upper one when the
            // scaled picture has an odd number of pixel rows, rather than
            // reaching past the end of it.
            let top = pixel(&pixels, column, u32::from(row) * 2);
            let bottom = pixel(
                &pixels,
                column,
                (u32::from(row) * 2 + 1).min(scaled.height() - 1),
            );
            cells.push([top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]]);
        }
    }
    Some(ImageThumbnail {
        width,
        height,
        cols,
        rows,
        cells,
    })
}

/// One pixel as RGB, or black when the coordinate is outside the picture.
fn pixel(pixels: &image::RgbImage, column: u16, row: u32) -> [u8; 3] {
    pixels
        .get_pixel_checked(u32::from(column), row)
        .map(|pixel| pixel.0)
        .unwrap_or([0, 0, 0])
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// The picture the panel last put on the terminal, and the escapes the next
/// frame owes it.
///
/// The panel is redrawn every frame, but a picture is not cheap to send: at a
/// few hundred kilobytes it is orders of magnitude more than a frame, and the
/// caret can rest on a chip for minutes. So the frame paints the box and the
/// caption and this remembers what the terminal is holding — which picture, in
/// which rectangle — and the escapes the run loop has to write for the two to
/// agree again.
#[derive(Debug)]
pub struct PlacedPicture {
    /// The image id the terminal holds this picture under. Two surfaces that
    /// can be on screen at once need two ids, or one's clear deletes the
    /// other's picture. See [`crate::terminal_image::PAIRING_IMAGE_ID`].
    id: u32,
    /// The picture the terminal holds, by the label of the chip it belongs to.
    label: Option<String>,
    /// The cell rectangle it was placed in.
    rect: Option<Rect>,
    /// The bytes that were sent for it, so a placement move does not have to
    /// read the file again or encode a format the terminal cannot take.
    bytes: Option<Arc<Vec<u8>>>,
    /// Escapes this frame queued, for the run loop to write after it.
    pending: Vec<u8>,
}

impl Default for PlacedPicture {
    fn default() -> Self {
        Self {
            id: crate::terminal_image::PREVIEW_IMAGE_ID,
            label: None,
            rect: None,
            bytes: None,
            pending: Vec::new(),
        }
    }
}

impl PlacedPicture {
    /// A picture under an image id of its own, for a surface that can share a
    /// frame with the chip preview's.
    pub fn with_id(id: u32) -> Self {
        Self {
            id,
            ..Self::default()
        }
    }

    /// Take the escapes the frame queued, leaving none behind.
    pub fn take_escapes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    /// Whether the terminal is holding a picture this client put there.
    pub fn is_holding(&self) -> bool {
        self.label.is_some()
    }

    /// Whether the terminal is already holding this picture in this rectangle.
    pub fn holds(&self, label: &str, rect: Rect) -> bool {
        self.label.as_deref() == Some(label) && self.rect == Some(rect)
    }

    /// Put `bytes` at `rect`, under `label`.
    ///
    /// The same picture in the same rectangle sends nothing; the same picture
    /// in another rectangle is a placement, which is a few dozen bytes; and
    /// anything else is the picture itself, which also replaces whatever the
    /// terminal was holding under this client's id.
    pub fn place(
        &mut self,
        support: &crate::terminal_image::ImageSupport,
        label: &str,
        bytes: Arc<Vec<u8>>,
        rect: Rect,
    ) {
        if !support.supports_pixels() || rect.width == 0 || rect.height == 0 {
            self.clear(support);
            return;
        }
        let placement = crate::terminal_image::Placement {
            x: rect.x,
            y: rect.y,
            cols: rect.width,
            rows: rect.height,
        };
        if self.label.as_deref() == Some(label)
            && let Some(escape) = support.place_as(self.id, placement)
        {
            self.pending.extend_from_slice(escape.as_bytes());
            self.rect = Some(rect);
            self.bytes = Some(bytes);
            return;
        }
        if let Some(escape) = support.transmit_as(self.id, &bytes, placement) {
            self.pending.extend_from_slice(escape.as_bytes());
            self.label = Some(label.to_string());
            self.rect = Some(rect);
            self.bytes = Some(bytes);
        }
    }

    /// Take the picture off the terminal, if it is holding one.
    pub fn clear(&mut self, support: &crate::terminal_image::ImageSupport) {
        if let Some(escape) = self
            .label
            .is_some()
            .then(|| support.clear_as(self.id))
            .flatten()
        {
            self.pending.extend_from_slice(escape.as_bytes());
        }
        self.label = None;
        self.rect = None;
        self.bytes = None;
    }
}

/// What the panel draws with, gathered once per frame.
struct Panel<'a> {
    theme: &'a TuiTheme,
    strings: Strings,
    tier: GlyphTier,
    /// How the terminal paints colour, which is what decides whether a picture
    /// is drawn as pixels or described in words.
    mode: ColorMode,
}

/// Draw the panel for whatever chip the caret is standing on.
///
/// `area` is the room above the composer stack. Nothing is drawn without a
/// chip under the caret, without focus in the composer, or in a space too
/// small for a bordered panel to mean anything — and a picture the terminal is
/// still holding is taken down in every one of those cases, which is why this
/// runs on frames that draw nothing at all.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    let showing = app.overlay.is_none()
        && app.page.is_session_page()
        && app.focus == Focus::Composer
        && area.height >= PANEL_MIN_AREA_HEIGHT;
    let peek = if showing {
        app.composer.chip_peek()
    } else {
        None
    };
    let panel = Panel {
        theme,
        strings: app.strings,
        tier: app.glyph_tier(),
        mode: app.capability.mode,
    };
    match &peek {
        Some(peek) => match peek.payload {
            ChipPeekPayload::Paste(content) => {
                // A paste has no picture to take down, but a picture left over
                // from the chip the caret came from has to go.
                clear_picture(&app.terminal_images, &mut app.picture);
                render_paste(frame, area, peek, content, &panel);
            }
            ChipPeekPayload::Image(image) => {
                // The terminal first: whatever the frame paints below, the
                // picture it is holding has to be the one this panel is about.
                match image_panel(area, image, &panel) {
                    Some((popup, box_area)) => {
                        place_picture(
                            &app.terminal_images,
                            &mut app.picture,
                            peek.label,
                            &image.source,
                            box_area,
                        );
                        render_image(frame, popup, peek, image, box_area, &panel);
                    }
                    // No room for even a one-row box: the picture goes with it.
                    None => clear_picture(&app.terminal_images, &mut app.picture),
                }
            }
        },
        None => clear_picture(&app.terminal_images, &mut app.picture),
    }
}

/// Put the picture the panel is standing in for on the terminal itself.
///
/// Three cases, in order of what they cost: the same picture in the same
/// rectangle is already there and sends nothing; the same picture in another
/// rectangle is a placement, which is a few dozen bytes; anything else is the
/// picture itself, which also replaces whatever the terminal was holding under
/// this client's id.
fn place_picture(
    support: &crate::terminal_image::ImageSupport,
    picture: &mut PlacedPicture,
    label: &str,
    source: &crate::composer::ImageSource,
    rect: Rect,
) {
    if !support.supports_pixels() || rect.width == 0 || rect.height == 0 {
        clear_picture(support, picture);
        return;
    }
    if picture.holds(label, rect) {
        return;
    }
    let Some(bytes) = picture_bytes(support, picture, label, source) else {
        // Bytes this client cannot read: the panel says so in words, and a
        // picture of the last chip is not the answer to this one.
        clear_picture(support, picture);
        return;
    };
    picture.place(support, label, bytes, rect);
}

/// Take the picture off the terminal, if it is holding one.
fn clear_picture(support: &crate::terminal_image::ImageSupport, picture: &mut PlacedPicture) {
    picture.clear(support);
}

/// The bytes to send for one picture, prepared once and kept while it is the
/// one on screen.
///
/// A clipboard picture is already in memory, shared with the attachment; a
/// picture named by a path is read here, because the frame that first shows it
/// is the first moment the reader has asked for it. Reading it is a memory
/// copy of something the kernel is holding, and it happens once per chip
/// rather than once per frame.
fn picture_bytes(
    support: &crate::terminal_image::ImageSupport,
    picture: &mut PlacedPicture,
    label: &str,
    source: &crate::composer::ImageSource,
) -> Option<Arc<Vec<u8>>> {
    if picture.label.as_deref() == Some(label)
        && let Some(bytes) = &picture.bytes
    {
        return Some(bytes.clone());
    }
    let raw = match source {
        crate::composer::ImageSource::Bytes(bytes) => bytes.clone(),
        crate::composer::ImageSource::Path(path) => Arc::new(std::fs::read(path).ok()?),
    };
    let prepared = support.prepare(&raw)?;
    picture.bytes = Some(prepared.clone());
    Some(prepared)
}

/// The paste preview: head, elision, tail, and the key that opens it.
fn render_paste(
    frame: &mut Frame<'_>,
    area: Rect,
    peek: &ChipPeek<'_>,
    content: &str,
    panel: &Panel<'_>,
) {
    let theme = panel.theme;
    let width = panel_width(area);
    let inner_width = usize::from(width).saturating_sub(4);
    let lines = paste_lines(content, inner_width, panel);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let Some(popup) = panel_rect(area, width, height) else {
        return;
    };
    // Only the rows that fit are drawn: a terminal too short for the whole
    // preview still shows what it can of it rather than nothing.
    let visible = usize::from(popup.height.saturating_sub(2));
    let hint = if peek.on_chip {
        panel.strings.preview_expand_enter()
    } else {
        panel.strings.preview_expand_paste()
    };
    let content_area = draw_panel(frame, popup, peek.label, Some(hint), theme);
    frame.render_widget(
        Paragraph::new(lines.into_iter().take(visible).collect::<Vec<_>>()),
        content_area,
    );
}

/// The image panel's rectangle and the cell box the picture fills in it.
///
/// The panel is sized *to the picture*: a rectangle of another shape is a
/// picture of another shape once the terminal scales it, and the reader would
/// be looking at a stretched screenshot. The picture's pixel shape becomes the
/// cell shape through [`CELL_ASPECT`], and whatever the room cannot hold, the
/// picture gives up first.
fn image_panel(area: Rect, image: &ImageAttachment, panel: &Panel<'_>) -> Option<(Rect, Rect)> {
    let meta_rows = 1 + u16::from(image.thumbnail.is_none());
    let room = Rect {
        width: panel_width(area).saturating_sub(4),
        height: area.height.saturating_sub(2 + meta_rows),
        ..area
    };
    let (width, height) = match &image.thumbnail {
        Some(thumbnail) => picture_box(thumbnail.width, thumbnail.height, room),
        // Nothing was decoded, so nothing is known about its shape and the
        // panel is as wide as the sentence that says so.
        None => (
            content_width(&image_caption(image, usize::MAX, panel.theme))
                .max(content_width(panel.strings.preview_unavailable()))
                .saturating_add(2)
                .min(room.width.max(1)),
            1,
        ),
    };
    let popup = panel_rect(area, width.saturating_add(4), height + meta_rows + 2)?;
    let content = Rect {
        x: popup.x + 2,
        y: popup.y + 1,
        width: popup.width.saturating_sub(4),
        height: popup.height.saturating_sub(2),
    };
    let raster = Rect {
        height: content.height.saturating_sub(meta_rows),
        ..content
    };
    Some((popup, raster))
}

/// How many cells a sentence needs, in the display width the terminal counts.
fn content_width(text: &str) -> u16 {
    u16::try_from(crate::text::display_width(text)).unwrap_or(u16::MAX)
}

/// The cell box a picture of `width` by `height` pixels fills in `room`.
fn picture_box(width: u32, height: u32, room: Rect) -> (u16, u16) {
    if width == 0 || height == 0 || room.width == 0 || room.height == 0 {
        return (1, 1);
    }
    let max_cols = room.width.clamp(1, THUMBNAIL_COLS);
    let max_rows = room.height.clamp(1, THUMBNAIL_ROWS);
    // Start from the full height, because a picture is usually wider than the
    // room is tall, and give back what the width cannot hold.
    let rows = max_rows;
    let cols = ((f32::from(rows) * CELL_ASPECT * width as f32 / height as f32).round() as u16)
        .clamp(1, max_cols);
    let rows = ((f32::from(cols) * height as f32 / (CELL_ASPECT * width as f32)).round() as u16)
        .clamp(1, max_rows);
    (cols, rows)
}

/// The image preview: the picture the terminal draws, and what is known about
/// it underneath.
///
/// `box_area` is the cell rectangle the picture occupies; when the terminal
/// was handed the picture, that is where it is already drawn, and the raster
/// under it is for the terminal that never received it.
fn render_image(
    frame: &mut Frame<'_>,
    popup: Rect,
    peek: &ChipPeek<'_>,
    image: &ImageAttachment,
    box_area: Rect,
    panel: &Panel<'_>,
) {
    let theme = panel.theme;
    if popup.height < 3 || popup.width < 4 {
        return;
    }
    let content_area = draw_panel(frame, popup, peek.label, None, theme);
    let meta_rows = 1 + u16::from(image.thumbnail.is_none());
    if box_area.width > 0
        && box_area.height > 0
        && panel.mode == ColorMode::TrueColor
        && let Some(thumbnail) = &image.thumbnail
    {
        draw_raster(frame, box_area, thumbnail, panel.tier);
    }
    // The footer is the last row of the panel, under whatever was drawn.
    let footer = Rect {
        y: content_area.bottom().saturating_sub(meta_rows),
        height: meta_rows,
        ..content_area
    };
    let mut lines = vec![Line::from(Span::styled(
        image_caption(image, usize::from(content_area.width), theme),
        theme.muted(),
    ))];
    if image.thumbnail.is_none() {
        lines.push(Line::from(Span::styled(
            panel.strings.preview_unavailable().to_string(),
            theme.warning(),
        )));
    }
    frame.render_widget(Paragraph::new(lines), footer);
}

/// The head, an elision and the tail of a collapsed paste.
///
/// Over `2 * PASTE_PREVIEW_LINES` lines the middle is replaced by one row
/// saying how much of it there is: the reader is checking what they pasted,
/// and forty lines of log are read at the ends, not in the middle.
fn paste_lines(content: &str, width: usize, panel: &Panel<'_>) -> Vec<Line<'static>> {
    let theme = panel.theme;
    let all = content.lines().collect::<Vec<_>>();
    if all.is_empty() {
        return vec![Line::from("")];
    }
    let head = PASTE_PREVIEW_LINES.min(all.len());
    let tail = PASTE_PREVIEW_LINES.min(all.len().saturating_sub(head));
    let omitted = all.len().saturating_sub(head + tail);
    let mut lines = all[..head]
        .iter()
        .map(|line| preview_line(line, width, theme.prose()))
        .collect::<Vec<_>>();
    if omitted > 0 {
        lines.push(Line::from(Span::styled(
            truncate_to_width(
                &format!(
                    "{} {omitted} {}",
                    glyphs::elision(panel.tier),
                    panel.strings.preview_more_lines()
                ),
                width,
                "…",
            ),
            theme.muted(),
        )));
    }
    lines.extend(
        all[all.len() - tail..]
            .iter()
            .map(|line| preview_line(line, width, theme.prose())),
    );
    lines
}

/// One line of a paste, drawn as the draft would draw it.
///
/// Control characters become spaces for the same reason the composer's own
/// rows do it: a paste is whatever the clipboard held, and an escape sequence
/// painted verbatim is a terminal told to do something.
fn preview_line(line: &str, width: usize, style: Style) -> Line<'static> {
    let clean = line
        .chars()
        .map(|character| {
            if character.is_ascii_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    Line::from(Span::styled(truncate_to_width(&clean, width, "…"), style))
}

/// What is known about an attached picture, on one row under it.
fn image_caption(image: &ImageAttachment, width: usize, theme: &TuiTheme) -> String {
    let mut parts = Vec::new();
    if let Some(name) = image_file_name(image) {
        parts.push(name);
    }
    parts.push(image_format(&image.mime_type).to_string());
    if let Some(thumbnail) = &image.thumbnail {
        parts.push(format!(
            "{}{}{}",
            thumbnail.width,
            glyphs::times(GlyphTier::of(theme)),
            thumbnail.height
        ));
    }
    parts.push(crate::composer::compact_bytes(image.byte_len));
    let separator = format!(" {} ", glyphs::separator(GlyphTier::of(theme)));
    truncate_to_width(&parts.join(&separator), width, "…")
}

/// The picture's own file name, when it has one.
///
/// A clipboard image has no name — it never was a file — and the label the
/// draft gave it is already on the border, so there is nothing to say here.
fn image_file_name(image: &ImageAttachment) -> Option<String> {
    let crate::composer::ImageSource::Path(path) = &image.source else {
        return None;
    };
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())?;
    Some(name.to_string())
}

/// The media type as a reader names it: `image/png` is `PNG`.
fn image_format(mime_type: &str) -> String {
    let subtype = mime_type.split('/').nth(1).unwrap_or(mime_type);
    let subtype = subtype.split('+').next().unwrap_or(subtype);
    match subtype.to_ascii_lowercase().as_str() {
        "" => mime_type.to_ascii_uppercase(),
        "svg" => "SVG".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// Draw the panel's chrome and return the rows its content may use.
fn draw_panel(
    frame: &mut Frame<'_>,
    popup: Rect,
    title: &str,
    hint: Option<&str>,
    theme: &TuiTheme,
) -> Rect {
    frame.render_widget(Clear, popup);
    // The panel is drawn over the transcript rather than in a band of its own,
    // so a line that ends against its left edge is cut in half and has to give:
    // see [`crate::modal::blank_cut_glyphs`].
    crate::modal::blank_cut_glyphs(frame, popup);
    let title = truncate_to_width(title, usize::from(popup.width).saturating_sub(4), "…");
    let block = border_block(theme)
        .border_style(theme.muted())
        .style(theme.base())
        .title(Line::from(Span::styled(
            format!(" {title} "),
            theme.strong(),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if let Some(hint) = hint {
        render_border_hint(frame, popup, hint, theme);
    }
    // One column of padding each side, so the text never touches the border.
    Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    }
}

/// The one action the panel names, written into its bottom border.
///
/// It costs no content row, which on the shortest terminals is the row the
/// last line of the paste is using.
fn render_border_hint(frame: &mut Frame<'_>, popup: Rect, hint: &str, theme: &TuiTheme) {
    // A corner and a dash on each side, and the pads around the text: below
    // this the hint is a smear on the border rather than a sentence.
    const CHROME: usize = 6;
    let available = usize::from(popup.width).saturating_sub(CHROME);
    if available < 8 {
        return;
    }
    let hint = truncate_to_width(hint, available, "…");
    let y = popup.bottom().saturating_sub(1);
    let line = Line::from(vec![
        Span::styled(
            format!("{} ", glyphs::heading_rule(GlyphTier::of(theme))),
            theme.muted(),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(theme.roles.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect {
            x: popup.x + 2,
            y,
            width: popup.width.saturating_sub(4),
            height: 1,
        },
    );
}

/// Paint a thumbnail into `area` as half blocks, centred, aspect preserved.
///
/// The area has already been sized to the picture, so the centring only ever
/// absorbs the rounding of a cell; it is still the terminal that decides how
/// many pixels a cell is. The theme is deliberately not consulted: a picture
/// has no token to resolve to, and a preview that mixed a terminal's sixteen
/// colours would be a diagram of the picture rather than the picture.
/// Whether to draw one at all is [`Panel::mode`]'s question, answered before
/// this is called.
fn draw_raster(frame: &mut Frame<'_>, area: Rect, thumbnail: &ImageThumbnail, tier: GlyphTier) {
    if area.width == 0 || area.height == 0 || thumbnail.cols == 0 || thumbnail.rows == 0 {
        return;
    }
    // One scale for both axes: a picture stretched to fill the box is a
    // picture that lies about its shape.
    let scale = (f32::from(area.width) / f32::from(thumbnail.cols))
        .min(f32::from(area.height) / f32::from(thumbnail.rows));
    let cols = ((f32::from(thumbnail.cols) * scale).floor() as u16).clamp(1, area.width);
    let rows = ((f32::from(thumbnail.rows) * scale).floor() as u16).clamp(1, area.height);
    let x = area.x + area.width.saturating_sub(cols) / 2;
    let y = area.y + area.height.saturating_sub(rows) / 2;
    let symbol = glyphs::half_block(tier);
    let buffer = frame.buffer_mut();
    for row in 0..rows {
        for column in 0..cols {
            let source_column = (u32::from(column) * u32::from(thumbnail.cols) / u32::from(cols))
                .min(u32::from(thumbnail.cols) - 1) as u16;
            let source_row = (u32::from(row) * u32::from(thumbnail.rows) / u32::from(rows))
                .min(u32::from(thumbnail.rows) - 1) as u16;
            let Some((top, bottom)) = thumbnail.cell(source_column, source_row) else {
                continue;
            };
            let Some(cell) = buffer.cell_mut((x + column, y + row)) else {
                continue;
            };
            cell.set_symbol(symbol);
            cell.set_fg(rgb(top));
            cell.set_bg(rgb(bottom));
        }
    }
}

/// The colour one pixel is drawn in.
///
/// The theme's own colour degradation is bypassed on purpose: a picture has no
/// token to resolve to, and a preview that mixed a terminal's sixteen colours
/// would be a diagram of the picture rather than the picture.
fn rgb(pixel: [u8; 3]) -> Color {
    Color::Rgb(pixel[0], pixel[1], pixel[2])
}

/// The width the panel takes out of the room it is given.
fn panel_width(area: Rect) -> u16 {
    let share = area.width / PANEL_WIDTH_SHARE_DENOMINATOR * PANEL_WIDTH_SHARE_NUMERATOR;
    share.max(PANEL_MIN_WIDTH).min(area.width.saturating_sub(2))
}

/// The panel's rectangle: `width` wide, `height` tall, bottom-anchored.
fn panel_rect(area: Rect, width: u16, height: u16) -> Option<Rect> {
    let height = height.min(area.height);
    if height < 3 || width < 4 || area.width < 4 {
        return None;
    }
    Some(Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.bottom().saturating_sub(height),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings() -> Strings {
        Strings::for_locale(crate::locale::Locale::En)
    }

    /// A truecolor unicode terminal, which is the panel at its most capable.
    fn theme() -> TuiTheme {
        TuiTheme::resolve(
            None,
            vibex_ui::GpuiThemeMode::Dark,
            crate::theme::ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: crate::theme::GlyphMode::Unicode,
            },
        )
    }

    fn panel(theme: &TuiTheme) -> Panel<'_> {
        Panel {
            theme,
            strings: strings(),
            tier: GlyphTier::Full,
            mode: ColorMode::TrueColor,
        }
    }

    /// A real PNG of `width` by `height` pixels, each quadrant a solid colour.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut picture = image::RgbImage::new(width, height);
        for (x, y, pixel) in picture.enumerate_pixels_mut() {
            *pixel = image::Rgb([
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                64,
            ]);
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

    /// Whether a raster tells the truth about the picture's shape.
    ///
    /// A cell is two pixels tall, so the raster's pixel box is `cols` by
    /// `2 * rows` — and that box has to have the picture's own aspect ratio,
    /// or the preview is of something the reader did not paste.
    fn keeps_shape(thumbnail: &ImageThumbnail) -> bool {
        let raster = u64::from(thumbnail.cols) * u64::from(thumbnail.height);
        let picture = u64::from(thumbnail.width) * u64::from(thumbnail.rows) * 2;
        // Scaling to whole cells rounds; one part in a hundred is rounding.
        raster.abs_diff(picture) * 100 <= picture.max(1)
    }

    #[test]
    fn a_thumbnail_keeps_the_pictures_shape_and_reports_its_size() {
        let thumbnail = thumbnail(&png(400, 200)).expect("a png decodes");
        assert_eq!((thumbnail.width, thumbnail.height), (400, 200));
        assert!(
            keeps_shape(&thumbnail),
            "{}x{}",
            thumbnail.cols,
            thumbnail.rows
        );
        assert_eq!(
            thumbnail.cells.len(),
            usize::from(thumbnail.cols) * usize::from(thumbnail.rows)
        );
        assert!(thumbnail.cols <= THUMBNAIL_COLS);
        assert!(thumbnail.rows <= THUMBNAIL_ROWS);
    }

    #[test]
    fn a_tall_picture_is_not_stretched_to_fill_the_box() {
        // A phone screenshot: the raster is a narrow strip rather than one
        // widened to the box, because a preview that re-shaped the picture
        // would be a preview of something else.
        let thumbnail = thumbnail(&png(100, 400)).expect("a png decodes");
        assert!(
            keeps_shape(&thumbnail),
            "{}x{}",
            thumbnail.cols,
            thumbnail.rows
        );
        assert!(
            thumbnail.cols < thumbnail.rows,
            "a 1:4 picture is not a wide raster: {}x{}",
            thumbnail.cols,
            thumbnail.rows
        );
    }

    #[test]
    fn bytes_that_are_not_a_picture_have_no_thumbnail() {
        assert!(thumbnail(b"not a picture at all").is_none());
        assert!(thumbnail(&[]).is_none());
    }

    #[test]
    fn a_paste_preview_shows_both_ends_and_counts_what_it_skipped() {
        let log = (0..40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let theme = theme();
        let lines = paste_lines(&log, 40, &panel(&theme));
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text.len(), PASTE_PREVIEW_LINES * 2 + 1);
        assert_eq!(text[0], "line 0");
        assert_eq!(text[PASTE_PREVIEW_LINES - 1], "line 2");
        assert!(
            text[PASTE_PREVIEW_LINES].contains("34 more lines"),
            "{:?}",
            text[PASTE_PREVIEW_LINES]
        );
        assert_eq!(text[PASTE_PREVIEW_LINES + 1], "line 37");
        assert_eq!(text[text.len() - 1], "line 39");
    }

    #[test]
    fn a_short_paste_is_shown_whole() {
        let theme = theme();
        let lines = paste_lines("one\ntwo", 40, &panel(&theme));
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_paste_cannot_paint_an_escape_into_the_terminal() {
        let theme = theme();
        let lines = paste_lines("before \u{1b}[31mred\u{7}", 40, &panel(&theme));
        let text = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert_eq!(text, "before  [31mred ");
    }

    #[test]
    fn the_caption_names_the_file_the_format_the_size_and_the_pixels() {
        let image = ImageAttachment {
            label: "Image #1".into(),
            mime_type: "image/png".into(),
            source: crate::composer::ImageSource::Path("/tmp/shots/shot.png".into()),
            byte_len: 412_000,
            thumbnail: None,
        };
        let theme = theme();
        let caption = image_caption(&image, 80, &theme);
        assert_eq!(caption, "shot.png · PNG · 412 KB");

        // A clipboard image has no name, and one with pixels names them.
        let clipboard = ImageAttachment {
            source: crate::composer::ImageSource::Bytes(std::sync::Arc::new(Vec::new())),
            mime_type: "image/jpeg".into(),
            byte_len: 900,
            thumbnail: Some(ImageThumbnail {
                width: 640,
                height: 480,
                cols: 2,
                rows: 1,
                cells: vec![[0, 0, 0, 0, 0, 0], [0, 0, 0, 0, 0, 0]],
            }),
            ..image
        };
        let caption = image_caption(&clipboard, 80, &theme);
        assert_eq!(caption, "JPEG · 640×480 · 900 bytes");
    }

    #[test]
    fn the_panel_is_bottom_anchored_and_never_wider_than_its_room() {
        let area = Rect::new(0, 0, 100, 20);
        let popup = panel_rect(area, panel_width(area), 9).expect("a panel fits");
        assert_eq!(popup.bottom(), area.bottom());
        assert_eq!(popup.width, 75);
        assert_eq!(popup.height, 9);

        // A narrow terminal gets the whole width, less the margins.
        let narrow = Rect::new(0, 0, 30, 20);
        assert_eq!(panel_width(narrow), 28);

        // Nothing is drawn in a space too short to hold a border.
        assert!(panel_rect(area, 40, 0).is_none());
    }

    /// Whether a cell box has the shape of a picture of `width` by `height`
    /// pixels, within the rounding of a cell.
    fn keeps_picture_shape(cols: u16, rows: u16, width: u32, height: u32) -> bool {
        let boxed = f64::from(cols) / (f64::from(rows) * f64::from(CELL_ASPECT));
        let picture = f64::from(width) / f64::from(height);
        (boxed - picture).abs() / picture < 0.06
    }

    #[test]
    fn a_pictures_panel_is_sized_to_the_pictures_own_shape() {
        let room = Rect::new(0, 0, 90, 24);
        // Twice as wide as it is tall, and a cell is two pixels tall, so the
        // cell box is four times as wide: 24 rows would ask for 96 columns,
        // and the room has 90 — which gives back a row rather than a shape.
        let (cols, rows) = picture_box(400, 200, room);
        assert_eq!(cols, 90, "the picture takes the width the room has");
        assert_eq!(rows, 23, "and keeps its shape while taking it");
        assert!(keeps_picture_shape(cols, rows, 400, 200));

        // A tall picture is bounded by the height instead: a phone screenshot
        // is a strip, not a box stretched to the panel's width.
        let (cols, rows) = picture_box(100, 400, room);
        assert!(cols < rows, "{cols}x{rows} is not a tall picture");
        assert!(rows <= 24);

        // Room that cannot hold either shape still gives one cell of picture.
        assert_eq!(picture_box(400, 200, Rect::new(0, 0, 0, 0)), (1, 1));
        assert_eq!(picture_box(0, 0, room), (1, 1));
    }

    #[test]
    fn a_picture_is_sent_once_and_moved_when_the_panel_moves() {
        use crate::composer::ImageSource;
        use crate::terminal_image::{ImageProtocol, ImageSupport};

        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut picture = PlacedPicture::default();
        let source = ImageSource::Bytes(std::sync::Arc::new(png(40, 20)));
        let first = Rect::new(4, 6, 40, 10);

        place_picture(&support, &mut picture, "[Image #1]", &source, first);
        let sent = String::from_utf8(picture.take_escapes()).expect("escapes are text");
        assert!(
            sent.contains("a=T,f=100"),
            "the picture was not sent: {sent:?}"
        );
        assert!(picture.is_holding());

        // The next frame is the same frame: the terminal already has it.
        place_picture(&support, &mut picture, "[Image #1]", &source, first);
        assert!(
            picture.take_escapes().is_empty(),
            "an unchanged frame sent the picture again"
        );

        // Another rectangle is a placement, not a transmission.
        let moved = Rect::new(8, 7, 40, 10);
        place_picture(&support, &mut picture, "[Image #1]", &source, moved);
        let placed = String::from_utf8(picture.take_escapes()).expect("escapes are text");
        assert!(
            placed.contains("a=p,"),
            "the picture was not moved: {placed:?}"
        );
        assert!(
            !placed.contains("f=100"),
            "a move carried the picture again"
        );

        // Another chip replaces it, and taking the panel down deletes it.
        place_picture(
            &support,
            &mut picture,
            "[Image #2]",
            &ImageSource::Bytes(std::sync::Arc::new(png(10, 10))),
            moved,
        );
        assert!(picture.take_escapes().windows(4).any(|w| w == b"a=T,"));
        clear_picture(&support, &mut picture);
        let cleared = String::from_utf8(picture.take_escapes()).expect("escapes are text");
        assert_eq!(cleared, "\x1b_Ga=d,d=i,i=1,q=2\x1b\\");
        assert!(!picture.is_holding());

        // Nothing left to clear is nothing sent.
        clear_picture(&support, &mut picture);
        assert!(picture.take_escapes().is_empty());
    }

    #[test]
    fn a_terminal_without_pixels_is_sent_no_picture_at_all() {
        use crate::composer::ImageSource;
        use crate::terminal_image::{ImageProtocol, ImageSupport};

        let support = ImageSupport::forced(ImageProtocol::None);
        let mut picture = PlacedPicture::default();
        place_picture(
            &support,
            &mut picture,
            "[Image #1]",
            &ImageSource::Bytes(std::sync::Arc::new(png(40, 20))),
            Rect::new(4, 6, 40, 10),
        );
        assert!(picture.take_escapes().is_empty());
        assert!(!picture.is_holding());
    }

    #[test]
    fn a_picture_the_client_cannot_read_leaves_nothing_on_the_terminal() {
        use crate::composer::ImageSource;
        use crate::terminal_image::{ImageProtocol, ImageSupport};

        let support = ImageSupport::forced(ImageProtocol::Kitty);
        let mut picture = PlacedPicture::default();
        // A readable picture first, then an unreadable one in the same place.
        place_picture(
            &support,
            &mut picture,
            "[Image #1]",
            &ImageSource::Bytes(std::sync::Arc::new(png(40, 20))),
            Rect::new(4, 6, 40, 10),
        );
        let _ = picture.take_escapes();
        place_picture(
            &support,
            &mut picture,
            "[Image #2]",
            &ImageSource::Bytes(std::sync::Arc::new(b"not a picture".to_vec())),
            Rect::new(4, 6, 40, 10),
        );
        let escapes = String::from_utf8(picture.take_escapes()).expect("escapes are text");
        assert!(
            escapes.contains("a=d"),
            "the old picture was left behind: {escapes:?}"
        );
        assert!(!picture.is_holding());
    }
}
