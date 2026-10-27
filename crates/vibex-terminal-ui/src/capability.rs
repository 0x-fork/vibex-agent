//! What this emulator can draw, stated once for the processes that draw into it.
//!
//! A terminal is not one thing: the screen a reader looks at and the screen a
//! program writes to are the same glass, but the program is told about it
//! through an environment that the *launching* process inherits. That is the
//! seam this module exists for. The desktop is launched from kitty, so it
//! carries `TERM=xterm-kitty` and a window id; a terminal tab it spawns inherits
//! both, and a client inside that tab is told it is writing to kitty when it is
//! writing to this cell grid.
//!
//! The consequence is specific rather than cosmetic: a client that believes it
//! has a graphics protocol draws its Agent marks as pictures, and every one of
//! them arrives as blank cells because this emulator has no pixel layer at all.
//! Saying so before the child starts is what turns those cells back into the
//! characters the client already knows how to draw.
//!
//! # Why the statement lives here
//!
//! It is a property of the emulator, not of the PTY host: the host happens to be
//! the process that spawns the child today, but the same grid is what a remote
//! seat is looking at, and a second host that forgot to declare this would
//! reintroduce the bug from a different file. [`child_environment`] is the one
//! place that answers "what should a program on the other end be told", and the
//! host applies it without deciding anything of its own.

/// Whether the emulator has a layer it can place a picture on.
///
/// It does not, and the two builds say so for the same reason: the native core
/// is `alacritty_terminal`, which parses cells and colours and drops the APC an
/// image protocol travels in; the WASM fallback keeps the same frame contract
/// and even less. A `true` here would be a promise to draw pixels somewhere.
pub const DRAWS_PICTURES: bool = false;

/// The terminal name a child of this emulator should read.
///
/// `xterm-256color` is what the grid actually is — 256 indexed colours, a cursor,
/// and no graphics — and every shell and TUI already knows the name, so nothing
/// downstream needs a special case for it.
pub const TERM_NAME: &str = "xterm-256color";

/// The colour depth a child of this emulator should read.
///
/// Stated rather than inferred: the native core resolves `Rgb` cells faithfully,
/// so the screen really does draw 24-bit colour, while [`TERM_NAME`] is a name
/// that famously does not promise it. Left to infer, a client would drop the
/// whole page to the 256-colour cube.
pub const COLOR_TERM: &str = "truecolor";

/// The variables that describe the *outer* terminal rather than this PTY.
///
/// Each one is a claim about a host this emulator is not: a window id, a
/// clipboard key, a program name, a pixel protocol. Passing one through is what
/// makes a child draw into a layer that does not exist, so they are cleared
/// when the child is started.
///
/// `VIBEX_TUI_IMAGE` is the TUI's own override rather than a terminal's, and it
/// is deliberately absent from this list: it is *set* to `off` rather than
/// unset, because unsetting it would only let detection answer again.
pub const OUTER_TERMINAL_VARIABLES: [&str; 13] = [
    // kitty: the window id and the public key both describe a graphics
    // terminal, and the key also gates the OSC 5522 clipboard read — which
    // cannot work here either, because the clipboard belongs to the outer one.
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_LISTEN_ON",
    // iTerm2's identity, in both of the spellings it publishes.
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    // The other terminals that identify themselves and speak pixels.
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_PANE",
    "WT_SESSION",
    "WT_PROFILE_ID",
];

/// The TUI's own image override, and the one value this module writes.
///
/// `off` outranks detection in the client, so a mark falls back to the Agent's
/// name even if another pixel claim leaks back into the environment later.
pub const IMAGE_OVERRIDE: (&str, &str) = ("VIBEX_TUI_IMAGE", "off");

/// What a program on the other end of this emulator must be told.
///
/// Ordered so the caller can apply it in two passes: the names to clear, then
/// the terminal's own three claims — the image override last, because a host
/// that wrote it before clearing would have it removed by the loop.
pub const fn child_environment() -> ChildEnvironment {
    ChildEnvironment {
        term: TERM_NAME,
        color_term: COLOR_TERM,
        cleared: OUTER_TERMINAL_VARIABLES,
        override_image: IMAGE_OVERRIDE,
    }
}

/// The environment a spawning host applies to a child, as values rather than
/// as prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildEnvironment {
    /// [`TERM_NAME`].
    pub term: &'static str,
    /// [`COLOR_TERM`].
    pub color_term: &'static str,
    /// [`OUTER_TERMINAL_VARIABLES`], to be removed.
    pub cleared: [&'static str; 13],
    /// [`IMAGE_OVERRIDE`], to be set after `cleared`.
    pub override_image: (&'static str, &'static str),
}

impl ChildEnvironment {
    /// Every variable to unset, in one list.
    pub fn removed(&self) -> impl Iterator<Item = &'static str> {
        self.cleared.into_iter()
    }

    /// Every variable to set, in the order a host must apply it.
    pub fn written(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        [
            ("TERM", self.term),
            ("COLORTERM", self.color_term),
            self.override_image,
        ]
        .into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The override is applied *after* the clears, so it must not also be in
    /// the list of names to clear — a host that looped the clear list last
    /// would silently turn the fix off.
    #[test]
    fn the_image_override_is_not_also_cleared() {
        let cleared = OUTER_TERMINAL_VARIABLES
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        assert!(!cleared.contains(IMAGE_OVERRIDE.0));
        assert!(!cleared.contains("TERM"));
        assert!(!cleared.contains("COLORTERM"));
    }

    /// An emulator with no pixel layer must not hand a child a name that
    /// promises one.
    #[test]
    fn the_declared_terminal_is_not_a_graphics_terminal() {
        // Read through a binding rather than the constant so the assertion is
        // a runtime question: it is the value a child would receive that
        // matters, and a `const` block would stop reporting the failure.
        let draws_pictures = DRAWS_PICTURES;
        assert!(!draws_pictures);
        let term = TERM_NAME.to_ascii_lowercase();
        for graphics in ["kitty", "ghostty", "wezterm", "sixel", "direct"] {
            assert!(
                !term.contains(graphics),
                "{TERM_NAME} must not claim {graphics}"
            );
        }
        assert_eq!(
            IMAGE_OVERRIDE,
            ("VIBEX_TUI_IMAGE", "off"),
            "the client's own escape hatch is what makes this a guarantee"
        );
    }

    /// A child is told three things and nothing else; the list is what a host
    /// iterates, so a duplicate name would be a silent ordering hazard.
    #[test]
    fn the_written_variables_are_unique_and_end_with_the_override() {
        let written = child_environment().written().collect::<Vec<_>>();
        let keys = written.iter().map(|(key, _)| *key).collect::<HashSet<_>>();
        assert_eq!(keys.len(), written.len(), "a name was written twice");
        assert_eq!(written.last().copied(), Some(IMAGE_OVERRIDE));
        assert_eq!(
            child_environment().removed().count(),
            OUTER_TERMINAL_VARIABLES.len()
        );
    }
}
