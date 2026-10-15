//! Design tokens for the native mobile client.
//!
//! The palette resolves from the shared desktop token source
//! (`crates/vibex-ui` generated tokens) at render time through an appearance
//! mode and the user's light/dark theme selection, so the phone reads as the
//! same product as the desktop shell in every palette. Views must go through
//! these accessors instead of hardcoding colors.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use gpui::{Anchor, App, Hsla, Pixels, Rems, Rgba, Window, hsla, px, rems, rgb};
use gpui_component::{Edges, Theme, ThemeMode as ComponentThemeMode};
use vibex_desktop_model::ThemeSelection;
use vibex_ui::{
    GpuiThemeDefinition, GpuiThemeMode, default_theme, semantic_token, theme_at, theme_index,
};

use crate::platform::WindowInsetsExt as _;

// ---------------------------------------------------------------------------
// Appearance mode
// ---------------------------------------------------------------------------

/// The mobile appearance preference. `System` resolves against the platform
/// at render time so the phone flips with the OS dark-mode toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppearanceMode {
    Light,
    Dark,
    #[default]
    System,
}

impl AppearanceMode {
    pub fn all() -> [AppearanceMode; 3] {
        [
            AppearanceMode::System,
            AppearanceMode::Light,
            AppearanceMode::Dark,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            AppearanceMode::System => "System",
            AppearanceMode::Light => "Light",
            AppearanceMode::Dark => "Dark",
        }
    }

    pub fn from_storage(value: &str) -> Option<AppearanceMode> {
        match value {
            "light" => Some(AppearanceMode::Light),
            "dark" => Some(AppearanceMode::Dark),
            "system" => Some(AppearanceMode::System),
            _ => None,
        }
    }

    pub fn to_storage(self) -> &'static str {
        match self {
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
            AppearanceMode::System => "system",
        }
    }
}

// Encoding fits the small persistence payload: index into `AppearanceMode::all`.
const MODE_SYSTEM: u8 = 0;
const MODE_LIGHT: u8 = 1;
const MODE_DARK: u8 = 2;

static APPEARANCE_MODE: AtomicU8 = AtomicU8::new(MODE_SYSTEM);

/// Applies the persisted appearance preference for this process. Called once
/// at startup before the first frame and whenever the user changes the
/// setting; every color accessor reads from here.
pub fn set_appearance_mode(mode: AppearanceMode) {
    let encoded = match mode {
        AppearanceMode::System => MODE_SYSTEM,
        AppearanceMode::Light => MODE_LIGHT,
        AppearanceMode::Dark => MODE_DARK,
    };
    APPEARANCE_MODE.store(encoded, Ordering::Relaxed);
}

pub fn appearance_mode() -> AppearanceMode {
    match APPEARANCE_MODE.load(Ordering::Relaxed) {
        MODE_LIGHT => AppearanceMode::Light,
        MODE_DARK => AppearanceMode::Dark,
        _ => AppearanceMode::System,
    }
}

// The System preference resolves against the platform appearance, which the
// app refreshes from its window-appearance observer (and once per frame while
// rendering, so a fresh launch lands on the right side before the first
// observer tick). Dark is the safe default for a cold read.
static SYSTEM_DARK: AtomicU8 = AtomicU8::new(1);

/// Records the platform appearance for the System preference. Called by the
/// app shell whenever the window appearance is known to have changed.
pub fn set_system_dark(dark: bool) {
    SYSTEM_DARK.store(dark as u8, Ordering::Relaxed);
}

/// Whether the window is currently dark. A stored Light/Dark preference wins;
/// the System preference tracks the platform appearance the same way the
/// desktop shell resolves `ThemeMode::System`.
pub fn is_dark() -> bool {
    match appearance_mode() {
        AppearanceMode::Light => false,
        AppearanceMode::Dark => true,
        AppearanceMode::System => SYSTEM_DARK.load(Ordering::Relaxed) == 1,
    }
}

/// Whether the stored preference can still flip at runtime (System mode).
pub fn follows_system() -> bool {
    appearance_mode() == AppearanceMode::System
}

// ---------------------------------------------------------------------------
// Shared token resolution
// ---------------------------------------------------------------------------

/// The palette the client is painting with, per appearance.
///
/// Mirrors the desktop shell's paint-time resolution: colors are read
/// imperatively through [`semantic_color`], which has no `App` handle to
/// consult, so the active catalog position is cached here. `UNSET` resolves to
/// the catalog default, which is what a cold read before any selection lands
/// on.
const UNSET: usize = usize::MAX;

static ACTIVE_LIGHT: AtomicUsize = AtomicUsize::new(UNSET);
static ACTIVE_DARK: AtomicUsize = AtomicUsize::new(UNSET);

fn mode_slot(mode: GpuiThemeMode) -> &'static AtomicUsize {
    match mode {
        GpuiThemeMode::Light => &ACTIVE_LIGHT,
        GpuiThemeMode::Dark => &ACTIVE_DARK,
    }
}

/// The catalog mode the current appearance resolves to.
pub fn mode(dark: bool) -> GpuiThemeMode {
    model_mode(dark)
}

fn model_mode(dark: bool) -> GpuiThemeMode {
    if dark {
        GpuiThemeMode::Dark
    } else {
        GpuiThemeMode::Light
    }
}

/// The theme variant the client is painting with for `mode`.
pub fn active_theme(mode: GpuiThemeMode) -> &'static GpuiThemeDefinition {
    let index = mode_slot(mode).load(Ordering::Relaxed);
    if index == UNSET {
        return default_theme(mode);
    }
    theme_at(index).unwrap_or_else(|| default_theme(mode))
}

/// Point the paint-time palette at `selection`.
///
/// The phone mirrors the desktop's selection so both surfaces name the same
/// palettes; an id the catalog does not know falls back to the default for
/// that appearance instead of failing the frame.
pub fn set_theme_selection(selection: &ThemeSelection) {
    for mode in GpuiThemeMode::ALL {
        let requested = match mode {
            GpuiThemeMode::Light => selection.light(),
            GpuiThemeMode::Dark => selection.dark(),
        };
        let index = requested
            .and_then(|id| theme_index(id, mode))
            .unwrap_or(UNSET);
        mode_slot(mode).store(index, Ordering::Relaxed);
    }
}

/// One step above the desktop lookup helper: a missing token is a build-time
/// contract with `tokens.json`, so a typo surfaces as a panic at first paint
/// instead of a silent wrong color.
pub fn semantic_color(name: &str, dark: bool) -> Hsla {
    let token = semantic_token(active_theme(model_mode(dark)), name)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    Hsla {
        a: token.alpha,
        ..rgb(token.rgb).into()
    }
}

fn tone(name: &str) -> Hsla {
    semantic_color(name, is_dark())
}

// ---------------------------------------------------------------------------
// Palette (dark values kept as the legacy fallback contract for tests)
// ---------------------------------------------------------------------------

/// Flattens a translucent token (for example the desktop's 10% white border)
/// over an opaque surface so callers that need a solid fill keep the same
/// contrast the alpha produced on the given background. `Rgba::blend`
/// composites its argument on top of the receiver, so the surface receives
/// the token. The blend must stay in `Rgba`: a `u32` round-trip through
/// `gpui::rgb` would reinterpret the `0xRRGGBBAA` layout as `0x00RRGGBB` and
/// turn the alpha byte into a full-blue channel.
fn flatten(token: Rgba, over: Rgba) -> Rgba {
    over.blend(token)
}

