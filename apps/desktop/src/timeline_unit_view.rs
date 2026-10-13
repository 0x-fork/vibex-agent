//! One retained element tree per process unit of the focused timeline.
//!
//! A turn's process section is rebuilt on every frame the timeline is dirty, and
//! a live turn is dirty on every streamed chunk, so the units inside it were laid
//! out again even when nothing about them had moved. Measured on an expanded tool
//! group, that cost showed up entirely in prepaint — 12 to 42ms a frame against 4
//! to 6ms collapsed, with element construction flat at ~1ms — so the fix has to
//! skip layout and element prepaint, not merely shrink them.
//!
//! A unit owns its subtree here and is mounted through `Entity::cached`: a frame
//! that does not move the unit's render key replays the previous frame's prepaint
//! and paint ranges without walking the subtree at all, and only the unit whose
//! content actually changed is rendered again.
//!
//! Two boundaries have to hold for that. The view must be a child of a *plain*
//! element tree: `gpui-pre` disables nested caching while an ancestor view
//! re-renders, so the timeline boundary above this one is mounted uncached for
//! that reason. And the parent must tell the view when its content moved, which
//! is what [`TimelineUnitRenderKey`] is for: the parent compares the key each
//! frame and notifies the view when it is stale.

use super::*;

/// Weak unit views retained before dead entries are swept.
///
/// A unit that scrolls out of view drops its view with the element that mounted
/// it, so most of the table is tombstones by the time it reaches this size.
const TIMELINE_UNIT_VIEW_LIMIT: usize = 512;

/// Whether a process unit's retained element tree still describes this frame.
///
/// The unit view renders again only when this key moves. A unit's own content
/// and its disclosure are both carried by `revision` — a group folds its open
/// state into it — so the rest of the key is the surrounding state that changes
/// a unit's shape without touching any row: the language, the display flags that
/// decide whether a command or a file operation gets its own card, an open find
/// bar's tint, and the palette the elements paint with.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineUnitRenderKey {
    revision: i64,
    base: TimelineUnitRenderBase,
}

/// The part of a unit's render key that every unit of one render shares.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TimelineUnitRenderBase {
    locale: locale::ResolvedLocale,
    /// Enhanced command and file-operation display.
    flags: u8,
    /// Folded find-in-conversation and session-search highlight state.
    search: u64,
    /// The colors elements inherit rather than carry in their own style.
    theme: u64,
}

pub(super) struct TimelineUnitView {
    workbench: WeakEntity<VibexWorkbench>,
    /// The session whose timeline this unit belongs to, so a unit rendered while
    /// a pane is borrowed borrows that pane's view back.
    session_id: Option<VibexSessionId>,
    turn_id: String,
    unit_id: String,
    /// Render inputs the mounted element tree was built from.
    key: Option<TimelineUnitRenderKey>,
    #[cfg(test)]
    render_count: usize,
}

impl TimelineUnitView {
    fn new(
        workbench: &Entity<VibexWorkbench>,
        session_id: Option<VibexSessionId>,
        turn_id: String,
        unit_id: String,
    ) -> Self {
        Self {
            workbench: workbench.downgrade(),
            session_id,
            turn_id,
            unit_id,
            key: None,
            #[cfg(test)]
            render_count: 0,
        }
    }

    /// How many times this view built its element tree.
    #[cfg(test)]
    pub(super) fn render_count(&self) -> usize {
        self.render_count
    }
}

