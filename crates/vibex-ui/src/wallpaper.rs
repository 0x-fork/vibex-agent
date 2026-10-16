//! Letting a user wallpaper show through the theme's surfaces.
//!
//! The desktop paints a user image behind the workbench and then paints the
//! theme over it, so an opaque palette would hide the image completely. While a
//! wallpaper is behind them, the roles that are *containers* take an alpha: the
//! page and the chrome that frames it at [`WALLPAPER_BASE_ALPHA`], the panels
//! and plates that sit on the page at the more opaque
//! [`WALLPAPER_RAISED_ALPHA`].
//!
//! Three rules keep the result predictable:
//!
//! - Only containers fade. Ink, lines, status colors, borders, and selection
//!   plates keep the authored color, so text over a wallpaper is exactly as
//!   legible as text over the page, and a selected row never turns into a hole.
//! - Cards keep the authored plate too. A card is where long-form content
//!   reads, and the page around it already lets the image through.
//! - The fade *sets* an alpha rather than multiplying one, so the pass is
//!   idempotent: a client that maps a role and a framework bridge that maps the
//!   same role land on the same color.
//!
//! The legacy component tokens are left alone here, but the framework realigns
//! them to their colors on any edit that changes a color, so the fade reaches
//! them anyway. The client restores the one the framework paints a window's own
//! base plate from, after the appearance pass has closed: a translucent base
//! plate would let the desktop behind the window show through it. Tooltips,
//! hints, and notifications read their plate from a token as well, which is
//! what keeps them readable over any image.

use gpui_component::Theme;

/// Alpha for the page and the chrome that frames it: the window background, the
/// sidebar, the title bar, the status bar, and an accordion's body.
pub const WALLPAPER_BASE_ALPHA: f32 = 0.55;

/// Alpha for the panels and plates that sit on the page: group boxes, popovers,
/// lists, tables, tab bars, inputs, and the secondary/muted plates.
///
/// More opaque than the page because these are one layer closer to the reader,
/// and a surface nested inside another one compounds the transparency.
pub const WALLPAPER_RAISED_ALPHA: f32 = 0.72;

/// Fades the theme's container roles so a wallpaper behind them shows through.
///
/// Called by the appearance pass after the component palette is filled in, and
/// only while a wallpaper is actually painted. Applying it to a theme twice is
/// the same as applying it once.
pub fn let_wallpaper_through(theme: &mut Theme) {
    macro_rules! fade_to {
        ($alpha:expr; $($field:ident),* $(,)?) => {
            $(
                theme.colors.$field = theme.colors.$field.alpha($alpha);
            )*
        };
    }

    fade_to!(
        WALLPAPER_BASE_ALPHA;
        // The page itself and the chrome that frames it.
        background,
        accordion,
        sidebar,
        status_bar,
        title_bar,
    );

    fade_to!(
        WALLPAPER_RAISED_ALPHA;
        // Panels and plates that sit on the page.
        group_box,
        popover,
        list,
        list_head,
        table,
        table_head,
        table_foot,
        tab_bar,
        tab_bar_segmented,
        secondary,
        muted,
        input,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::white;

    /// A theme whose container roles are all the same opaque color, so each
    /// assertion reads the alpha the pass chose and nothing else.
    fn opaque_theme() -> Theme {
        let mut theme = Theme::default();
        for color in [
            &mut theme.colors.background,
            &mut theme.colors.accordion,
            &mut theme.colors.sidebar,
            &mut theme.colors.status_bar,
            &mut theme.colors.title_bar,
            &mut theme.colors.group_box,
            &mut theme.colors.popover,
            &mut theme.colors.list,
            &mut theme.colors.list_head,
            &mut theme.colors.table,
            &mut theme.colors.table_head,
            &mut theme.colors.table_foot,
            &mut theme.colors.tab_bar,
            &mut theme.colors.tab_bar_segmented,
            &mut theme.colors.secondary,
            &mut theme.colors.muted,
            &mut theme.colors.input,
        ] {
            *color = white();
        }
        theme
    }

    #[test]
    fn containers_fade_and_ink_keeps_the_authored_color() {
        let mut theme = opaque_theme();
        let foreground = theme.colors.foreground;
        let border = theme.colors.border;
        let primary = theme.colors.primary;
        let list_active = theme.colors.list_active;
        let card_like = theme.colors.group_box_foreground;

        let_wallpaper_through(&mut theme);

        assert_eq!(theme.colors.background.a, WALLPAPER_BASE_ALPHA);
        assert_eq!(theme.colors.sidebar.a, WALLPAPER_BASE_ALPHA);
        assert_eq!(theme.colors.title_bar.a, WALLPAPER_BASE_ALPHA);
        assert_eq!(theme.colors.group_box.a, WALLPAPER_RAISED_ALPHA);
        assert_eq!(theme.colors.popover.a, WALLPAPER_RAISED_ALPHA);
        assert_eq!(theme.colors.input.a, WALLPAPER_RAISED_ALPHA);

        // Ink, lines, accents, and a selected row's plate are not containers.
        assert_eq!(theme.colors.foreground, foreground);
        assert_eq!(theme.colors.border, border);
        assert_eq!(theme.colors.primary, primary);
        assert_eq!(theme.colors.list_active, list_active);
        assert_eq!(theme.colors.group_box_foreground, card_like);
    }

    /// The legacy tokens are what the framework paints a window's base plate and
    /// every tooltip from. They stay authored: a translucent base plate would
    /// let whatever is behind the window show through it.
    #[test]
    fn the_legacy_component_tokens_stay_authored() {
        let mut theme = opaque_theme();
        let background = theme.tokens.background.color;
        let popover = theme.tokens.popover.color;

        let_wallpaper_through(&mut theme);

        assert_eq!(theme.tokens.background.color, background);
        assert_eq!(theme.tokens.popover.color, popover);
    }

    /// A client maps a role and the framework bridge maps the same role, so the
    /// pass can run twice over one theme. Setting the alpha rather than
    /// multiplying it is what makes that safe.
    #[test]
    fn fading_twice_lands_on_the_same_color() {
        let mut once = opaque_theme();
        let_wallpaper_through(&mut once);
        let mut twice = once.clone();
        let_wallpaper_through(&mut twice);

        assert_eq!(twice.colors.background, once.colors.background);
        assert_eq!(twice.colors.group_box, once.colors.group_box);
    }
}