fn opaque(name: &str, surface: &str) -> Hsla {
    let dark = is_dark();
    let token = semantic_color(name, dark);
    if token.a >= 1.0 {
        return token;
    }
    let over = semantic_color(surface, dark);
    let flattened = flatten(Rgba::from(token), Rgba::from(over).alpha(1.0));
    flattened.into()
}

// ---------------------------------------------------------------------------
// Spacing and typography
// ---------------------------------------------------------------------------

pub const SPACING_XS: f32 = 4.0;
pub const SPACING_SM: f32 = 8.0;
pub const SPACING_MD: f32 = 12.0;
pub const SPACING_LG: f32 = 16.0;
pub const SPACING_XL: f32 = 20.0;

/// The narrowest a toast may become before it stops reading as a card.
pub const TOAST_MIN_WIDTH: f32 = 200.0;
/// The widest a toast may grow: a landscape window or a tablet would otherwise
/// stretch one line across the whole screen.
pub const TOAST_MAX_WIDTH: f32 = 420.0;
/// How many toasts may share the screen. A phone shows one or two; beyond that
/// they cover the content they are talking about.
pub const TOAST_MAX_VISIBLE: usize = 3;

/// The rem the ladder below is expressed against. Every value in the type
/// scale is a logical pixel at this rem; [`ui_rems`] converts it, so the whole
/// interface tracks the platform's accessibility text scaling the same way
/// `apps/desktop/src/typography.rs` makes the shell track it.
///
/// The reader's text-size step lives in the window's live rem base
/// ([`rem_base`]), *not* in [`ui_rems`]: the base multiplies every rem in the
/// frame at once, so a step repaints the whole interface — the hand-built rows
/// and the kit controls alike — through the one value the kit also reads.
///
/// The base is the body step rather than the web's 16px convention: the kit
/// derives its own `text_base` from this rem, so anchoring it on [`FONT_BODY`]
/// is what keeps a kit control the same size as the row label beside it.
pub const UI_REM: f32 = FONT_BODY;

/// Ratio of a line box to its font size. A surface that does not carry its own
/// metric takes [`line_height_for`], so a ladder step scales its leading with
/// it instead of freezing at one screen's pixel height.
///
/// GPUI's own default is the golden ratio (1.618), which a phone cannot afford
/// inside its touch-target rows; this is the tighter box the ladder uses.
pub const LINE_HEIGHT_RATIO: f32 = 1.45;

/// The title of a surface or a hero row.
pub const FONT_TITLE: f32 = 22.0;
/// A section's own heading — strictly above the rows it groups.
pub const FONT_HEADING: f32 = 18.0;
/// Conversation text and every primary row label. The ladder is anchored here:
/// it is the size the reader looks at longest, so it sets both [`UI_REM`] and
/// the step everything else is judged against.
pub const FONT_BODY: f32 = 17.0;
/// Supporting text under a primary label: row details, timeline metadata.
pub const FONT_DETAIL: f32 = 15.0;
/// Captions, chips, and button labels.
pub const FONT_CAPTION: f32 = 14.0;
/// Code buffers: monospace reads larger than the interface face at the same
/// size, so it sits at the caption's step. An alias rather than a second value
/// so the two can never drift apart.
pub const FONT_CODE: f32 = FONT_CAPTION;
/// The floor of the ladder: timestamps, counts, badges. It is the smallest
/// step the phone renders, kept at the size secondary copy stays legible in.
pub const FONT_MICRO: f32 = 13.0;
/// Large isolated numerals — the pairing code, a count on an otherwise empty
/// page. Never used for prose, so it sits above [`FONT_TITLE`].
pub const FONT_DISPLAY: f32 = 30.0;
/// The product wordmark where a page leads with it. Above [`FONT_DISPLAY`]
/// because it is a mark, not a size any row or label ever takes.
pub const FONT_WORDMARK: f32 = 36.0;

/// The sidebar's own steps. The tree is the screen the reader scans most, so
/// each step sits one rung above the same role elsewhere: a project name reads
/// a step over a section heading, a session row at body size, and its metadata
/// at the detail size rather than the caption floor.
pub const FONT_SIDEBAR_TITLE: f32 = 20.0;
pub const FONT_SIDEBAR_ROW: f32 = FONT_BODY;
pub const FONT_SIDEBAR_META: f32 = FONT_DETAIL;

/// Body line height as a rem multiple: the handful of call sites that need the
/// number rather than an element style (a max-height line count) read it here
/// instead of restating the ratio.
pub const BODY_LINE_HEIGHT_REM: f32 = FONT_BODY * LINE_HEIGHT_RATIO / UI_REM;

/// The window's live rem base: the ladder's default rem times the reader's
/// text-size step. This is the single place the preference enters layout — the
/// kit reads the same value as its `theme.font_size`, so a step moves both
/// halves of the interface together.
pub fn rem_base() -> Pixels {
    rem_base_for(font_scale_percent())
}

/// The rem base one text-size step produces.
///
/// Split from [`rem_base`] so the scaling rule is unit-testable without
/// mutating the process-global step (which parallel UI tests also read).
fn rem_base_for(percent: u32) -> Pixels {
    px(UI_REM * percent.clamp(75, 150) as f32 / 100.0)
}

/// A ladder value as rems. Pure unit conversion: the scale already lives in
/// [`rem_base`], so applying it here as well would square it.
pub fn ui_rems(value: f32) -> Rems {
    rems(value / UI_REM)
}

/// A ladder value as scaled pixels, for the few surfaces GPUI sizes in pixels
/// rather than rems (a brand lockup whose vector geometry must track its text).
pub fn scaled_px(value: f32) -> Pixels {
    px(value * font_scale_percent() as f32 / 100.0)
}

/// The size a code buffer is drawn at, which is a step below the body text and
/// follows the same preference.
pub fn code_font_size() -> Rems {
    ui_rems(FONT_CODE)
}

pub fn font_title() -> Rems {
    ui_rems(FONT_TITLE)
}

pub fn font_heading() -> Rems {
    ui_rems(FONT_HEADING)
}

pub fn font_body() -> Rems {
    ui_rems(FONT_BODY)
}

pub fn font_detail() -> Rems {
    ui_rems(FONT_DETAIL)
}

pub fn font_caption() -> Rems {
    ui_rems(FONT_CAPTION)
}

pub fn font_micro() -> Rems {
    ui_rems(FONT_MICRO)
}

pub fn font_display() -> Rems {
    ui_rems(FONT_DISPLAY)
}

pub fn font_wordmark() -> Rems {
    ui_rems(FONT_WORDMARK)
}

/// The label size the mark's geometry below is authored against: the 42pt
/// lockup `logo-wordmark-white.svg` draws for the desktop, where the mark's box
/// is 46×45, sits 2px into a centering row, and then rides 6px above it.
const WORDMARK_AUTHORED_FONT: f32 = 42.0;
const WORDMARK_MARK_WIDTH: f32 = 46.0;
const WORDMARK_MARK_HEIGHT: f32 = 45.0;
const WORDMARK_MARK_MARGIN_TOP: f32 = 2.0;
const WORDMARK_MARK_TOP_OFFSET: f32 = -6.0;