impl Render for TimelineUnitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count = self.render_count.saturating_add(1);
        }
        let Some(workbench) = self.workbench.upgrade() else {
            return Empty.into_any_element();
        };
        let session_id = self.session_id.clone();
        let turn_id = self.turn_id.clone();
        let unit_id = self.unit_id.clone();
        let rendered = workbench.update(cx, |this, cx| {
            this.with_session_view_for_render(session_id.as_ref(), |this| {
                let (turn, unit) = this.timeline_process_unit_by_id(&turn_id, &unit_id)?;
                let key = this.timeline_unit_render_key(&unit, cx);
                let element = this.render_timeline_process_unit(&turn, &unit, window, cx);
                Some((key, element, unit.revision, unit.streaming))
            })
        });
        let Some((key, element, revision, streaming)) = rendered else {
            return Empty.into_any_element();
        };
        self.key = Some(key);
        // The measurement reports the unit's own height back to the table the
        // parent places it with. The view itself only holds weak handles, so the
        // element carries one for as long as it stays mounted.
        let report_workbench = workbench.downgrade();
        let report_session = session_id;
        let report_unit = unit_id.clone();
        let report = Rc::new(move |height: f32, cx: &mut App| {
            let _ = report_workbench.update(cx, |this, cx| {
                this.with_session_view_for_render(report_session.as_ref(), |this| {
                    this.record_timeline_unit_height(&report_unit, revision, streaming, height, cx)
                });
            });
        });
        TimelineUnitMeasure {
            id: ElementId::Name(format!("timeline-unit:{unit_id}").into()),
            child: element,
            report,
        }
        .into_any_element()
    }
}

/// Measures one unit's natural height inside the box its parent reserved for it,
/// reports it, and paints the unit at that box.
///
/// `Entity::cached` lays a view out at the style it was mounted with, because
/// caching skips rendering the contents to measure them. A unit's own content
/// still decides how tall it is, so the view measures its child here, once per
/// rendered frame, and hands the number to the height table: the box the parent
/// reserves follows on the next frame, exactly the way a measured estimate has
/// always replaced an estimated one.
struct TimelineUnitMeasure {
    id: ElementId,
    child: AnyElement,
    report: Rc<dyn Fn(f32, &mut App)>,
}

impl IntoElement for TimelineUnitMeasure {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TimelineUnitMeasure {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
    ) -> (LayoutId, ()) {
        // The box is the parent's, not the content's: the view was mounted at
        // the height the table holds. The content is laid out and measured once,
        // during prepaint, at that same width.
        let mut style = gpui::Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let measured = self.child.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::MinContent,
            ),
            window,
            cx,
        );
        (self.report)(f32::from(measured.height), cx);
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            self.child.prepaint_at(bounds.origin, window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            self.child.paint(window, cx)
        });
    }
}

impl VibexWorkbench {
    /// Whether this render retains one element tree per process unit.
    ///
    /// A session group pane borrows another session's view and is itself a cached
    /// boundary, so a unit boundary inside it would be busted by every pane
    /// re-render; the child Agent rail paints a whole child timeline in one
    /// column. Both keep the inline path, and with it the run's own measurement.
    pub(super) fn retaining_timeline_process_units(&self) -> bool {
        !self.rendering_child_agent_timeline()
            && self.timeline.session_id.is_some()
            && self.timeline.session_id.as_ref() == self.selected_session_id.as_ref()
    }

    /// One unit's element: its own cached view, or the inline element when this
    /// render keeps the pane path.
    pub(super) fn timeline_process_unit_element(
        &mut self,
        turn: &TimelineConversationTurn,
        unit: &TimelineProcessUnit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.retaining_timeline_process_units() {
            return self.render_timeline_process_unit(turn, unit, window, cx);
        }
        let view = self.ensure_timeline_unit_view(turn, unit, cx);
        let key = self.timeline_unit_render_key(unit, cx);
        if view.read(cx).key != Some(key) {
            // The unit moved since it last rendered: ask for a fresh subtree. Its
            // height follows from the measurement it reports while prepainting.
            view.update(cx, |_, cx| cx.notify());
        }
        let height = cached_timeline_process_unit_height(
            self.timeline_process_unit_heights.get(&unit.id),
            unit.revision,
            unit.streaming,
        )
        .unwrap_or(unit.estimated_height);
        view.cached(StyleRefinement::default().w_full().h(px(height.max(1.0))))
            .into_any_element()
    }

