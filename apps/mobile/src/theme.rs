//! Design tokens for the native mobile client.
//!
//! The palette resolves from the shared desktop token source
//! (`crates/vibex-ui` generated tokens) at render time through an appearance
//! mode and the user's light/dark theme selection, so the phone reads as the
//! same product as the desktop shell in every palette. Views must go through
//! these accessors instead of hardcoding colors.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
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
/// scale is a logical pixel *at the default rem*; [`ui_rems`] converts it, so
/// the whole interface tracks the platform's accessibility text scaling the
/// same way `apps/desktop/src/typography.rs` makes the shell track it.
pub const UI_REM: f32 = 16.0;

/// Page-level title on a full-screen surface (a settings sub-page, the new
/// session page). One step above [`FONT_TITLE`].
pub const FONT_APP_TITLE: f32 = 22.0;
/// The title of a surface or a hero row.
pub const FONT_TITLE: f32 = 20.0;
/// A section's own heading — strictly above the rows it groups.
pub const FONT_HEADING: f32 = 17.0;
/// Conversation text and every primary row label.
pub const FONT_BODY: f32 = 16.0;
/// Supporting text under a primary label: row details, timeline metadata.
pub const FONT_DETAIL: f32 = 14.0;
/// Captions, chips, and button labels.
pub const FONT_CAPTION: f32 = 13.0;
/// The floor of the ladder: timestamps, counts, badges. Never below the
/// 12sp Android minimum.
pub const FONT_MICRO: f32 = 12.0;

// The sidebar preserves the desktop hierarchy at one denser mobile type step
// so a full project tree remains scannable on a phone.
pub const FONT_SIDEBAR_TITLE: f32 = 17.0;
pub const FONT_SIDEBAR_ROW: f32 = 15.0;
pub const FONT_SIDEBAR_META: f32 = 13.0;

/// The rem ladder. Views must reach for these instead of `px(theme::FONT_*)`,
/// which ignores the platform's text-scaling setting.
pub fn ui_rems(value: f32) -> Rems {
    rems(value / UI_REM)
}

pub fn font_app_title() -> Rems {
    ui_rems(FONT_APP_TITLE)
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

pub fn font_sidebar_title() -> Rems {
    ui_rems(FONT_SIDEBAR_TITLE)
}

pub fn font_sidebar_row() -> Rems {
    ui_rems(FONT_SIDEBAR_ROW)
}

pub fn font_sidebar_meta() -> Rems {
    ui_rems(FONT_SIDEBAR_META)
}

/// Line height for a step of the ladder: 1.45× the font, rounded to a whole
/// logical pixel so baselines land on the touch grid.
pub fn line_height_for(font: f32) -> Pixels {
    px((font * 1.45).round())
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
/// Pills, chips, and the composer's own container.
pub const RADIUS_PILL: f32 = 999.0;

pub const DRAWER_ROW_HEIGHT: f32 = 56.0;
/// A phone row is a touch target first: the desktop's 32/40px rows put the
/// label and the trailing metadata within a few pixels of each other.
pub const SIDEBAR_ROW_HEIGHT: f32 = 48.0;

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
/// Desktop's compact spinner size for an active workspace or session.
pub const SIDEBAR_STATUS_ICON_SIZE: f32 = 12.0;
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

pub fn bg_composer() -> Hsla {
    tone("composer-surface")
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
        // The phone runs one step denser than the desktop shell; keep kit controls
        // on the compact end so they sit in the same rhythm as the hand-built rows.
        theme.font_family = "IBM Plex Sans".into();
        theme.font_size = px(15.0);
        theme.mono_font_size = px(13.0);
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
}