/// The brand mark that supplies a wordmark lockup's capital "V", sized for the
/// label drawn beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordmarkMark {
    /// The box the SVG is drawn in. The mark's own ink follows the viewBox's
    /// ratio inside it, so the box is not square.
    pub width: Pixels,
    pub height: Pixels,
    /// Margin over the box. The row centers margin boxes, so this is part of
    /// the geometry rather than a gap between two elements.
    pub margin_top: Pixels,
    /// What `.top()` takes: the offset that moves the drawn mark off the row's
    /// center line and onto the label's baseline.
    pub top_offset: Pixels,
}

/// The mark beside a label rendered at `font` pixels.
///
/// Every length scales from the label's own size. That is the whole point of
/// the helper: the authored offsets land the mark's point on the 42pt baseline,
/// and a mark frozen in pixels leaves that line as soon as the reader's
/// text-size step moves the label off 42 — the label is where the step enters
/// the lockup, so a fixed mark drifts against it at every other size.
pub fn wordmark_mark(font: Pixels) -> WordmarkMark {
    let scale = f32::from(font) / WORDMARK_AUTHORED_FONT;
    WordmarkMark {
        width: px(WORDMARK_MARK_WIDTH * scale),
        height: px(WORDMARK_MARK_HEIGHT * scale),
        margin_top: px(WORDMARK_MARK_MARGIN_TOP * scale),
        top_offset: px(WORDMARK_MARK_TOP_OFFSET * scale),
    }
}

/// The sidebar's steps, one rung above the same role on the content pages. The
/// tree is read by scanning, and the extra step is what stops a project name
/// from reading as a section label and a session row from reading as metadata.
pub fn font_sidebar_title() -> Rems {
    ui_rems(FONT_SIDEBAR_TITLE)
}

pub fn font_sidebar_row() -> Rems {
    ui_rems(FONT_SIDEBAR_ROW)
}

pub fn font_sidebar_meta() -> Rems {
    ui_rems(FONT_SIDEBAR_META)
}

/// Line box for a step of the ladder, at [`LINE_HEIGHT_RATIO`]. Returned as
/// rems so it scales with [`rem_base`] like the font it belongs to.
pub fn line_height_for(font: f32) -> Rems {
    rems(font * LINE_HEIGHT_RATIO / UI_REM)
}

pub const ICON_MD: f32 = 18.0;
pub const ICON_SM: f32 = 16.0;
pub const ICON_STATUS: f32 = 6.0;
/// The plate behind a settings row's glyph.
pub const ICON_SLOT: f32 = 34.0;
/// A settings row is title + detail, so it is taller than a single-line list
/// row while staying a single touch target.
pub const SETTINGS_ROW_HEIGHT: f32 = 60.0;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The compact top bar stays below the native status-bar inset supplied by GPUI.
pub const HEADER_HEIGHT: f32 = 48.0;
/// Every header control is a full touch target, not the 40px the phone used to
/// reserve: the audit measured those under the platform minimum.
pub const HEADER_BUTTON_SIZE: f32 = 44.0;
/// Drawer and overlay headers have a little more room for the compact action
/// cluster while the session page keeps the shorter workbench header.
pub const DRAWER_HEADER_HEIGHT: f32 = 56.0;
/// Minimum tappable edge for controls that are not header buttons.
pub const TOUCH_TARGET: f32 = 44.0;
/// Caps centered panel content so tablets do not stretch a phone layout.
pub const CARD_WIDTH: f32 = 300.0;

pub const RADIUS_CARD: f32 = 10.0;
pub const RADIUS_CONTROL: f32 = 8.0;
/// The top corners of a sheet pulled up from the bottom edge.
///
/// Rounder than any card: the sheet's whole top edge is a curve the eye
/// follows from the scrim up the panel, and it is the arc the grabber sits on.
/// A card's radius reads as a corner; a sheet's has to read as a lip.
pub const RADIUS_SHEET: f32 = 22.0;
/// The prompt card's own radius: rounder than a list card, because it is the
/// one surface the reader types into rather than reads.
pub const RADIUS_COMPOSER: f32 = 14.0;
/// Pills, chips, and the composer's own container.
pub const RADIUS_PILL: f32 = 999.0;
/// The height of the pill inside a control row. The row keeps a full touch
/// target; only the pill the reader sees is this tall.
pub const CHIP_HEIGHT: f32 = 32.0;

pub const DRAWER_ROW_HEIGHT: f32 = 56.0;
/// A phone row is a touch target first: the desktop's 32/40px rows put the
/// label and the trailing metadata within a few pixels of each other.
///
/// It is the floor, not the whole story — a row that stacks two lines takes
/// [`sidebar_row_height`], which grows with the type so a larger text step does
/// not clip its second line.
pub const SIDEBAR_ROW_HEIGHT: f32 = 48.0;

/// The height of a sidebar row that stacks a title over its metadata.
///
/// The two lines, never below the touch-target floor. The text-size preference
/// grows the rem base, so a row pinned to the fixed floor would clip its second
/// line at the larger steps — this makes room for it instead. At the default
/// step the text is shorter than the floor, so the row keeps its current
/// density and only the accessibility steps add height.
pub fn sidebar_row_height() -> Pixels {
    sidebar_row_height_for(rem_base())
}

/// The two-line sidebar row height one rem base produces.
///
/// The two-line row is a project or workspace row: its title is
/// [`FONT_SIDEBAR_TITLE`] and its metadata [`FONT_SIDEBAR_META`], so the box is
/// measured from that pair rather than the content ladder's heading/caption.
///
/// Split from [`sidebar_row_height`] so the fitting rule is unit-testable at
/// every step without mutating the process-global text-size preference.
fn sidebar_row_height_for(rem: Pixels) -> Pixels {
    let text = f32::from(line_height_for(FONT_SIDEBAR_TITLE).to_pixels(rem))
        + f32::from(line_height_for(FONT_SIDEBAR_META).to_pixels(rem));
    px(text.max(SIDEBAR_ROW_HEIGHT))
}

// ---------------------------------------------------------------------------
// Motion
//
// Every animation runs through [`motion_ms`], so the Reduce Motion preference
// collapses the whole interface in one place instead of per call site.
// ---------------------------------------------------------------------------

/// The fastest transition the phone uses: a pressed plate, a chip selection.
pub const MOTION_FAST_MS: u64 = 120;
/// A page or sheet entering or leaving.
pub const MOTION_BASE_MS: u64 = 200;
/// A large surface travelling the width of the viewport.
pub const MOTION_SLOW_MS: u64 = 280;

static REDUCE_MOTION: AtomicU8 = AtomicU8::new(0);

/// The reader's text-size step, as a percentage of the ladder below. Held as
/// an integer because atomics do not carry floats; 100 is the default.
static FONT_SCALE_PERCENT: AtomicU32 = AtomicU32::new(100);

/// The steps the text-size setting offers. Three is what a phone can show
/// without turning the choice into a slider nobody can judge.
pub const FONT_SCALE_STEPS: [u32; 3] = [90, 100, 115];

