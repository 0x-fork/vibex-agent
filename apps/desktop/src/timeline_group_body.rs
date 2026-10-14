//! SPIKE: window an expanded process group's rows by the timeline viewport.
//!
//! The timeline virtualizes by turn, and a turn already windows its process
//! *units* by the viewport ([`super::TimelineProcessRun`]). An expanded group is
//! one unit holding every row of the run, so its body is the last place that
//! lays out content nobody can see: a session that ran for hours is one
//! expanded group with hundreds of rows, and each frame rebuilt, laid out and
//! prepainted all of them. The developer HUD's draw phases put that cost in
//! prepaint (25.9ms a frame against 6.2ms collapsed), which is why two earlier
//! changes tried to retain settled units behind cached views — and were reverted
//! twice, because a cached subtree stops receiving the hover and click input the
//! rows need, and every input it reads has to be routed to its view by hand.
//!
//! This body takes the cheaper route to the same layout cost: the rows stay in
//! the live tree, and only the ones the viewport shows are built, measured and
//! placed. It is deliberately a spike — it exists to measure the win before the
//! real element is written, and the workbench keeps `VIBEX_TIMELINE_GROUP_WINDOW`
//! so one build can measure both sides. What it does not do yet, and the real
//! element must:
//!
//! * pin the row a pending reveal (find-in-conversation, session search, the
//!   permission alert) points at when it falls outside the window;
//! * settle row heights across content and disclosure revisions the way
//!   `super::settle_timeline_process_unit_height` does for units, so an estimate
//!   that lands does not move the rows around it, and a streamed row never
//!   shrinks inside one content box;
//! * keep bottom-follow and the captured scroll anchor steady while a streamed
//!   row grows;
//! * place rows from the body's own measured width rather than the last width
//!   the surface reported;
//! * pick its own overscan from how the reader is scrolling. It keeps a shallow
//!   one today ([`super::TIMELINE_GROUP_ROW_OVERSCAN_PX`]) because the unit
//!   run's slack, measured in pixels, covered most of a group whole.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Styled as _, Window, div, point, px, size,
};

/// Builds one row of the group's body on demand.
///
/// Rows are built during prepaint, after the view's render has returned, so the
/// builder may update the workbench to reach the row's current state.
pub(super) type TimelineGroupRowBuilder =
    Box<dyn FnMut(usize, &mut Window, &mut App) -> AnyElement>;

/// Records what the body measured, so the rows it did not build this frame keep
/// a real height instead of an estimate.
pub(super) type TimelineGroupRowRecorder = Box<dyn FnMut(&[(usize, f32)], &mut App)>;

/// One expanded process group's rows, placed by hand at the origins the
/// workbench's row-height table holds.
struct TimelineGroupBody {
    /// Origin of each row, relative to the body's own origin.
    origins: Rc<Vec<Pixels>>,
    /// The height the rows occupy in the flow.
    total_height: Pixels,
    /// Slack the window keeps above and below the viewport, in pixels.
    overscan: Pixels,
    /// Builds row `index` on demand.
    build_row: TimelineGroupRowBuilder,
    /// Records the heights this body measured.
    record: TimelineGroupRowRecorder,
}

impl TimelineGroupBody {
    fn new(
        origins: Rc<Vec<Pixels>>,
        total_height: Pixels,
        overscan: Pixels,
        build_row: TimelineGroupRowBuilder,
        record: TimelineGroupRowRecorder,
    ) -> Self {
        Self {
            origins,
            total_height,
            overscan,
            build_row,
            record,
        }
    }
}

impl IntoElement for TimelineGroupBody {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The flow child the body reserves its height with, and the rows it placed.
struct TimelineGroupBodyLayout {
    spacer: AnyElement,
    rows: Vec<AnyElement>,
}

impl Element for TimelineGroupBody {
    type RequestLayoutState = TimelineGroupBodyLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The rows are placed by hand during prepaint, so the body only has to
        // reserve the height they would have taken in the flow.
        let mut spacer = div()
            .w_full()
            .min_w_0()
            .h(self.total_height)
            .flex_none()
            .into_any_element();
        let layout_id = spacer.request_layout(window, cx);
        (
            layout_id,
            TimelineGroupBodyLayout {
                spacer,
                rows: Vec::new(),
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        layout.spacer.prepaint(window, cx);
        layout.rows.clear();
        if bounds.size.width <= px(0.0) {
            return;
        }
        // The mask this body is prepainted under is the timeline viewport
        // intersected with the disclosure's revealed box, so it is exactly the
        // band the rows have to be built for.
        let viewport = window.content_mask().bounds;
        let available_space = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::MinContent,
        );
        let visible = super::timeline_process_visible_units(
            &self.origins,
            self.total_height,
            bounds,
            viewport,
            self.overscan,
        );
        let mut measurements = Vec::new();
        let mut previous_end = None;
        for index in visible {
            let Some(origin) = self.origins.get(index).copied() else {
                continue;
            };
            let mut row = (self.build_row)(index, window, cx);
            let measured = row.layout_as_root(available_space, window, cx);
            // The preceding disclosure may have changed height in this very
            // frame. Place siblings after its measured bottom, not its stale
            // cached bottom.
            let origin = super::timeline_process_measured_origin(index, origin, previous_end);
            previous_end = Some((index, origin + measured.height));
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                row.prepaint_at(bounds.origin + point(px(0.0), origin), window, cx);
            });
            measurements.push((index, f32::from(measured.height)));
            layout.rows.push(row);
        }
        if !measurements.is_empty() {
            (self.record)(&measurements, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        layout.spacer.paint(window, cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in &mut layout.rows {
                row.paint(window, cx);
            }
        });
    }
}

/// The windowed body of one expanded process group.
pub(super) fn timeline_group_body(
    origins: Rc<Vec<Pixels>>,
    total_height: Pixels,
    overscan: Pixels,
    build_row: TimelineGroupRowBuilder,
    record: TimelineGroupRowRecorder,
) -> AnyElement {
    TimelineGroupBody::new(origins, total_height, overscan, build_row, record).into_any_element()
}