    /// The cached view standing for one unit, created on first use.
    fn ensure_timeline_unit_view(
        &mut self,
        turn: &TimelineConversationTurn,
        unit: &TimelineProcessUnit,
        cx: &mut Context<Self>,
    ) -> Entity<TimelineUnitView> {
        if let Some(view) = self
            .timeline_unit_views
            .get(&unit.id)
            .and_then(|view| view.upgrade())
        {
            return view;
        }
        if self.timeline_unit_views.len() >= TIMELINE_UNIT_VIEW_LIMIT {
            self.timeline_unit_views
                .retain(|_, view| view.upgrade().is_some());
        }
        let workbench = cx.entity();
        let session_id = self.timeline.session_id.clone();
        let view = cx.new(|_| {
            TimelineUnitView::new(&workbench, session_id, turn.id.clone(), unit.id.clone())
        });
        self.timeline_unit_views
            .insert(unit.id.clone(), view.downgrade());
        view
    }

    /// The unit a retained view stands for, resolved from the turn projection
    /// the virtual list is rendering.
    fn timeline_process_unit_by_id(
        &self,
        turn_id: &str,
        unit_id: &str,
    ) -> Option<(Rc<TimelineConversationTurn>, TimelineProcessUnit)> {
        let turn = self
            .conversation_turns_render_cache
            .borrow()
            .iter()
            .find(|turn| turn.id == turn_id)
            .cloned()?;
        let unit = self
            .timeline_process_units(&turn)
            .into_iter()
            .find(|unit| unit.id == unit_id)?;
        Some((turn, unit))
    }

    /// The render key one unit's retained element tree is valid for.
    fn timeline_unit_render_key(
        &self,
        unit: &TimelineProcessUnit,
        cx: &App,
    ) -> TimelineUnitRenderKey {
        TimelineUnitRenderKey {
            revision: unit.revision,
            base: self.timeline_unit_render_base(cx),
        }
    }

    /// The part of a unit's render key that every unit of one render shares.
    fn timeline_unit_render_base(&self, cx: &App) -> TimelineUnitRenderBase {
        let mut search = std::collections::hash_map::DefaultHasher::new();
        self.conversation_find_open.hash(&mut search);
        self.conversation_find_query.hash(&mut search);
        self.conversation_find_active_item_id.hash(&mut search);
        self.session_search_highlight_query.hash(&mut search);
        self.session_search_highlight_item_id.hash(&mut search);
        let mut theme = std::collections::hash_map::DefaultHasher::new();
        let tokens = cx.theme();
        let hash_color = |color: Hsla, state: &mut std::collections::hash_map::DefaultHasher| {
            color.h.to_bits().hash(state);
            color.s.to_bits().hash(state);
            color.l.to_bits().hash(state);
            color.a.to_bits().hash(state);
        };
        hash_color(tokens.background, &mut theme);
        hash_color(tokens.foreground, &mut theme);
        hash_color(tokens.muted, &mut theme);
        hash_color(tokens.muted_foreground, &mut theme);
        hash_color(tokens.border, &mut theme);
        hash_color(tokens.danger, &mut theme);
        hash_color(tokens.success, &mut theme);
        cx.theme().is_dark().hash(&mut theme);
        TimelineUnitRenderBase {
            locale: locale::current_locale(),
            flags: u8::from(self.ui_state.session.enhanced_command_execution_display)
                | (u8::from(self.ui_state.session.enhanced_file_operation_display) << 1),
            search: search.finish(),
            theme: theme.finish(),
        }
    }

    /// Folds one measured unit height into the table the parent places it with.
    fn record_timeline_unit_height(
        &mut self,
        unit_id: &str,
        revision: i64,
        streaming: bool,
        measured: f32,
        cx: &mut Context<Self>,
    ) {
        if !measured.is_finite() || measured <= 0.0 {
            return;
        }
        let measured = measured.ceil().max(1.0);
        let layout_width = self.timeline_layout_width;
        let current = self.timeline_process_unit_heights.get(unit_id).copied();
        let settled = settle_timeline_process_unit_height(
            current,
            revision,
            measured,
            streaming,
            layout_width,
        );
        // The revision and the box are refreshed even when the height did not
        // move, so the layout lookup keeps matching — but a chunk that only
        // advanced the revision must not ask for another frame.
        let moved = current.is_none_or(|current| (current.height - settled.height).abs() >= 1.0);
        self.timeline_process_unit_heights
            .insert(unit_id.to_string(), settled);
        if moved {
            cx.notify();
        }
    }
}