/// Stores the reader's text-size step.
///
/// The step only reaches layout through [`rem_base`], which the kit publishes
/// as the window rem and which this module rewrites whenever the scale changes.
/// Callers pass their window so the new base takes effect on this frame rather
/// than the next window layout.
pub fn set_font_scale_percent(percent: u32) {
    FONT_SCALE_PERCENT.store(percent.clamp(75, 150), Ordering::Relaxed);
}

/// Applies a text-size step to the live window and repaints it.
///
/// `window.set_rem_size` is the one lever that moves the whole interface: the
/// ladder's rems resolve against it, and so do the kit controls once
/// [`apply_component_theme`] has written the same value into `theme.font_size`.
pub fn apply_font_scale(percent: u32, window: &mut Window) {
    set_font_scale_percent(percent);
    window.set_rem_size(rem_base());
}

pub fn font_scale_percent() -> u32 {
    FONT_SCALE_PERCENT.load(Ordering::Relaxed)
}

/// Applies the persisted Reduce Motion preference. Called at startup and when
/// the setting changes.
pub fn set_reduce_motion(reduce: bool) {
    REDUCE_MOTION.store(reduce as u8, Ordering::Relaxed);
}

pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed) == 1
}

/// A duration honouring Reduce Motion: zero when the preference is on, so
/// `with_animation` paints the final frame immediately.
pub fn motion_ms(base: u64) -> Duration {
    if reduce_motion() {
        Duration::ZERO
    } else {
        Duration::from_millis(base)
    }
}

// ---------------------------------------------------------------------------
// Sidebar tree geometry
//
// These mirror the desktop's `SIDEBAR_*` constants in `apps/desktop/src/app.rs`
// so the phone renders the same project tree: same icon slots, same indents,
// same folder guides, same worktree card. Only the row height is the phone's
// own, because every desktop row is a hover target while every phone row is a
// touch target.
// ---------------------------------------------------------------------------

/// Horizontal padding of the row list (the desktop's `px_4`).
pub const SIDEBAR_LIST_PADDING: f32 = 16.0;
/// Leading slot that carries a project logo, a folder mark, or a worktree
/// status dot.
pub const SIDEBAR_ICON_SLOT: f32 = 30.0;
/// The slot hangs left of the list padding so its glyph optically aligns with
/// the section heading above the list.
pub const SIDEBAR_ICON_SLOT_OVERHANG: f32 = 8.0;
/// Gap between the leading slot and the row title.
pub const SIDEBAR_ICON_TITLE_GAP: f32 = 4.0;
/// Indent of a project's own sessions in the compact hierarchy.
pub const SIDEBAR_PROJECT_SESSION_INDENT: f32 = 12.0;
/// Indent of the sessions inside a worktree card.
pub const SIDEBAR_WORKSPACE_SESSION_INDENT: f32 = 20.0;
/// Indent of the rows filed inside a folder.
pub const SIDEBAR_FOLDER_CHILD_INDENT: f32 = 18.0;
/// Where a folder draws the guide line down its child column.
pub const SIDEBAR_FOLDER_GUIDE_OFFSET: f32 = 7.0;
/// Width of one level of the ownership tree's guide column.
///
/// A delegated session hangs off the session above it: the column starts exactly
/// where that row's Agent logo starts, a one-pixel trunk runs down it and an
/// elbow turns off the trunk into the child's own logo. The constants are the
/// desktop sidebar's, so the two trees draw the same branch.
pub const SIDEBAR_DELEGATED_GUIDE_WIDTH: f32 = 16.0;
/// Where the trunk runs inside its guide column.
///
/// The column starts on the parent row's Agent logo, so the trunk's own edge
/// lands on that logo's center: the branch reads as leaving the logo above it,
/// and the one-pixel line stays on a pixel boundary instead of blurring across
/// a half pixel.
pub const SIDEBAR_DELEGATED_GUIDE_TRUNK_OFFSET: f32 = SIDEBAR_AGENT_LOGO_SIZE / 2.0;
/// Gap between a delegated row's guide column and its own Agent logo.
///
/// It is the row's own leading gap, which the elbow measures itself against.
pub const SIDEBAR_DELEGATED_ROW_GAP: f32 = SIDEBAR_ICON_TITLE_GAP;
/// How far the elbow reaches past the trunk.
///
/// The trunk has to cross the rest of its own column and the row's gap, which
/// ends the elbow exactly where the child's Agent logo begins.
pub const SIDEBAR_DELEGATED_GUIDE_ELBOW_WIDTH: f32 = SIDEBAR_DELEGATED_GUIDE_WIDTH
    + SIDEBAR_DELEGATED_ROW_GAP
    - SIDEBAR_DELEGATED_GUIDE_TRUNK_OFFSET;
/// A session row starts its content past the card overhang instead of hanging
/// into it the way project and worktree rows do.
pub const SIDEBAR_SESSION_CONTENT_INSET: f32 = 8.0;
/// Trailing metadata column of a session row (time and status). Keeping it
/// compact leaves more room for the session title.
pub const SIDEBAR_SESSION_META_WIDTH: f32 = 48.0;
/// Corner radius of a row's fill.
pub const SIDEBAR_ROW_RADIUS: f32 = 8.0;
/// Corner radius of the worktree card that wraps a worktree and its sessions.
pub const SIDEBAR_CARD_RADIUS: f32 = 10.0;
/// Right inset of the worktree card (the desktop's `pr(4)`).
pub const SIDEBAR_CARD_INSET: f32 = 4.0;
/// Agent mark on a session row.
pub const SIDEBAR_AGENT_LOGO_SIZE: f32 = 14.0;
/// Project and folder marks.
pub const SIDEBAR_PROJECT_LOGO_SIZE: f32 = 16.0;
/// Worktree and session status dot.
pub const SIDEBAR_STATUS_DOT: f32 = 8.0;
/// Unread-completion dot, which is deliberately smaller than a status dot.
pub const SIDEBAR_UNREAD_DOT: f32 = 7.0;

