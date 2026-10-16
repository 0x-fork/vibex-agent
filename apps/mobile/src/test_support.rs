//! Scaffolding shared by the mobile tests that drive the kit's touch menus.
//!
//! A long press over text leaves an edit menu the kit draws itself — over the
//! composer through the input's touch selection, and over message text through
//! the window selection — and the kit names every item after its label, which
//! the language setting translates. A test that wants to press Copy therefore
//! reads the painted items out of the kit's observation registry, in the order
//! the bar lays them out, instead of looking up a label that changes with the
//! language.

use gpui::{Bounds, ElementId, Pixels, VisualTestContext};

/// The kit's edit menu as this frame painted it: one `(label, bounds)` per
/// item, left to right, and empty when no menu is up.
pub(crate) fn touch_menu_items(cx: &mut VisualTestContext) -> Vec<(String, Bounds<Pixels>)> {
    let mut items = cx.update(|window, _| {
        gpui_base::test_support::snapshots(window)
            .into_iter()
            .filter_map(|snapshot| {
                let path = snapshot.path();
                if !path.iter().any(is_touch_menu) {
                    return None;
                }
                let ElementId::Name(label) = path.last()? else {
                    return None;
                };
                Some((label.to_string(), snapshot.bounds()))
            })
            .collect::<Vec<_>>()
    });
    items.sort_by_key(|(_, bounds)| bounds.left());
    items
}

/// The two menus a long press can leave: the input's, over the composer, and
/// the window's, over text a message renders.
fn is_touch_menu(id: &ElementId) -> bool {
    match id {
        ElementId::Name(name) => name.as_ref() == "window-touch-selection",
        ElementId::NamedInteger(name, _) => name.as_ref() == "input-touch-selection",
        _ => false,
    }
}