/// Indent per tree level, matching the desktop's nested sidebar spacing.
pub const SIDEBAR_INDENT: f32 = 14.0;
/// Width of the trailing sidebar menu column. Row movement uses a long press on
/// the body, so no separate drag affordance is reserved.
pub const SIDEBAR_ACTION_WIDTH: f32 = 34.0;
/// The menu is layered over the row wrapper, so row content needs only the
/// small visual inset before its icon rather than the whole menu hitbox width.
pub const SIDEBAR_ACTION_CONTENT_INSET: f32 = 8.0;
pub const DRAWER_ACTION_HEIGHT: f32 = 40.0;
pub const DRAWER_SECTION_HEIGHT: f32 = 40.0;
/// Horizontal travel before a page pan may be recognized as a drawer swipe.
/// Deliberately above the platform touch slop so ordinary taps and slow
/// vertical scrolls on the session screen never open a side page, while a
/// normal swipe still arms after roughly a centimeter of travel.
pub const DRAWER_DRAG_THRESHOLD: f32 = 12.0;
/// A pan whose vertical travel reaches this fraction of its horizontal travel
/// belongs to list scrolling, not to a drawer swipe.
pub const DRAWER_VERTICAL_CANCEL_RATIO: f32 = 1.6;
/// Rightward travel a finger must cover over a page opened on top of another one
/// before the swipe counts as the back gesture. Deliberately well above the
/// drawer's arm threshold: opening a drawer by accident is a page move the user
/// can see and undo, while leaving a page by accident can lose the work in it.
pub const OVERLAY_BACK_SWIPE_TRAVEL: f32 = 48.0;
/// Fraction of the viewport a page must travel before settling on the next page.
pub const DRAWER_SNAP_TRAVEL_RATIO: f32 = 0.15;
/// Release-adjacent movement in the intended direction commits the transition.
pub const DRAWER_SNAP_COMMIT_DIRECTION_THRESHOLD: f32 = 2.0;
/// Reversing a transition requires a much clearer release-adjacent movement.
pub const DRAWER_SNAP_REVERSE_DIRECTION_THRESHOLD: f32 = 28.0;
/// A swipe only commits when its release velocity points at the target with at
/// least this magnitude (logical px/s), so slow drags rely on travel instead.
pub const DRAWER_SWIPE_VELOCITY: f32 = 600.0;
/// Ratio between horizontal and vertical release velocity for a fling to count
/// as a horizontal swipe.
pub const DRAWER_SWIPE_STRAIGHTNESS: f32 = 1.5;
pub const DRAWER_BACKDROP_OPACITY: f32 = 0.4;
pub const DRAWER_OPEN_ANIMATION_MS: u64 = 160;
pub const DRAWER_CLOSE_ANIMATION_MS: u64 = 100;

/// How tall a bottom sheet may rise before its body scrolls instead.
///
/// A sheet is a thing laid over the page, not a page of its own: whatever it
/// explains keeps a band of the screen so the reader never loses their place.
pub const SHEET_HEIGHT_RATIO: f32 = 0.62;
/// The scrim behind a sheet at full rise.
///
/// One value for every sheet: the sheet is the same object whichever page pulls
/// it up, and the pages that dimmed harder or softer were reading as different
/// kinds of surface for no reason the reader could act on.
pub const SHEET_BACKDROP_OPACITY: f32 = 0.55;
/// How long a sheet takes to rise from the foot of the screen.
///
/// Longer than the drawer's own snap: a panel travels the height of the sheet
/// itself, and the eye has to be able to follow its top edge all the way up.
pub const SHEET_RISE_MS: u64 = 280;
/// How long a sheet takes to leave. A little quicker than the rise, because the
/// reader has already decided and the panel is going where they pushed it.
pub const SHEET_FALL_MS: u64 = 220;
/// The bar at the top of every sheet.
pub const SHEET_GRABBER_WIDTH: f32 = 44.0;
pub const SHEET_GRABBER_HEIGHT: f32 = 5.0;
/// How far a finger must pull the panel before the sheet follows it, so a tap
/// that wobbles on the way down is still a tap.
pub const SHEET_PULL_THRESHOLD: f32 = 8.0;
/// How far the panel must be pulled for the finger's release to dismiss the
/// sheet. A fixed distance rather than a share of the sheet: what the reader
/// judges it against is the rows that slid up under the finger.
pub const SHEET_PULL_DISMISS_TRAVEL: f32 = 96.0;
/// How fast a released pull must still be moving to dismiss the sheet, in
/// logical pixels per second: a short flick leaves the same way a long drag
/// does.
pub const SHEET_PULL_DISMISS_VELOCITY: f32 = 700.0;
/// How long after a pull lets go a tap on the scrim is ignored.
///
/// The lift that ends a pull can land on the scrim itself once the panel has
/// moved out from under the finger, and that is not a tap anyone made.
pub const SHEET_PULL_TAP_GUARD_MS: u64 = 200;

// ---------------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------------

pub fn bg_primary() -> Hsla {
    tone("background")
}

pub fn bg_card() -> Hsla {
    tone("card")
}

pub fn bg_card_dim() -> Hsla {
    // Dense lists need the dimmed fill flattened over the page background so
    // nested rows keep one consistent contrast step in both appearances.
    opaque("secondary", "background")
}

pub fn bg_popover() -> Hsla {
    tone("popover")
}

pub fn text_primary() -> Hsla {
    tone("foreground")
}

pub fn text_secondary() -> Hsla {
    // The desktop sidebar fades one foreground; the phone keeps the ladder
    // explicit. Secondary text is 78% of the foreground in both appearances.
    let mut color = tone("foreground");
    color.a = 0.78;
    color
}

pub fn text_muted() -> Hsla {
    tone("muted-foreground")
}

pub fn border_default() -> Hsla {
    opaque("border", "background")
}

pub fn border_subtle() -> Hsla {
    opaque("sidebar-border", "background")
}

pub fn border_input() -> Hsla {
    opaque("input", "background")
}

/// The bar at the top of a bottom sheet.
///
/// Not the border tone: a border is a tenth of a black and vanishes against the
/// card it is drawn on, while this bar is the one control that says the panel
/// can be pulled back down. It takes the muted foreground at a share that reads
/// on both appearances without competing with the sheet's own title.
pub fn sheet_grabber() -> Hsla {
    let mut color = tone("muted-foreground");
    color.a = if is_dark() { 0.55 } else { 0.45 };
    color
}

pub fn row_pressed_bg() -> Hsla {
    // Desktop parity (`theme::hover_wash`): dark paints a soft-white wash,
    // light the tone-flipped soft-black.
    let (luminance, alpha) = if is_dark() {
        (0.92, 0.11)
    } else {
        (0.10, 0.11)
    };
    hsla(0.0, 0.0, luminance, alpha)
}

pub fn row_active_bg() -> Hsla {
    // Desktop parity (`theme::active_wash`).
    let (luminance, alpha) = if is_dark() {
        (0.92, 0.16)
    } else {
        (0.10, 0.16)
    };
    hsla(0.0, 0.0, luminance, alpha)
}

pub fn accent() -> Hsla {
    tone("accent")
}

pub fn accent_foreground() -> Hsla {
    tone("accent-foreground")
}

pub fn primary() -> Hsla {
    tone("primary")
}

pub fn primary_foreground() -> Hsla {
    tone("primary-foreground")
}

/// Status and chart accents stay identical across both appearances, matching
/// the desktop right-rail tones.
///
/// `accent_success` names the role a successful/connected state paints with.
/// The shared catalog has no `success` semantic token yet, so it resolves the
/// same `chart-2` tone the desktop right rail uses for the same state — the
/// name is what the call sites should depend on, not the token behind it.
pub fn accent_success() -> Hsla {
    tone("chart-2")
}

pub fn accent_green() -> Hsla {
    accent_success()
}

pub fn accent_blue() -> Hsla {
    tone("chart-category-1")
}

pub fn accent_yellow() -> Hsla {
    tone("warning")
}

pub fn accent_red() -> Hsla {
    tone("destructive")
}

pub fn accent_dim() -> Hsla {
    tone("muted-foreground")
}

pub fn accent_purple() -> Hsla {
    tone("chart-category-8")
}

/// Git change statuses share the desktop right-rail tones exactly.
pub fn status_added() -> Hsla {
    tone("right-rail-status-added")
}

pub fn status_modified() -> Hsla {
    tone("right-rail-status-modified")
}

pub fn status_untracked() -> Hsla {
    tone("right-rail-status-untracked")
}

/// Teal accent used for agent-account markers in the session-settings cascade.
pub fn accent_chart3() -> Hsla {
    tone("chart-3")
}

pub fn sidebar_drop_bg() -> Hsla {
    let mut color = accent_blue();
    color.a = 0.18;
    color
}

pub fn workbench_bg() -> Hsla {
    sidebar_bg()
}

pub fn workbench_panel_bg() -> Hsla {
    tone("card")
}

pub fn sidebar_bg() -> Hsla {
    tone("sidebar")
}

pub fn sidebar_selected_bg() -> Hsla {
    opaque("sidebar-accent", "sidebar")
}

pub fn sidebar_text_primary() -> Hsla {
    tone("sidebar-foreground")
}

pub fn sidebar_text_secondary() -> Hsla {
    let mut color = tone("sidebar-foreground");
    color.a = 0.78;
    color
}

pub fn sidebar_text_muted() -> Hsla {
    tone("muted-foreground")
}

/// The desktop's sidebar builds its contrast ladder by fading one foreground
/// colour rather than by swapping palette entries. Rows that copy a desktop
/// opacity go through here so the two trees land on the same shade.
pub fn sidebar_foreground(alpha: f32) -> Hsla {
    let mut color = tone("sidebar-foreground");
    color.a = alpha;
    color
}

// The ladder below replaces the flat 56%/48%/45%/35% the drawer used to fade
// with. At 11–13px those shades measured 3.7:1 down to 2.1:1 on the light
// palette; the desktop's own session row runs 88% light / 72% dark, which is
// where these values come from.
/// A session row's title.
pub fn sidebar_session_title() -> Hsla {
    sidebar_foreground(if is_dark() { 0.72 } else { 0.88 })
}

/// The timestamp, path, or count trailing a session row.
pub fn sidebar_session_meta() -> Hsla {
    sidebar_foreground(if is_dark() { 0.62 } else { 0.66 })
}

/// A placeholder written where content will be ("No sessions yet").
pub fn sidebar_placeholder() -> Hsla {
    sidebar_foreground(if is_dark() { 0.54 } else { 0.58 })
}

/// A row that is present but deliberately recessive (an archived session).
pub fn sidebar_archived() -> Hsla {
    sidebar_foreground(if is_dark() { 0.46 } else { 0.52 })
}

pub fn sidebar_card_focus_border() -> Hsla {
    let mut color = tone("sidebar-ring");
    color.a = if is_dark() { 0.42 } else { 0.45 };
    color
}

/// Guide line down a folder's child column.
pub fn sidebar_tree_guide() -> Hsla {
    let mut color = opaque("sidebar-border", "sidebar");
    color.a = 0.70;
    color
}

/// The switch track when it is on.
pub fn switch_track_on() -> Hsla {
    tone("primary")
}

/// The switch track when it is off.
pub fn switch_track_off() -> Hsla {
    opaque("muted", "card")
}

/// The switch knob: the surface colour, so it reads as a hole in the track.
pub fn switch_thumb() -> Hsla {
    tone("background")
}

pub fn backdrop(opacity: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, opacity)
}

// ---------------------------------------------------------------------------
// gpui-kit component theme
// ---------------------------------------------------------------------------

/// Point the gpui-kit global `Theme` at the same `vibex_ui` tokens the rest of
/// the phone renders from.
///
/// Without this bridge every kit component would paint from gpui-kit's own
/// default palette, which reads as a second design system bolted onto the
/// phone. The mapping mirrors the desktop shell's `theme::apply_appearance`
/// token for token, so a `Sheet`, `Input`, or `Button` renders in the product's
/// colors instead of the framework's.
///
/// Call this on the appearance mode changing and once before the first window
/// paints.
pub fn apply_component_theme(window: Option<&mut Window>, cx: &mut App) {
    let dark = is_dark();
    Theme::change(
        if dark {
            ComponentThemeMode::Dark
        } else {
            ComponentThemeMode::Light
        },
        window,
        cx,
    );

    let token = |name: &str| semantic_color(name, dark);
    Theme::update(cx, |theme| {
        // The kit's base is the same rem the ladder is written against, so a
        // kit control and the hand-built row beside it resolve to one scale.
        // Crucially it carries the reader's text-size step too: the kit's
        // `Root` publishes `theme.font_size` as the window rem, which is what
        // makes a step move the kit controls as well as the ladder.
        theme.font_family = "IBM Plex Sans".into();
        theme.font_size = rem_base();
        theme.mono_font_size = scaled_px(FONT_CODE);
        theme.radius = px(RADIUS_CONTROL);
        theme.radius_lg = px(RADIUS_CARD);

        let background = token("background");
        let foreground = token("foreground");
        let secondary = token("secondary");
        let muted = token("muted");
        let muted_foreground = token("muted-foreground");
        let border = token("border");
        let input = token("input");
        let sidebar = token("sidebar");
        let hover = row_pressed_bg();
        let active = row_active_bg();

        theme.background = background;
        theme.tokens.background = background.into();
        theme.foreground = foreground;
        theme.tokens.foreground = foreground.into();
        theme.secondary = secondary;
        theme.tokens.secondary = secondary.into();
        theme.secondary_foreground = token("secondary-foreground");
        theme.tokens.secondary_foreground = theme.secondary_foreground.into();
        theme.muted = muted;
        theme.tokens.muted = muted.into();
        theme.muted_foreground = muted_foreground;
        theme.tokens.muted_foreground = muted_foreground.into();
        theme.primary = token("primary");
        theme.tokens.primary = theme.primary.into();
        theme.primary_foreground = token("primary-foreground");
        theme.tokens.primary_foreground = theme.primary_foreground.into();
        theme.border = border;
        theme.tokens.border = border.into();
        theme.input = input;
        theme.tokens.input = input.into();
        theme.ring = token("ring");
        theme.tokens.ring = theme.ring.into();
        theme.sidebar = sidebar;
        theme.tokens.sidebar = sidebar.into();
        theme.sidebar_foreground = token("sidebar-foreground");
        theme.tokens.sidebar_foreground = theme.sidebar_foreground.into();
        theme.sidebar_border = token("sidebar-border");
        theme.tokens.sidebar_border = theme.sidebar_border.into();
        theme.accent = token("accent");
        theme.tokens.accent = theme.accent.into();
        theme.accent_foreground = token("accent-foreground");
        theme.tokens.accent_foreground = theme.accent_foreground.into();
        // Hover and pressed plates. The phone's washes are translucent by design
        // (`row_pressed_bg`), so the kit picks up the same wash the hand-built rows
        // already paint.
        theme.list_hover = hover;
        theme.tokens.list_hover = hover.into();
        theme.table_hover = hover;
        theme.tokens.table_hover = hover.into();
        theme.secondary_hover = hover;
        theme.tokens.secondary_hover = hover.into();
        theme.button_hover = hover;
        theme.tokens.button_hover = hover.into();
        theme.button_active = active;
        theme.tokens.button_active = active.into();
        theme.primary_hover = token("primary");
        theme.tokens.primary_hover = theme.primary_hover.into();
        theme.primary_active = token("primary");
        theme.tokens.primary_active = theme.primary_active.into();
        // The scrim darkens what is behind it, so light mode runs about half.
        theme.overlay = gpui::black().opacity(if dark { 0.60 } else { 0.32 });
        theme.title_bar = sidebar;
        theme.tokens.title_bar = sidebar.into();
        theme.title_bar_border = token("sidebar-border");
        theme.tokens.title_bar_border = theme.title_bar_border.into();
        // Everything above maps the product's own roles; the rest of the framework
        // palette would keep the stock neutral colors `Theme::change` loaded.
        // Complete it from the same variant. `Theme::update` then publishes the
        // result to the base layer that owns scrollbars, resize handles, and text
        // view defaults, and refreshes every window.
        vibex_ui::apply_component_palette(theme, active_theme(model_mode(dark)));
    });
}

/// Points the kit's notification layer at the phone's own corner.
///
/// The surface is the one the desktop workbench delivers its hints through; on
/// a phone only where it lands differs. Toasts sit along the bottom, where a
/// thumb already rests and where they cover no header control, and they keep
/// clear of the system gesture bar and of the software keyboard — both of which
/// reach the app as window insets, and the keyboard arrives as a bottom inset
/// the moment it opens.
///
/// The kit reads the layer's width and margins from the theme rather than from
/// each notification, so they are recomputed from the live window instead of
/// being set once: the usable width changes with rotation, and the bottom
/// margin changes with the keyboard.
///
/// This is called from the app's render, so it writes the theme global in place
/// rather than through `Theme::update`. `Theme::update` finishes by refreshing
/// every window, which during a render schedules the very frame it is already
/// in: the metrics would be re-derived and re-applied forever and the app would
/// never go idle. The write is guarded so it only lands when the window has
/// actually moved, and the layer renders after the view, so it reads the new
/// values in the same frame.
pub fn sync_notification_metrics(window: &Window, cx: &mut App) {
    let insets = window.insets().effective();
    let viewport = window.viewport_size();
    let usable = f32::from(viewport.width)
        - f32::from(insets.left)
        - f32::from(insets.right)
        - SPACING_MD * 2.0;
    let width = px(usable.clamp(TOAST_MIN_WIDTH, TOAST_MAX_WIDTH));
    let margin = |inset: Pixels| px(f32::from(inset) + SPACING_MD);
    let margins = Edges {
        top: margin(insets.top),
        right: margin(insets.right),
        bottom: margin(insets.bottom),
        left: margin(insets.left),
    };
    let settings = &mut Theme::global_mut(cx).notification;
    if settings.placement == Anchor::BottomCenter
        && settings.max_items == TOAST_MAX_VISIBLE
        && settings.width == width
        && settings.margins == margins
    {
        return;
    }
    settings.placement = Anchor::BottomCenter;
    settings.max_items = TOAST_MAX_VISIBLE;
    settings.width = width;
    settings.margins = margins;
}

#[cfg(test)]
mod tests {
    use super::*;

    // One combined test: the appearance preference lives in a process-global,
    // so parallel test threads would race each other's mode.
    #[test]
    fn appearance_preference_resolves_the_shared_palette() {
        set_appearance_mode(AppearanceMode::Light);
        assert!(!is_dark());
        assert_eq!(
            bg_primary(),
            semantic_color("background", false),
            "light background must resolve from the shared token source"
        );
        assert_eq!(bg_primary().a, 1.0);

        set_appearance_mode(AppearanceMode::Dark);
        assert!(is_dark());
        assert_eq!(
            bg_primary(),
            semantic_color("background", true),
            "dark background must resolve from the shared token source"
        );

        // Dark border is white at 10% over #0d0d0d: flattening lifts the
        // surface to ~#252525 and must stay achromatic. A stray blue channel
        // here means the blend order or a hex round-trip corrupted the
        // channels (the bug that painted every dark-mode divider blue).
        let over = semantic_color("background", true);
        let flattened = flatten(
            Rgba::from(semantic_color("border", true)),
            Rgba::from(over).alpha(1.0),
        );
        assert_eq!(flattened.a, 1.0);
        let lifted = 0.1 + 0.9 * (0x0d as f32 / 255.0);
        for channel in [flattened.r, flattened.g, flattened.b] {
            assert!(
                (channel - lifted).abs() < 1e-3,
                "flattened dark border must stay an achromatic wash"
            );
        }
        assert_eq!(Rgba::from(border_default()), flattened);
        // The opaque accessors never leak alpha.
        assert_eq!(border_default().a, 1.0);
        assert_eq!(border_subtle().a, 1.0);
        assert_eq!(bg_card_dim().a, 1.0);
        assert_eq!(sidebar_selected_bg().a, 1.0);

        // Status tones match the desktop right-rail values.
        assert_eq!(accent_red(), semantic_color("destructive", true));
        assert_eq!(accent_yellow(), semantic_color("warning", true));
        assert_eq!(accent_blue(), semantic_color("chart-category-1", true));

        // Storage encoding round-trips every mode.
        for mode in AppearanceMode::all() {
            set_appearance_mode(mode);
            assert_eq!(appearance_mode(), mode);
            assert_eq!(AppearanceMode::from_storage(mode.to_storage()), Some(mode));
        }
        assert_eq!(AppearanceMode::from_storage("nope"), None);
        set_appearance_mode(AppearanceMode::System);
    }

    // The three tests below cover the type-scale contract that the mobile
    // screen got wrong: one rem base carries the reader's step, the ladder is
    // pure unit conversion on top of it, and the sidebar has no second ladder.

    #[test]
    fn the_reader_step_moves_the_rem_base_once() {
        // The scale enters layout exactly once, through the window rem. If a
        // ladder accessor multiplied it too, the default step would no longer
        // render at its authored size. Float arithmetic on the two sides of
        // each case can differ by an ulp, so compare within half a pixel.
        let close = |left: Pixels, right: Pixels| (f32::from(left) - f32::from(right)).abs() < 0.5;
        assert!(close(rem_base_for(100), px(UI_REM)));
        assert!(close(rem_base_for(115), px(UI_REM * 1.15)));
        assert!(close(rem_base_for(90), px(UI_REM * 0.90)));
        assert!(
            close(rem_base_for(1000), px(UI_REM * 1.5)),
            "clamped at the top"
        );
        assert!(
            close(rem_base_for(0), px(UI_REM * 0.75)),
            "clamped at the floor"
        );

        // `ui_rems` must not re-apply the step: at the default step a body
        // value is exactly one rem, and the same value is scale-independent.
        assert_eq!(font_body().0, 1.0);
        assert_eq!(font_micro().0, FONT_MICRO / UI_REM);
    }

    #[test]
    fn the_ladder_descends_and_has_no_duplicate_steps() {
        let ladder = [
            (FONT_TITLE, "title"),
            (FONT_HEADING, "heading"),
            (FONT_BODY, "body"),
            (FONT_DETAIL, "detail"),
            (FONT_CAPTION, "caption"),
            (FONT_MICRO, "micro"),
        ];
        for pair in ladder.windows(2) {
            assert!(
                pair[0].0 > pair[1].0,
                "the ladder must strictly descend: {pair:?}"
            );
        }
        // Code is the one deliberate alias.
        assert_eq!(FONT_CODE, FONT_CAPTION);

        // The sidebar deliberately does not reuse the content ladder: the tree
        // is read by scanning, so each step sits one rung above the same role
        // on a content page. A project name outranks a section heading, a
        // session row outranks a row detail, and its metadata outranks a chip
        // label — if any of these collapse back onto the content ladder the
        // tree loses the extra step it was given. A const block makes each
        // claim a build error, not only a failing test run.
        const {
            assert!(
                FONT_SIDEBAR_TITLE > FONT_HEADING,
                "a project name must outrank a section heading"
            );
            assert!(
                FONT_SIDEBAR_ROW > FONT_DETAIL,
                "a session row must outrank a row detail"
            );
            assert!(
                FONT_SIDEBAR_META > FONT_CAPTION,
                "sidebar metadata must outrank a chip label"
            );
            // The three sidebar steps still descend among themselves.
            assert!(FONT_SIDEBAR_TITLE > FONT_SIDEBAR_ROW);
            assert!(FONT_SIDEBAR_ROW > FONT_SIDEBAR_META);
        }

        // The phone's sizes, pinned so a future tweak has to be deliberate.
        // The ladder is anchored on the conversation text; the floor stays at
        // the smallest size a phone can render legibly in secondary copy.
        assert_eq!(
            [
                FONT_TITLE,
                FONT_HEADING,
                FONT_BODY,
                FONT_DETAIL,
                FONT_CAPTION,
                FONT_MICRO,
            ],
            [22.0, 18.0, 17.0, 15.0, 14.0, 13.0]
        );
        assert_eq!(
            [FONT_SIDEBAR_TITLE, FONT_SIDEBAR_ROW, FONT_SIDEBAR_META],
            [20.0, 17.0, 15.0]
        );
        assert_eq!(UI_REM, FONT_BODY, "the rem base is the body step");
    }

    #[test]
    fn line_height_scales_with_the_font_it_belongs_to() {
        // A line box is a rem multiple, so it tracks the rem base like the
        // font does rather than freezing at one screen's pixel height.
        let body = line_height_for(FONT_BODY);
        assert!((body.0 - FONT_BODY * LINE_HEIGHT_RATIO / UI_REM).abs() < f32::EPSILON);
        assert!((BODY_LINE_HEIGHT_REM - body.0).abs() < f32::EPSILON);
        // A denser step gets a proportionally tighter box.
        assert!(line_height_for(FONT_MICRO).0 < body.0);
    }

    /// The lockup's mark is a function of the label beside it, at every step.
    ///
    /// The authored 42pt numbers are the desktop wordmark's; what matters here
    /// is that nothing about the mark is frozen. A mark that keeps one pixel
    /// size while the label follows the reader's step is what put its point off
    /// the label's baseline at any step but the default, and the four ratios
    /// below are the contract that keeps it there.
    #[test]
    fn the_wordmark_mark_scales_with_the_label_it_belongs_to() {
        let authored = wordmark_mark(px(WORDMARK_AUTHORED_FONT));
        let close =
            |left: Pixels, right: Pixels| (f32::from(left) - f32::from(right)).abs() < 0.001;
        assert!(close(authored.width, px(WORDMARK_MARK_WIDTH)));
        assert!(close(authored.height, px(WORDMARK_MARK_HEIGHT)));
        assert!(close(authored.margin_top, px(WORDMARK_MARK_MARGIN_TOP)));
        assert!(close(authored.top_offset, px(WORDMARK_MARK_TOP_OFFSET)));

        // The mark's box is wider than it is tall, and its ink is drawn to the
        // viewBox's ratio inside it rather than to the box's own.
        assert!(authored.width > authored.height);
        assert!(authored.top_offset < px(0.0), "the mark rides above center");

        for font in [
            FONT_MICRO,
            FONT_CAPTION,
            FONT_BODY,
            FONT_HEADING,
            FONT_WORDMARK,
        ] {
            let mark = wordmark_mark(px(font));
            let scale = font / WORDMARK_AUTHORED_FONT;
            assert!(close(mark.width, px(WORDMARK_MARK_WIDTH * scale)));
            assert!(close(mark.height, px(WORDMARK_MARK_HEIGHT * scale)));
            assert!(close(mark.margin_top, px(WORDMARK_MARK_MARGIN_TOP * scale)));
            assert!(close(mark.top_offset, px(WORDMARK_MARK_TOP_OFFSET * scale)));
        }
    }

    #[test]
    fn a_two_line_sidebar_row_never_clips_its_second_line() {
        // A row stacking a project title over its metadata must be at least as
        // tall as its two line boxes at every step the reader can pick, or the
        // larger steps clip the metadata line the row exists to show.
        let lines_at = |rem: Pixels| {
            f32::from(line_height_for(FONT_SIDEBAR_TITLE).to_pixels(rem))
                + f32::from(line_height_for(FONT_SIDEBAR_META).to_pixels(rem))
        };
        for percent in FONT_SCALE_STEPS {
            let rem = rem_base_for(percent);
            let row = f32::from(sidebar_row_height_for(rem));
            assert!(
                row >= lines_at(rem),
                "row {row}px must hold {}px of text at {percent}%",
                lines_at(rem)
            );
        }
        // At the default step the two lines now exceed the touch-target floor,
        // so the row takes the text height: the floor is a minimum, not a cap
        // that would clip the second line.
        assert!(lines_at(rem_base_for(100)) > SIDEBAR_ROW_HEIGHT);
        assert_eq!(
            sidebar_row_height_for(rem_base_for(100)),
            px(lines_at(rem_base_for(100)))
        );
        // At the compact step the text still fits inside the floor, so the row
        // stays at the touch target rather than shrinking with the type.
        assert!(lines_at(rem_base_for(90)) <= SIDEBAR_ROW_HEIGHT);
        assert_eq!(
            sidebar_row_height_for(rem_base_for(90)),
            px(SIDEBAR_ROW_HEIGHT)
        );
        // A larger step keeps growing the row.
        assert!(
            sidebar_row_height_for(rem_base_for(115)) > sidebar_row_height_for(rem_base_for(100))
        );
    }

    /// One level of the delegated tree is the desktop's 16px guide column, and
    /// the elbow it turns into the child has to end where that child's Agent logo
    /// begins: one uninterrupted line from the parent's mark to its child's, with
    /// no gap that reads as a break and no overshoot into the mark itself.
    #[test]
    fn a_branch_elbow_reaches_the_child_agent_logo() {
        assert_eq!(SIDEBAR_DELEGATED_GUIDE_WIDTH, 16.0);
        assert_eq!(
            SIDEBAR_DELEGATED_GUIDE_TRUNK_OFFSET,
            SIDEBAR_AGENT_LOGO_SIZE / 2.0
        );
        // The gap the elbow crosses is the leading gap the row already leaves,
        // not a second number that could drift away from it.
        assert_eq!(SIDEBAR_DELEGATED_ROW_GAP, SIDEBAR_ICON_TITLE_GAP);
        assert_eq!(
            SIDEBAR_DELEGATED_GUIDE_TRUNK_OFFSET + SIDEBAR_DELEGATED_GUIDE_ELBOW_WIDTH,
            SIDEBAR_DELEGATED_GUIDE_WIDTH + SIDEBAR_ICON_TITLE_GAP
        );
    }
}
