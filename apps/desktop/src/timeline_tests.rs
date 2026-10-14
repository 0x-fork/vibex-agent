use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use vibex_core::TimelineTurnStart;

struct LiveTimelineProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for LiveTimelineProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = self.workbench.update(cx, |workbench, cx| {
            workbench.render_agent_workbench_for(false, window, cx)
        });
        div().w(px(600.0)).h(px(400.0)).child(timeline)
    }
}

fn live_timeline_item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_layout-{sequence}")).unwrap(),
        session_id: VibexSessionId::parse("session_layout").unwrap(),
        sequence,
        timestamp_ms: sequence,
        source: if matches!(payload, TimelinePayload::UserMessage(_)) {
            TimelineSource::User
        } else {
            TimelineSource::Agent
        },
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

fn progress_tool(sequence: i64, tool: usize) -> TimelineItem {
    live_timeline_item(
        sequence,
        TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
            tool_call_id: format!("tool-{tool}"),
            tool_name: "exec_command".into(),
            status: vibex_core::ToolCallStatus::Progress,
            summary: "Running command".into(),
            input_summary: Some("cargo check --locked".into()),
            output_summary: Some("Checking workspace".into()),
            raw_extension: None,
        }),
    )
}

fn live_tool_timeline(
    cx: &mut TestAppContext,
    windowed: bool,
) -> (Entity<VibexWorkbench>, &mut VisualTestContext) {
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            // Exercise the production timeline without booting a runtime or
            // reading the developer's persisted UI preferences.
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.reasoning_display_mode = if windowed {
                ReasoningDisplayMode::Timeline
            } else {
                ReasoningDisplayMode::LatestAtBottom
            };
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            workbench
                .pending_agent_turn_session_ids
                .insert(session_id.to_string());
            let mut items = vec![live_timeline_item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Check the workspace".into(),
                    ..Default::default()
                }),
            )];
            for tool in 0..24 {
                if windowed {
                    items.push(live_timeline_item(
                        items.len() as i64 + 1,
                        TimelinePayload::Reasoning(vibex_core::ReasoningPayload {
                            text: format!("Check package {tool}"),
                            is_final: true,
                        }),
                    ));
                }
                items.push(progress_tool(items.len() as i64 + 1, tool));
            }
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    (view.read_with(cx, |view, _| view.workbench.clone()), cx)
}

fn draw_live_timeline_frames(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    count: usize,
) -> Vec<(Pixels, Pixels)> {
    (0..count)
        .map(|_| {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            workbench.read_with(cx, |workbench, _| {
                (
                    workbench.timeline_row_sizes[0].height,
                    workbench.timeline_scroll.offset().y,
                )
            })
        })
        .collect()
}

fn apply_live_timeline_item(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    item: TimelineItem,
) {
    workbench.update(cx, |workbench, cx| {
        let session_id = item.session_id.clone();
        let sequence = item.sequence;
        assert!(
            workbench
                .apply_timeline_events_to_borrowed_view(
                    &session_id,
                    vec![TimelineLiveEvent {
                        session_id: session_id.clone(),
                        sequence,
                        item
                    }],
                    cx,
                )
                .0
        );
    });
}

#[gpui::test]
fn tool_progress_preserves_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        for following in [true, false] {
            if !following {
                workbench.update(cx, |workbench, _| {
                    workbench.timeline_follow.set_following_bottom(false);
                    let offset = workbench.timeline_scroll.offset();
                    workbench
                        .timeline_scroll
                        .set_offset(point(offset.x, offset.y / 2.0));
                });
            }
            let frames = draw_live_timeline_frames(&workbench, cx, 16);
            let before = *frames.last().unwrap();
            assert!(frames[12..].iter().all(|frame| *frame == before));
            workbench.read_with(cx, |workbench, _| {
                assert_eq!(
                    !workbench.timeline_process_unit_heights.is_empty(),
                    windowed
                );
            });
            let end = workbench.read_with(cx, |workbench, _| {
                workbench.timeline.authoritative_end_sequence.unwrap()
            });
            for sequence in end + 1..=end + 4 {
                apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 23));
                let frames = draw_live_timeline_frames(&workbench, cx, 4);
                assert!(
                    frames.iter().all(|frame| *frame == before),
                    "tool progress must not scroll unchanged content (windowed={windowed}, following={following}): {before:?} -> {frames:?}"
                );
            }
        }
    }
}

#[gpui::test]
fn tool_revisions_preserve_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let sequence = workbench.read_with(cx, |workbench, _| {
            workbench.timeline.authoritative_end_sequence.unwrap()
        });
        let mut revised = progress_tool(sequence, 23);
        let TimelinePayload::ToolCall(tool) = &mut revised.payload else {
            unreachable!();
        };
        tool.output_summary = Some("Checking workspace crates".into());
        apply_live_timeline_item(&workbench, cx, revised);
        let frames = draw_live_timeline_frames(&workbench, cx, 8);
        assert!(
            frames.iter().all(|frame| *frame == before),
            "a tool revision must not re-estimate unchanged geometry (windowed={windowed}): {before:?} -> {frames:?}"
        );
    }
}

#[gpui::test]
fn refreshed_tool_history_preserves_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let (updates, mut items, mode) = workbench.update(cx, |workbench, _| {
            (
                workbench.begin_timeline_load(),
                workbench.timeline.items.to_vec(),
                workbench.ui_state.session.reasoning_display_mode,
            )
        });
        let item = items.last_mut().unwrap();
        let TimelinePayload::ToolCall(tool) = &mut item.payload else {
            unreachable!();
        };
        tool.output_summary = Some("Checking workspace crates".into());
        let prepared = runtime
            .block_on(timeline_loading::PreparedTimeline::prepare(
                item.session_id.clone(),
                items,
                None,
                true,
                mode,
            ))
            .unwrap();
        workbench.update(cx, |workbench, _| {
            assert!(workbench.install_prepared_timeline(prepared, &mut updates.borrow_mut()));
            workbench.rebuild_timeline_sizes();
        });
        let frames = draw_live_timeline_frames(&workbench, cx, 8);
        assert!(
            frames.iter().all(|frame| *frame == before),
            "history refresh must retain measured geometry (windowed={windowed}): {before:?} -> {frames:?}"
        );
    }
}

#[gpui::test]
fn new_tool_content_grows_the_timeline_without_reversing_follow(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let sequence = workbench.read_with(cx, |workbench, _| {
            workbench.timeline.authoritative_end_sequence.unwrap() + 1
        });
        apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 24));
        let mut frames = vec![before];
        frames.extend(draw_live_timeline_frames(&workbench, cx, 8));
        assert!(
            frames.last().unwrap().0 > before.0,
            "new content must acquire its full height"
        );
        assert!(
            frames
                .windows(2)
                .all(|pair| pair[1].0 >= pair[0].0 && pair[1].1 <= pair[0].1),
            "new content must not reverse bottom follow (windowed={windowed}): {frames:?}"
        );
        workbench.read_with(cx, |workbench, _| {
            assert_eq!(
                workbench.timeline_scroll.offset().y,
                -workbench.timeline_scroll.max_offset().y
            );
        });
    }
}

#[gpui::test]
fn tool_progress_stays_stable_after_returning_to_a_cached_session(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let before = *draw_live_timeline_frames(&workbench, cx, 16)
        .last()
        .unwrap();
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let other_session = VibexSessionId::parse("session_other").unwrap();
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(other_session.clone());
        workbench.borrow_session_view(&other_session);
        workbench
            .timeline
            .replace_authoritative(other_session, Vec::new());
    });
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(session_id.clone());
        assert!(workbench.borrow_session_view(&session_id));
        workbench.refresh_borrowed_view_geometry();
    });
    let restored = draw_live_timeline_frames(&workbench, cx, 4);
    assert!(
        restored.iter().all(|frame| *frame == before),
        "restored geometry: {restored:?}"
    );
    let sequence = workbench.read_with(cx, |workbench, _| {
        workbench.timeline.authoritative_end_sequence.unwrap() + 1
    });
    apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 23));
    let frames = draw_live_timeline_frames(&workbench, cx, 4);
    assert!(
        frames.iter().all(|frame| *frame == before),
        "progress after restoring: {frames:?}"
    );
}

struct TimelinePaintFrame {
    row_bounds: Bounds<Pixels>,
    scroll_offset: Point<Pixels>,
}

struct TimelineBottomFollowProbe {
    session: SessionView,
    height: f32,
    follow: bool,
    painted: Rc<RefCell<Vec<TimelinePaintFrame>>>,
}

impl Render for TimelineBottomFollowProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = self.height;
        self.session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
        if self.follow {
            self.session.scroll_timeline_to_latest();
        }
        let painted = self.painted.clone();
        let scroll = self.session.timeline_scroll.clone();
        v_virtual_list(
            cx.entity(),
            "timeline-bottom-follow-probe",
            self.session.timeline_row_sizes.clone(),
            move |_, _, _, _| {
                let painted = painted.clone();
                let scroll = scroll.clone();
                vec![
                    div()
                        .debug_selector(|| "follow-turn".into())
                        .w_full()
                        .h(px(height))
                        .on_prepaint(move |bounds, _, _| {
                            painted.borrow_mut().push(TimelinePaintFrame {
                                row_bounds: bounds,
                                scroll_offset: scroll.offset(),
                            });
                        })
                        .into_any_element(),
                ]
            },
        )
        .w(px(500.0))
        .h(px(400.0))
        .py_4()
        .track_scroll(&self.session.timeline_scroll)
    }
}

#[gpui::test]
fn timeline_bottom_follow_paints_the_current_extent_in_the_same_frame(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let painted = Rc::new(RefCell::new(Vec::new()));
    let observed = painted.clone();
    let (view, cx) = cx.add_window_view(|_, _| TimelineBottomFollowProbe {
        session: SessionView::new(SessionContentWidthMode::Standard),
        height: 600.0,
        follow: false,
        painted,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for height in [600.0, 820.0, 520.0, 940.0, 400.0, 200.0, 760.0] {
        observed.borrow_mut().clear();
        view.update(cx, |view, cx| {
            view.height = height;
            view.follow = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (offset, bounds, inset, max_offset) = view.read_with(cx, |view, _| {
            let session = &view.session;
            (
                session.timeline_scroll.offset().y,
                session.timeline_scroll.bounds(),
                px(session.timeline_list_padding_top_px),
                session.timeline_scroll.max_offset().y,
            )
        });
        assert_eq!(offset, -max_offset, "height {height}");
        assert!(!observed.borrow().is_empty());
        for frame in observed.borrow().iter() {
            assert_eq!(
                frame.row_bounds.top(),
                bounds.top() + inset + frame.scroll_offset.y,
                "the row and scrollbar must use the same offset at height {height}"
            );
            if max_offset > px(0.0) {
                assert_eq!(
                    frame.row_bounds.bottom(),
                    bounds.bottom() - inset,
                    "height {height}"
                );
            }
        }
    }
}

struct ReasoningTimelineProbe {
    session: SessionView,
    unit_count: usize,
    windowed_reasoning: bool,
    measured_units: Rc<RefCell<BTreeMap<usize, f32>>>,
    measured_turn: Rc<Cell<Option<f32>>>,
    offsets: Rc<RefCell<Vec<Pixels>>>,
}

#[test]
fn reasoning_toggles_retain_geometry_and_release_only_the_changed_layout() {
    for row_id in ["reasoning", "reasoning-live:active"] {
        let mut view = SessionView::new(SessionContentWidthMode::Standard);
        view.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(900.0))]);
        view.timeline_measured_turn_heights
            .insert("active".into(), 900.0);
        view.timeline_measured_turn_layout_signatures
            .insert("active".into(), 1);
        let measured = TimelineProcessUnitHeight {
            revision: 1,
            layout_width: Some(500.0),
            layout_invalidated: false,
            height: 240.0,
        };
        view.timeline_process_unit_heights
            .insert("history".into(), measured);
        if row_id == "reasoning" {
            view.timeline_process_unit_heights
                .insert(row_id.into(), measured);
        }
        let sizes = view.timeline_row_sizes.clone();
        view.set_reasoning_expanded(row_id.into(), Some("active"), false);
        assert!(Rc::ptr_eq(&view.timeline_row_sizes, &sizes));
        assert_eq!(view.timeline_measured_turn_heights["active"], 900.0);
        assert_eq!(view.timeline_process_unit_heights["history"], measured);
        assert!(
            !view
                .timeline_measured_turn_layout_signatures
                .contains_key("active")
        );
        if let Some(unit) = view.timeline_process_unit_heights.get(row_id) {
            assert_eq!(
                cached_timeline_process_unit_height(Some(unit), 2, true),
                Some(240.0)
            );
            let collapsed =
                settle_timeline_process_unit_height(Some(*unit), 2, 20.0, true, Some(500.0));
            assert_eq!(
                collapsed.height, 20.0,
                "a manual collapse bypasses the streaming hold"
            );
            assert!(!collapsed.layout_invalidated);
        }
        // The first nested prepaint still reserves the old run height. Its
        // provisional fingerprint must not block the following real collapse.
        let signature = measured_timeline_layout_signature(Some(900.0), 900.0, None, Some(2));
        assert_eq!(signature, None);
        assert_eq!(
            stable_streaming_timeline_height_for_layout(
                Some(900.0),
                680.0,
                true,
                signature,
                Some(2)
            ),
            680.0
        );
        let signature = measured_timeline_layout_signature(Some(900.0), 680.0, signature, Some(2));
        assert_eq!(signature, Some(2));
        assert_eq!(
            stable_streaming_timeline_height_for_layout(
                Some(680.0),
                660.0,
                true,
                signature,
                Some(2)
            ),
            680.0
        );
    }
}

impl Render for ReasoningTimelineProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.session.reasoning_expansion["reasoning"];
        for (index, measured) in std::mem::take(&mut *self.measured_units.borrow_mut()) {
            let id = format!("unit-{index}");
            let id = if index + 1 == self.unit_count {
                "reasoning"
            } else {
                &id
            };
            let settled = settle_timeline_process_unit_height(
                self.session.timeline_process_unit_heights.get(id).copied(),
                1,
                measured,
                index + 1 == self.unit_count,
                Some(500.0),
            );
            self.session
                .timeline_process_unit_heights
                .insert(id.into(), settled);
        }
        if let Some(measured) = self.measured_turn.take() {
            let previous = self
                .session
                .timeline_measured_turn_heights
                .get("active")
                .copied();
            let previous_signature = self
                .session
                .timeline_measured_turn_layout_signatures
                .get("active")
                .copied();
            let signature = Some(u64::from(expanded));
            let height = stable_streaming_timeline_height_for_layout(
                previous,
                measured,
                true,
                previous_signature,
                signature,
            );
            if let Some(signature) =
                measured_timeline_layout_signature(previous, height, previous_signature, signature)
            {
                self.session
                    .timeline_measured_turn_layout_signatures
                    .insert("active".into(), signature);
            }
            self.session
                .timeline_measured_turn_heights
                .insert("active".into(), height);
            self.session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
        }
        if self.session.timeline_follow.following_bottom {
            self.session.scroll_timeline_to_latest();
        }
        let units = Rc::new(
            (0..self.unit_count)
                .map(|index| {
                    let id = if index + 1 == self.unit_count {
                        "reasoning".into()
                    } else {
                        format!("unit-{index}")
                    };
                    TimelineProcessUnit {
                        estimated_height: self.session.timeline_process_unit_heights[&id].height,
                        id,
                        rows: index..index + 1,
                        revision: 1,
                        streaming: index + 1 == self.unit_count,
                        kind: TimelineProcessUnitKind::Row,
                    }
                })
                .collect::<Vec<_>>(),
        );
        let mut total = px(0.0);
        let origins = Arc::new(
            units
                .iter()
                .map(|unit| {
                    let origin = total;
                    total += px(unit.estimated_height + TIMELINE_PROCESS_UNIT_GAP);
                    origin
                })
                .collect::<Vec<_>>(),
        );
        total -= px(TIMELINE_PROCESS_UNIT_GAP);
        let windowed_reasoning = self.windowed_reasoning;
        let unit_count = self.unit_count;
        let measured_units = self.measured_units.clone();
        let measured_turn = self.measured_turn.clone();
        let offsets = self.offsets.clone();
        let scroll = self.session.timeline_scroll.clone();
        let row_height = self.session.timeline_row_sizes[0].height;
        let entity = cx.weak_entity();
        v_virtual_list(
            cx.entity(),
            "reasoning-timeline-probe",
            self.session.timeline_row_sizes.clone(),
            move |_, _, window, cx| {
                let build_entity = entity.clone();
                let units_for_builder = units.clone();
                let measured_units = measured_units.clone();
                let build_unit = move |index: usize, _: &mut Window, cx: &mut App| {
                    let content = if index + 1 == unit_count {
                        let control = if expanded {
                            render_reasoning_first_line_layout(
                                cx.theme().muted_foreground,
                                div().h(px(20.0)).child("Thinking").into_any_element(),
                                Some(div().h(px(280.0)).flex_none().into_any_element()),
                                windowed_reasoning.then_some(REASONING_WINDOW_HEIGHT),
                                ReasoningReveal::new("reasoning".into(), true, 1.0),
                            )
                        } else {
                            div().w_full().h(px(20.0)).child("Thinking")
                        };
                        let entity = build_entity.clone();
                        control
                            .id("toggle-reasoning")
                            .debug_selector(|| "toggle-reasoning".into())
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.measured_units
                                        .borrow_mut()
                                        .remove(&(this.unit_count - 1));
                                    this.measured_turn.set(None);
                                    this.session.set_reasoning_expanded(
                                        "reasoning".into(),
                                        Some("active"),
                                        !expanded,
                                    );
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    } else {
                        div().h(px(180.0)).flex_none().into_any_element()
                    };
                    let measured_units = measured_units.clone();
                    let entity = build_entity.clone();
                    let cached_height = units_for_builder[index].estimated_height;
                    div()
                        .w_full()
                        .flex_none()
                        .on_prepaint(move |bounds, _, cx| {
                            let height = f32::from(bounds.size.height).ceil();
                            measured_units.borrow_mut().insert(index, height);
                            if (height - cached_height).abs() >= 1.0 {
                                let _ = entity.update(cx, |_, cx| cx.notify());
                            }
                        })
                        .child(content)
                        .into_any_element()
                };
                let content = if unit_count > TIMELINE_PROCESS_RUN_FLOW_LIMIT {
                    TimelineProcessRun {
                        units: units.clone(),
                        origins: origins.clone(),
                        total_height: total,
                        pinned_unit: None,
                        session_id: None,
                        entity: WeakEntity::new_invalid(),
                        build_unit: Box::new(build_unit),
                    }
                    .into_any_element()
                } else {
                    v_flex()
                        .w_full()
                        .gap(px(TIMELINE_PROCESS_UNIT_GAP))
                        .children((0..unit_count).map(|index| build_unit(index, window, cx)))
                        .into_any_element()
                };
                let measured_turn = measured_turn.clone();
                let offsets = offsets.clone();
                let scroll = scroll.clone();
                let entity = entity.clone();
                vec![
                    h_flex()
                        .w_full()
                        .h(row_height)
                        .items_start()
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .w_full()
                                .flex_none()
                                .on_prepaint(move |bounds, _, cx| {
                                    let height = f32::from(bounds.size.height).ceil();
                                    measured_turn.set(Some(height));
                                    offsets.borrow_mut().push(scroll.offset().y);
                                    if (height - f32::from(row_height)).abs() >= 1.0 {
                                        let _ = entity.update(cx, |_, cx| cx.notify());
                                    }
                                })
                                .child(content)
                                .child(div().h(px(300.0)).flex_none()),
                        )
                        .into_any_element(),
                ]
            },
        )
        .w(px(500.0))
        .h(px(700.0))
        .py_4()
        .track_scroll(&self.session.timeline_scroll)
    }
}

#[gpui::test]
fn reasoning_disclosures_preserve_history_and_never_reverse_bottom_follow(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for unit_count in [4, 20] {
        for windowed_reasoning in [true, false] {
            for following_bottom in [true, false] {
                let offsets = Rc::new(RefCell::new(Vec::new()));
                let observed = offsets.clone();
                let (view, cx) = cx.add_window_view(|_, _| {
                    let mut session = SessionView::new(SessionContentWidthMode::Standard);
                    session.agent_turn_pending = true;
                    session.reasoning_expansion.insert("reasoning".into(), true);
                    for index in 0..unit_count {
                        let id = if index + 1 == unit_count {
                            "reasoning".into()
                        } else {
                            format!("unit-{index}")
                        };
                        session.timeline_process_unit_heights.insert(
                            id,
                            TimelineProcessUnitHeight {
                                revision: 1,
                                layout_width: Some(500.0),
                                layout_invalidated: false,
                                height: if index + 1 == unit_count {
                                    // Header plus the window, or the whole body.
                                    if windowed_reasoning { 152.0 } else { 300.0 }
                                } else {
                                    180.0
                                },
                            },
                        );
                    }
                    let height = session
                        .timeline_process_unit_heights
                        .values()
                        .map(|unit| unit.height)
                        .sum::<f32>()
                        + (unit_count - 1) as f32 * TIMELINE_PROCESS_UNIT_GAP
                        + 300.0;
                    session
                        .timeline_measured_turn_heights
                        .insert("active".into(), height);
                    session
                        .timeline_measured_turn_layout_signatures
                        .insert("active".into(), 1);
                    session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
                    ReasoningTimelineProbe {
                        session,
                        unit_count,
                        windowed_reasoning,
                        measured_units: Rc::new(RefCell::new(BTreeMap::new())),
                        measured_turn: Rc::new(Cell::new(None)),
                        offsets,
                    }
                });
                cx.run_until_parked();
                view.update(cx, |view, cx| {
                    view.session
                        .timeline_follow
                        .set_following_bottom(following_bottom);
                    if !following_bottom {
                        let offset = view.session.timeline_scroll.offset().y + px(290.0);
                        view.session
                            .timeline_scroll
                            .set_offset(point(px(0.0), offset));
                    }
                    cx.notify();
                });
                cx.run_until_parked();
                for expanded in [false, true, false, true] {
                    let before =
                        view.read_with(cx, |view, _| view.session.timeline_scroll.offset().y);
                    observed.borrow_mut().clear();
                    let trigger = cx.debug_bounds("toggle-reasoning").unwrap();
                    cx.simulate_click(
                        trigger.origin + point(px(40.0), px(10.0)),
                        Modifiers::none(),
                    );
                    // Nested unit measurement and the enclosing virtual turn
                    // each settle in the following render. Observe every one.
                    for _ in 0..4 {
                        cx.update(|window, cx| {
                            window.refresh();
                            let _ = window.draw(cx);
                        });
                    }
                    let after = view.read_with(cx, |view, _| {
                        assert_eq!(view.session.reasoning_expansion["reasoning"], expanded);
                        let height = view.session.timeline_measured_turn_heights["active"];
                        assert_eq!(view.session.timeline_row_sizes[0].height, px(height));
                        assert_eq!(view.session.timeline_process_unit_heights.len(), unit_count);
                        view.session.timeline_scroll.offset().y
                    });
                    let mut previous = before;
                    assert!(!observed.borrow().is_empty());
                    for offset in observed.borrow().iter().copied() {
                        if following_bottom {
                            assert!(
                                offset >= before.min(after) && offset <= before.max(after),
                                "offset {offset:?} outside {before:?}..{after:?}"
                            );
                            assert!(
                                if expanded {
                                    offset <= previous
                                } else {
                                    offset >= previous
                                },
                                "reasoning toggle reversed the scroll direction"
                            );
                        } else {
                            assert_eq!(offset, before, "reading history must keep its position");
                        }
                        previous = offset;
                    }
                    if following_bottom {
                        assert!(
                            if expanded {
                                after < before
                            } else {
                                after > before
                            },
                            "the disclosure must reclaim or grow its extent: units {unit_count}, windowed {windowed_reasoning}, expanded {expanded}, before {before:?}, after {after:?}"
                        );
                    }
                }
            }
        }
    }
}

struct SidebarTimelineProbe {
    sidebar_width: f32,
    scroll: VirtualListScrollHandle,
    content_max_width: Option<f32>,
    rem_size: f32,
}

impl SidebarTimelineProbe {
    fn row_sizes(&self) -> Rc<Vec<Size<Pixels>>> {
        let width = timeline_content_width(
            Some(1400.0 - self.sidebar_width),
            self.content_max_width,
            self.rem_size,
        );
        // The first turns have not been measured; the visible history retains
        // its intrinsic heights, just like the production timeline's table.
        let body =
            "A historical answer with enough text to need a width-dependent estimate. ".repeat(12);
        let estimate = estimated_markdown_body_height(&body, estimated_chars_per_line(width));
        Rc::new(
            (0..10)
                .map(|index| size(px(width), px(if index < 4 { estimate } else { 120.0 })))
                .collect(),
        )
    }
}

impl Render for SidebarTimelineProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.rem_size));
        let row_sizes = self.row_sizes();
        let rendered_sizes = row_sizes.clone();
        let content_max_width = self.content_max_width;
        h_flex()
            .w(px(1400.0))
            .h(px(700.0))
            .items_stretch()
            .child(div().w(px(self.sidebar_width)).flex_none())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            "sidebar-resize-timeline",
                            row_sizes,
                            move |_, range, _, _| {
                                range
                                    .map(|index| {
                                        h_flex()
                                            .w_full()
                                            .h(rendered_sizes[index].height)
                                            .items_start()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .debug_selector(move || format!("resize-row-{index}"))
                                                    .w_full()
                                                    .when_some(content_max_width, |this, width| this.max_w(px(width)))
                                                    .h(px(120.0))
                                                    .child("A timeline row that fits at either sidebar width"),
                                            )
                                            .into_any_element()
                                    })
                                    .collect()
                            },
                        )
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .track_scroll(&self.scroll)
                        .px_4()
                        .py_4()
                        .gap_0(),
                    )
                    .child(div().h(px(100.0)).flex_none()),
            )
    }
}

#[gpui::test]
fn sidebar_width_changes_preserve_timeline_vertical_position(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let scroll = VirtualListScrollHandle::new();
    let scroll_for_view = scroll.clone();
    let (view, cx) = cx.add_window_view(|_, _| SidebarTimelineProbe {
        sidebar_width: 0.0,
        scroll: scroll_for_view,
        content_max_width: Some(AGENT_CONTENT_STANDARD_MAX_WIDTH),
        rem_size: 16.0,
    });
    for (content_width, rem_size) in [
        (SessionContentWidthMode::Standard, 16.0),
        (SessionContentWidthMode::Narrow, 16.0),
        (SessionContentWidthMode::Standard, 20.0),
        (SessionContentWidthMode::Narrow, 14.4),
    ] {
        view.update(cx, |view, cx| {
            view.sidebar_width = 0.0;
            view.content_max_width = session_content_max_width(content_width);
            view.rem_size = rem_size;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let history_offset = view.read_with(cx, |view, _| {
            view.row_sizes()
                .iter()
                .take(4)
                .map(|size| size.height)
                .sum::<Pixels>()
                + px(60.0)
        });
        for (offset, selector) in [
            (-history_offset, "resize-row-6"),
            (-scroll.max_offset().y, "resize-row-8"),
            (px(0.0), "resize-row-0"),
        ] {
            scroll.set_offset(point(px(0.0), offset));
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let before = cx.debug_bounds(selector).unwrap();
            let initial_offset = scroll.offset().y;
            let initial_max = scroll.max_offset().y;
            for width in [0.0, 20.0, 140.0, 220.0, 280.0, 210.0, 100.0, 0.0] {
                view.update(cx, |view, cx| {
                    view.sidebar_width = width;
                    cx.notify();
                });
                cx.update(|window, cx| {
                    let _ = window.draw(cx);
                });
                let after = cx.debug_bounds(selector).unwrap();
                assert_eq!(
                    after.size.width, before.size.width,
                    "the content still fits"
                );
                assert_eq!(
                    after.top(),
                    before.top(),
                    "sidebar width {width}, rem {rem_size}, {selector}"
                );
                assert_eq!(scroll.offset().y, initial_offset, "sidebar width {width}");
                assert_eq!(scroll.max_offset().y, initial_max, "sidebar width {width}");
            }
        }
    }
}

#[test]
fn timeline_width_follows_wrapping_insets_and_content_caps() {
    for rem_size in [14.4, 16.0, 20.0] {
        for cap in [
            AGENT_CONTENT_NARROW_MAX_WIDTH,
            AGENT_CONTENT_STANDARD_MAX_WIDTH,
        ] {
            assert_eq!(
                timeline_content_width(Some(1400.0), Some(cap), rem_size),
                cap
            );
            assert_eq!(
                timeline_content_width(Some(1120.0), Some(cap), rem_size),
                cap
            );
            assert_eq!(timeline_content_width(None, Some(cap), rem_size), cap);
        }
        for cap in [Some(AGENT_CONTENT_STANDARD_MAX_WIDTH), None] {
            assert_eq!(
                timeline_content_width(Some(600.0), cap, rem_size),
                600.0 - 2.0 * rem_size
            );
        }
        assert_eq!(
            timeline_content_width(Some(1400.0), None, rem_size),
            1400.0 - 2.0 * rem_size
        );
    }
    assert_eq!(
        timeline_content_width(None, None, 16.0),
        AGENT_CONTENT_STANDARD_MAX_WIDTH
    );
}

struct ProcessRunProbe {
    expanded: bool,
    bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl Render for ProcessRunProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        let first_height = if self.expanded { 160.0 } else { 24.0 };
        let units = (0..3)
            .map(|index| TimelineProcessUnit {
                rows: index..index + 1,
                id: format!("event-{index}"),
                revision: 1,
                streaming: false,
                estimated_height: 24.0,
                kind: TimelineProcessUnitKind::Row,
            })
            .collect();
        v_flex()
            .w(px(400.0))
            .h(px(400.0))
            .child(
                div().debug_selector(|| "toggle-process".into()).child(
                    Button::new("toggle").label("Details").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.expanded = !this.expanded;
                            cx.notify();
                        },
                    )),
                ),
            )
            .child(TimelineProcessRun {
                units: Rc::new(units),
                origins: Arc::new(vec![px(0.0), px(36.0), px(72.0)]),
                total_height: px(300.0),
                pinned_unit: None,
                session_id: None,
                entity: WeakEntity::new_invalid(),
                build_unit: Box::new(move |index, _, _| {
                    let bounds = bounds.clone();
                    div()
                        .w_full()
                        .h(px(if index == 0 { first_height } else { 24.0 }))
                        .on_prepaint(move |value, _, _| bounds.borrow_mut()[index] = value)
                        .into_any_element()
                }),
            })
    }
}

#[gpui::test]
fn expanding_and_collapsing_process_events_never_overlap_siblings(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let bounds = Rc::new(RefCell::new(vec![Bounds::default(); 3]));
    let observed = bounds.clone();
    let (_, cx) = cx.add_window_view(|_, _| ProcessRunProbe {
        expanded: false,
        bounds,
    });
    cx.run_until_parked();
    for expected_height in [160.0, 24.0, 160.0, 24.0] {
        let trigger = cx.debug_bounds("toggle-process").unwrap();
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = observed.borrow();
        assert_eq!(bounds[0].size.height, px(expected_height));
        for pair in bounds.windows(2) {
            assert_eq!(
                pair[1].top() - pair[0].bottom(),
                px(TIMELINE_PROCESS_UNIT_GAP)
            );
        }
    }
}

#[test]
fn an_offscreen_pinned_process_event_keeps_its_own_origin() {
    assert_eq!(
        timeline_process_measured_origin(20, px(900.0), Some((2, px(300.0)))),
        px(900.0)
    );
    assert_eq!(
        timeline_process_measured_origin(3, px(72.0), Some((2, px(300.0)))),
        px(300.0 + TIMELINE_PROCESS_UNIT_GAP)
    );
}

struct DisclosureProbe {
    progress: f32,
    height: f32,
    observed: Rc<Cell<f32>>,
}

impl Render for DisclosureProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let observed = self.observed.clone();
        v_flex().w(px(400.0)).child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| {
                    observed.set(f32::from(bounds.size.height));
                })
                .child(timeline_disclosure_body(
                    "probe-reveal".into(),
                    self.progress,
                    div().w_full().h(px(self.height)).into_any_element(),
                )),
        )
    }
}

#[gpui::test]
fn disclosures_measure_history_immediately_and_reverse_without_empty_frames(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let observed = Rc::new(Cell::new(0.0));
    let measured = observed.clone();
    let (view, cx) = cx.add_window_view(|_, _| DisclosureProbe {
        progress: 1.0,
        height: 120.0,
        observed,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        measured.get(),
        120.0,
        "history must reserve its real height on first paint"
    );
    for (progress, height, expected) in [
        (0.5, 120.0, 60.0),
        (0.25, 120.0, 30.0),
        (0.75, 120.0, 90.0),
        (1.0, 120.0, 120.0),
        (1.0, 240.0, 240.0),
    ] {
        view.update(cx, |view, cx| {
            view.progress = progress;
            view.height = height;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(measured.get(), expected);
    }
}

struct ReasoningRevealProbe {
    progress: f32,
    observed: Rc<Cell<f32>>,
}

impl Render for ReasoningRevealProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let observed = self.observed.clone();
        v_flex().w(px(400.0)).child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| {
                    observed.set(f32::from(bounds.size.height));
                })
                .child(render_reasoning_first_line_layout(
                    cx.theme().muted_foreground,
                    div().h(px(HEADER_HEIGHT)).into_any_element(),
                    Some(div().h(px(BODY_HEIGHT)).flex_none().into_any_element()),
                    None,
                    ReasoningReveal::new(
                        "reasoning-reveal-probe".into(),
                        self.progress > 0.0,
                        self.progress,
                    ),
                )),
        )
    }
}

/// The header an expanded reasoning row keeps whatever the reveal is doing.
const HEADER_HEIGHT: f32 = 20.0;
/// The body the reveal is expected to hold back until the spring reaches it.
const BODY_HEIGHT: f32 = 200.0;

#[gpui::test]
fn a_reasoning_body_reveals_on_the_same_spring_a_tool_detail_does(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let observed = Rc::new(Cell::new(0.0));
    let measured = observed.clone();
    let (view, cx) = cx.add_window_view(|_, _| ReasoningRevealProbe {
        progress: 1.0,
        observed,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        measured.get(),
        HEADER_HEIGHT + BODY_HEIGHT,
        "an open thought reserves its real height on the first paint"
    );
    // The header is the row's own line and never travels; everything under it
    // is the body, so the reveal's share of the row is exactly the spring.
    for (progress, expected) in [
        (0.5, HEADER_HEIGHT + BODY_HEIGHT * 0.5),
        (0.25, HEADER_HEIGHT + BODY_HEIGHT * 0.25),
        (1.0, HEADER_HEIGHT + BODY_HEIGHT),
        // The click that closes a row retargets the spring; the body keeps the
        // height it had on the frame the state flipped, so the collapse is
        // drawn instead of snapping away.
        (0.75, HEADER_HEIGHT + BODY_HEIGHT * 0.75),
        (0.0, HEADER_HEIGHT),
    ] {
        view.update(cx, |view, cx| {
            view.progress = progress;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(measured.get(), expected, "reveal at {progress}");
    }
}

/// A workbench laid out at a fixed size, so the composer's growth and the
/// conversation viewport above it can be measured against each other.
struct RailLaneProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for RailLaneProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.workbench.update(cx, |workbench, cx| {
            workbench.render_agent_workbench(window, cx)
        });
        div().w(px(900.0)).h(px(700.0)).child(page)
    }
}

/// The turn preview rail is a reading aid for the pane, not for whatever the
/// composer is currently showing.
///
/// Expanding a composer extension (a plan, a queue, a goal) grows the composer
/// and shortens the conversation viewport above it. The rail's lane must not
/// follow that: it belongs to the session page, whose height the composer
/// cannot change.
#[gpui::test]
fn composer_plan_growth_leaves_the_turn_preview_rail_its_lane(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_rail_lane").unwrap();
    let workbench_slot: Rc<RefCell<Option<Entity<VibexWorkbench>>>> = Rc::new(RefCell::new(None));
    let slot = workbench_slot.clone();
    let session_for_items = session_id.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let mut ui_state = DesktopUiStateV1::default();
        ui_state.appearance.locale = LocaleMode::En;
        ui_state.session.turn_preview_rail = true;
        let workbench = cx.new(|cx| {
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_for_items.clone());
            workbench.view_session_id = Some(session_for_items.clone());
            let item =
                |sequence: i64, payload: TimelinePayload, source: TimelineSource| TimelineItem {
                    id: TimelineItemId::parse(format!("timeline_rail_lane-{sequence}")).unwrap(),
                    session_id: session_for_items.clone(),
                    sequence,
                    timestamp_ms: sequence,
                    source,
                    kind: payload.kind(),
                    correlation_id: None,
                    provider_correlation_id: None,
                    redaction_state: TimelineRedactionState::None,
                    execution_attribution: None,
                    payload_depth: vibex_core::TimelinePayloadDepth::Detail,
                    payload,
                };
            let mut items = Vec::new();
            for turn in 0..5 {
                items.push(item(
                    items.len() as i64 + 1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: format!("Request {turn}"),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                    TimelineSource::User,
                ));
            }
            // The plan rides the last turn, which is the one the composer's
            // plan card mirrors.
            items.push(item(
                items.len() as i64 + 1,
                TimelinePayload::Plan(vibex_core::PlanPayload {
                    title: "Lane plan".into(),
                    steps: (0..9)
                        .map(|index| vibex_core::PlanStepPayload {
                            title: format!("Step {index}"),
                            status: vibex_core::PlanStepStatus::Pending,
                        })
                        .collect(),
                }),
                TimelineSource::Agent,
            ));
            workbench
                .timeline
                .replace_authoritative(session_for_items.clone(), items);
            workbench.rebuild_timeline_sizes();
            workbench
        });
        *slot.borrow_mut() = Some(workbench.clone());
        RailLaneProbe { workbench }
    });
    let workbench = workbench_slot
        .borrow()
        .clone()
        .expect("the probe should keep its workbench");

    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
    }

    draw(cx);
    let collapsed_rail = cx
        .debug_bounds("agent-turn-preview-rail")
        .expect("the rail should be laid out");
    let collapsed_viewport =
        workbench.read_with(cx, |workbench, _| workbench.timeline_scroll.bounds());
    let collapsed_tick = cx
        .debug_bounds("turn-preview-tick-0")
        .expect("the rail's first tick should be laid out");

    workbench.update(cx, |workbench, cx| {
        let items = workbench.timeline.items.clone();
        let plan = current_agent_plan(&items).expect("the plan item should project");
        let identity = composer_plan_identity(&session_id, &plan, &items);
        workbench.composer_plan_expanded = Some(identity);
        cx.notify();
    });
    draw(cx);
    let expanded_rail = cx
        .debug_bounds("agent-turn-preview-rail")
        .expect("the rail should still be laid out");
    let expanded_viewport =
        workbench.read_with(cx, |workbench, _| workbench.timeline_scroll.bounds());
    let expanded_tick = cx
        .debug_bounds("turn-preview-tick-0")
        .expect("the rail's first tick should still be laid out");

    assert!(
        (collapsed_viewport.size.height - expanded_viewport.size.height).abs() > px(1.0),
        "the expanded plan should shorten the conversation viewport: collapsed={:?} expanded={:?}",
        collapsed_viewport.size.height,
        expanded_viewport.size.height
    );
    assert_eq!(
        expanded_rail, collapsed_rail,
        "the rail's lane must not follow the composer's height"
    );
    assert_eq!(
        expanded_tick, collapsed_tick,
        "opening the plan panel must not move a tick the reader is aiming at"
    );
}

/// A child Agent rendered next to a conversation must not evict its Markdown.
///
/// The child Agent rail paints whole child timelines through the timeline's own
/// builders while the parent's view is borrowed. One retention table for both
/// let the rail's surfaces push the parent's out — the assistant messages the
/// reader is looking at — and a text view that is re-created is painted again
/// from scratch: with key-order eviction the surface that sorted first was
/// dropped on every frame, so a message on screen blinked for as long as the
/// child was open. The rail's surfaces are kept per child session now, and the
/// borrowed view's own table keeps the least recently painted surface.
#[gpui::test]
fn child_agent_rail_keeps_the_parent_timelines_markdown_surfaces(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let parent_id = VibexSessionId::parse("session_rail_parent").unwrap();
    let child_id = VibexSessionId::parse("session_rail_child").unwrap();
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let mut ui_state = DesktopUiStateV1::default();
        ui_state.appearance.locale = LocaleMode::En;
        let workbench = cx.new(|cx| {
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(parent_id.clone());
            workbench.view_session_id = Some(parent_id.clone());
            workbench
                .pending_agent_turn_session_ids
                .insert(parent_id.to_string());
            workbench
                .timeline
                .replace_authoritative(parent_id.clone(), rail_parent_items(3));
            workbench.rebuild_timeline_sizes();
            workbench
        });
        let mut child = ChildAgentTimelineState::new(child_id.clone());
        child
            .timeline
            .replace_authoritative(child_id.clone(), rail_child_items(210));
        child.loaded = true;
        workbench.update(cx, |workbench, _| {
            workbench
                .child_agent_timelines
                .insert(child_id.to_string(), child);
            workbench.child_agent_panel_active = true;
            workbench.child_agent_tabs = vec![child_id.clone()];
            workbench.child_agent_active_tab = Some(child_id.clone());
        });
        ChildRailProbe {
            workbench,
            rail: std::rc::Rc::new(std::cell::Cell::new(false)),
        }
    });
    let (workbench, rail) =
        probe.read_with(cx, |probe, _| (probe.workbench.clone(), probe.rail.clone()));

    fn draw(cx: &mut VisualTestContext, frames: usize) {
        for _ in 0..frames {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
    }

    draw(cx, 4);
    let parent = workbench.read_with(cx, |workbench, _| {
        markdown_surface_ids(&workbench.markdown_text_states)
    });
    assert_eq!(
        parent.len(),
        6,
        "the parent's own surfaces should be the only ones in its table: {parent:?}"
    );
    draw(cx, 2);
    let settled = workbench.read_with(cx, |workbench, _| {
        markdown_surface_ids(&workbench.markdown_text_states)
    });
    assert_eq!(parent, settled, "a settled conversation must not churn");

    rail.set(true);
    for frame in 0..5 {
        draw(cx, 1);
        let (now, rail_table) = workbench.read_with(cx, |workbench, _| {
            (
                markdown_surface_ids(&workbench.markdown_text_states),
                workbench
                    .child_agent_markdown_states
                    .get(child_id.as_str())
                    .map(markdown_surface_ids)
                    .unwrap_or_default(),
            )
        });
        for (id, state) in &parent {
            assert_eq!(
                now.get(id),
                Some(state),
                "frame {frame}: the rail re-created the parent's surface {id}"
            );
        }
        assert!(
            rail_table.len() <= MARKDOWN_TEXT_STATE_LIMIT,
            "the rail's own table stays bounded: {}",
            rail_table.len()
        );
        assert!(
            !rail_table.keys().any(|id| parent.contains_key(id)),
            "the rail may not keep the parent's surfaces at all"
        );
    }

    // Closing the tab drops everything the rail retained for that child.
    workbench.update(cx, |workbench, cx| {
        workbench.close_child_agent_tab(&child_id, cx);
    });
    draw(cx, 1);
    workbench.read_with(cx, |workbench, _| {
        assert!(
            !workbench
                .child_agent_markdown_states
                .contains_key(child_id.as_str()),
            "a closed child tab must not keep its retained text views"
        );
        assert_eq!(workbench.markdown_text_states.len(), 6);
    });
}

/// The Markdown table drops the surface its session stopped painting.
///
/// Key order was the previous rule, and it is not a recency order: filling the
/// table then repainting an early surface still evicted that surface, which is
/// the one thing a retained text view must not do while it is on screen.
#[gpui::test]
fn markdown_surface_table_evicts_the_least_recently_painted_surface(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = markdown_table_fixture(cx);
    workbench.update(cx, |workbench, cx| {
        let source: Arc<str> = Arc::from("Body");
        for index in 0..MARKDOWN_TEXT_STATE_LIMIT {
            let id = format!("markdown:surface-{index:03}");
            workbench.markdown_text_state(&id, &source, None, false, cx);
        }
        // Repaint the first surface: it is now the most recently painted one,
        // while `surface-001` is the oldest.
        workbench.markdown_text_state("markdown:surface-000", &source, None, false, cx);
        workbench.markdown_text_state("markdown:surface-192", &source, None, false, cx);
        assert!(
            workbench
                .markdown_text_states
                .contains_key("markdown:surface-000"),
            "a surface painted this frame must survive the next insertion"
        );
        assert!(
            !workbench
                .markdown_text_states
                .contains_key("markdown:surface-001"),
            "the least recently painted surface is the one that goes"
        );
        assert_eq!(
            workbench.markdown_text_states.len(),
            MARKDOWN_TEXT_STATE_LIMIT,
            "the bound still holds"
        );
    });
}

fn markdown_surface_ids(table: &BTreeMap<String, MarkdownTextState>) -> BTreeMap<String, EntityId> {
    table
        .iter()
        .map(|(id, entry)| (id.clone(), entry.state.entity_id()))
        .collect()
}

fn rail_parent_items(turns: usize) -> Vec<TimelineItem> {
    let session_id = VibexSessionId::parse("session_rail_parent").unwrap();
    let mut items = Vec::new();
    for turn in 0..turns {
        items.push(rail_item(
            &session_id,
            &format!("timeline_aaa{turn}u"),
            items.len() as i64 + 1,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: format!("Parent question {turn}"),
                ..Default::default()
            }),
        ));
        items.push(rail_item(
            &session_id,
            &format!("timeline_aaa{turn}a"),
            items.len() as i64 + 1,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: format!("Parent answer {turn} with some body text."),
                is_final: true,
            }),
        ));
    }
    items
}

fn rail_child_items(turns: usize) -> Vec<TimelineItem> {
    let session_id = VibexSessionId::parse("session_rail_child").unwrap();
    let mut items = Vec::new();
    for turn in 0..turns {
        for (suffix, payload) in [
            (
                "u",
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: format!("Child question {turn}"),
                    ..Default::default()
                }),
            ),
            (
                "a",
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: format!("Child answer {turn} with some body text."),
                    is_final: true,
                }),
            ),
        ] {
            items.push(rail_item(
                &session_id,
                &format!("timeline_zzz{turn}{suffix}"),
                items.len() as i64 + 1,
                payload,
            ));
        }
    }
    items
}

fn rail_item(
    session_id: &VibexSessionId,
    id: &str,
    sequence: i64,
    payload: TimelinePayload,
) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(id.to_string()).unwrap(),
        session_id: session_id.clone(),
        sequence,
        timestamp_ms: sequence,
        source: if matches!(payload, TimelinePayload::UserMessage(_)) {
            TimelineSource::User
        } else {
            TimelineSource::Agent
        },
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

struct ChildRailProbe {
    workbench: Entity<VibexWorkbench>,
    rail: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Render for ChildRailProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rail_open = self.rail.get();
        let (timeline, rail) = self.workbench.update(cx, |workbench, cx| {
            let timeline = workbench.render_agent_workbench_for(false, window, cx);
            let rail = rail_open.then(|| workbench.render_child_agent_timeline_panel(window, cx));
            (timeline, rail)
        });
        div()
            .flex()
            .w(px(1200.0))
            .h(px(700.0))
            .child(div().w(px(700.0)).h_full().child(timeline))
            .when_some(rail, |this, rail| {
                this.child(div().w(px(500.0)).h_full().child(rail))
            })
    }
}

fn markdown_table_fixture(
    cx: &mut TestAppContext,
) -> (Entity<VibexWorkbench>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            VibexWorkbench::with_initial_state(None, DesktopUiStateV1::default(), None, window, cx)
        });
        MarkdownTableProbe { workbench }
    });
    (view.read_with(cx, |probe, _| probe.workbench.clone()), cx)
}

struct MarkdownTableProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for MarkdownTableProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A page the reader's switch superseded must not keep its in-flight slot.
///
/// The older-history budget is per view and only a completion ever releases it.
/// A read that finishes while the reader is looking at another session used to
/// return before touching the count, so the parked view kept the slot for the
/// rest of its life: once two of those had accumulated it stopped prefetching
/// its own history at all, which is what a session that is switched to while it
/// runs looks like from the reader's side.
#[gpui::test]
fn a_history_page_a_switch_superseded_returns_its_slot(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_a = VibexSessionId::parse("session_layout").unwrap();
    let session_b = VibexSessionId::parse("session_switch_b").unwrap();
    let (workbench, cx) = live_tool_timeline(cx, true);
    let generation = workbench.update(cx, |workbench, _| {
        workbench.timeline_has_older = true;
        workbench.timeline_older_in_flight = 1;
        workbench.ensure_session_view(&session_b);
        workbench.session_generation
    });
    // The reader switches away while the page travels, so its answer arrives
    // for a view that is no longer the borrowed one.
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(session_b.clone());
        let _ = workbench.borrow_session_view(&session_b);
        workbench.finish_older_timeline_load(
            &session_a,
            4,
            generation,
            Ok(TimelinePage {
                session_id: session_a.clone(),
                items: Vec::new(),
                start_sequence: None,
                end_sequence: None,
                has_older: false,
                has_newer: false,
                starts_at_turn_start: true,
                leading_turn_start: None,
            }),
        );
    });
    let slots = workbench.read_with(cx, |workbench, _| {
        workbench
            .session_views
            .get(session_a.as_str())
            .map(|view| view.timeline_older_in_flight)
    });
    assert_eq!(
        slots,
        Some(0),
        "the parked view must not keep the slot its superseded page spent"
    );
}

/// A prefetch that could not start a read keeps the whole budget.
///
/// The slot is spent for a page that is really travelling; spending one for a
/// request that never left would leave the reader unable to ask for history,
/// because only a completion can give the slot back.
#[gpui::test]
fn a_prefetch_that_cannot_start_keeps_the_history_budget(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    // Park the reader at the top of a transcript that claims older history,
    // with no authority to read it from.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_has_older = true;
        workbench.agent_loading = false;
        workbench.timeline_follow.set_following_bottom(false);
        workbench
            .timeline_scroll
            .set_offset(point(px(0.0), px(0.0)));
    });
    // The precondition is asserted rather than assumed: an empty viewport would
    // hold the prefetch back on its own and the test would pass without a fix.
    let ready = workbench.read_with(cx, |workbench, _| {
        timeline_should_prefetch_older(TimelineOlderPrefetch {
            distance_to_top: workbench.timeline_distance_to_top(),
            viewport_height: f32::from(workbench.timeline_scroll.bounds().size.height),
            has_older: workbench.timeline_has_older,
            in_flight: workbench.timeline_older_in_flight,
            following_bottom: workbench.timeline_follow.following_bottom,
            scrollable: workbench.timeline_scroll.max_offset().y > px(0.0),
            loading: workbench.agent_loading,
        })
    });
    assert!(
        ready,
        "the fixture must be in the prefetching state for this test to mean anything"
    );
    workbench.update(cx, |workbench, cx| {
        for _ in 0..4 {
            workbench.prefetch_older_timeline_if_needed(cx);
        }
    });
    let slots = workbench.read_with(cx, |workbench, _| workbench.timeline_older_in_flight);
    assert_eq!(
        slots, 0,
        "a read that never started must not spend a slot that only a completion can release"
    );
}

/// A turn whose own first row has not arrived reads as loading, not as work.
///
/// The rows a later read adds to that turn are older than the ones on screen, so
/// a duration built from the rows already here would move under the reader. The
/// view says so instead, and stops reporting a turn in progress: the tick that
/// would keep the duration moving has nothing left to move.
#[gpui::test]
fn a_turn_that_is_still_arriving_reads_as_loading(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    // Keep only the response rows: what a window shows when its budget ran out
    // inside one long turn, with that turn's first row still below the window.
    workbench.update(cx, |workbench, _| {
        let session_id = workbench.timeline.session_id.clone().unwrap();
        let tail = workbench
            .timeline
            .items
            .iter()
            .filter(|item| item.kind != vibex_core::TimelineItemKind::UserMessage)
            .cloned()
            .collect::<Vec<_>>();
        workbench.timeline.replace_authoritative(session_id, tail);
        workbench.timeline_starts_mid_turn = true;
        workbench.conversation_turns_cache_key = None;
        workbench.timeline_row_sizes = Rc::new(Vec::new());
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let leading = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench
                .conversation_turns_cache
                .first()
                .map(|turn| turn.user_row.is_none()),
            workbench.conversation_turns_summary.has_incomplete_turn,
        )
    });
    assert_eq!(
        leading,
        (true, Some(true), false),
        "the arriving turn is the leading one, and it is not work in progress"
    );

    // Once the page that carries its first row lands, the view is a window of
    // whole turns again and the turn reads as work with a duration.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_starts_mid_turn = false;
        workbench.conversation_turns_cache_key = None;
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let loaded = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.conversation_turns_summary.has_incomplete_turn,
        )
    });
    assert_eq!(loaded, (false, true));

    // The read that cut the window can still place the turn it opened inside.
    // The header then counts from the turn's own first row instead of from the
    // row the budget stopped at: the label is for a turn nothing can place, not
    // for rows that have not been read back yet.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_starts_mid_turn = true;
        workbench.timeline.leading_turn_start = Some(TimelineTurnStart {
            sequence: 1,
            timestamp_ms: 1,
        });
        workbench.conversation_turns_cache_key = None;
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let placed = workbench.read_with(cx, |workbench, _| {
        let leading = workbench
            .conversation_turns_cache
            .first()
            .expect("the arriving turn is the cached leading turn");
        (leading.user_row.is_none(), leading.started_at_ms)
    });
    assert_eq!(
        placed,
        (true, 1),
        "the leading turn is dated from the read that cut the window"
    );
}

/// The boundary comes from the read that produced the page, not from what the
/// rows happen to look like.
#[gpui::test]
fn an_older_page_moves_the_views_leading_end(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_a = VibexSessionId::parse("session_layout").unwrap();
    let (workbench, cx) = live_tool_timeline(cx, true);
    let generation = workbench.update(cx, |workbench, _| workbench.session_generation);
    let items = workbench.read_with(cx, |workbench, _| workbench.timeline.items.clone());
    let page =
        |starts_at_turn_start: bool, leading_turn_start: Option<TimelineTurnStart>| TimelinePage {
            session_id: session_a.clone(),
            items: items.clone(),
            start_sequence: items.first().map(|item| item.sequence),
            end_sequence: items.last().map(|item| item.sequence),
            has_older: true,
            has_newer: false,
            starts_at_turn_start,
            leading_turn_start,
        };
    // The page's items all sit below the cursor it was read for.
    let cursor = items.last().map(|item| item.sequence).unwrap_or(0) + 1;
    let head = TimelineTurnStart {
        sequence: 1,
        timestamp_ms: 1,
    };
    workbench.update(cx, |workbench, _| {
        workbench.finish_older_timeline_load(
            &session_a,
            cursor,
            generation,
            Ok(page(false, Some(head))),
        );
    });
    let leading = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.timeline.leading_turn_start,
        )
    });
    assert_eq!(
        leading,
        (true, Some(head)),
        "a page that opens inside a turn carries where that turn begins"
    );
    workbench.update(cx, |workbench, _| {
        workbench.finish_older_timeline_load(&session_a, cursor, generation, Ok(page(true, None)));
    });
    let landed = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.timeline.leading_turn_start,
        )
    });
    assert_eq!(
        landed,
        (false, None),
        "a page that opens on a boundary finishes the turn above it"
    );
}

/// Draws frames the way the platform does when nothing has asked for a full
/// refresh, which is the state a retained element tree is reused in.
///
/// `Window::refresh` deliberately ignores every cached subtree, so a frame that
/// wants to see the reuse has to be drawn without it.
fn draw_plain_timeline_frames(cx: &mut VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

/// Whether a hover change and a click reach an element inside a cached view.
///
/// A retained process unit is mounted through `Entity::cached`, and the fork
/// replays a cached view's paint range instead of painting it again. This probe
/// isolates that from the timeline: the same child view is mounted plainly in one
/// pass and cached in the next, and both are hovered and clicked in the same
/// place.
struct CachedClickChild {
    clicks: Rc<std::cell::Cell<usize>>,
    renders: Rc<std::cell::Cell<usize>>,
}

impl Render for CachedClickChild {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let clicks = self.clicks.clone();
        div()
            .debug_selector(|| "cached-click-child".to_string())
            .w_full()
            .h(px(40.0))
            .hover(|style| style.opacity(0.5))
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                clicks.set(clicks.get() + 1)
            })
    }
}

struct CachedClickProbe {
    child: Entity<CachedClickChild>,
    cached: bool,
}

impl Render for CachedClickProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.cached {
            div()
                .size_full()
                .child(
                    self.child
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .child(self.child.clone())
                .into_any_element()
        }
    }
}

#[gpui::test]
fn a_click_and_a_hover_reach_an_element_inside_a_cached_view(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for cached in [false, true] {
        let clicks = Rc::new(std::cell::Cell::new(0));
        let renders = Rc::new(std::cell::Cell::new(0));
        let observed_clicks = clicks.clone();
        let observed_renders = renders.clone();
        let (_, cx) = cx.add_window_view(|_, cx| CachedClickProbe {
            child: cx.new(|_| CachedClickChild { clicks, renders }),
            cached,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx
            .debug_bounds("cached-click-child")
            .expect("the child paints");
        let before_hover = observed_renders.get();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        if !cached {
            assert!(
                observed_renders.get() > before_hover,
                "a hover change must reach a plainly mounted view"
            );
        }
        // A cached view is not notified when the hover changes, so its hover
        // wash goes stale until something else renders it. Recorded rather than
        // asserted: a retained unit cannot rely on hover to repaint itself, which
        // is one of the two reasons the retention was reverted.
        let hovered_through_cache = observed_renders.get() > before_hover;
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            observed_clicks.get(),
            1,
            "a click inside a cached view must reach its listener (cached={cached}, hover-through-cache={hovered_through_cache})"
        );
    }
}

/// The spike behind [`timeline_group_body`]: an expanded group's body lays out
/// only the rows the viewport shows, so a session that ran for hours stops
/// paying for the rows nobody can see.
///
/// The number this asserts is the one the measurement needs: the body must stop
/// building every row, and the rows it does build must be the band the viewport
/// is over — not the leading rows of the group.
#[gpui::test]
fn an_expanded_group_body_builds_only_the_rows_the_viewport_shows(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    const TOOL_ROWS: usize = 240;
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            let mut items = vec![live_timeline_item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Check the workspace".into(),
                    ..Default::default()
                }),
            )];
            for tool in 0..TOOL_ROWS {
                items.push(progress_tool(items.len() as i64 + 1, tool));
            }
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            // Open the single group the run projects before the first frame, so
            // the disclosure starts settled and its body is on screen.
            let group_id = workbench
                .conversation_turns_cache
                .iter()
                .find_map(|turn| {
                    turn.process_activity_groups_with_commands_and_file_operations
                        .first()
                        .map(|group| group.id.clone())
                })
                .expect("the tool run projects one activity group");
            workbench.timeline_command_expansion.insert(group_id, true);
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());

    let mut built = Vec::new();
    for _ in 0..4 {
        workbench.update(cx, |workbench, cx| {
            workbench.timeline_group_rows_built.clear();
            cx.notify();
        });
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let frame = workbench.read_with(cx, |workbench, _| {
            workbench.timeline_group_rows_built.clone()
        });
        if !frame.is_empty() {
            built = frame;
        }
    }
    let group_rows = workbench.read_with(cx, |workbench, _| {
        workbench
            .conversation_turns_cache
            .iter()
            .find_map(|turn| {
                turn.process_activity_groups_with_commands_and_file_operations
                    .first()
                    .map(|group| group.end_row - group.start_row)
            })
            .unwrap_or_default()
    });
    assert!(
        group_rows > 100,
        "the fixture should project a long run, got {group_rows} rows"
    );
    // One update can draw twice — the first correction of the row table notifies
    // the workbench and the test harness draws the dirty window again — so the
    // rows the window chose are what the assertions read, not the call count.
    built.sort_unstable();
    built.dedup();
    assert!(
        !built.is_empty(),
        "the settled body must build the rows the viewport shows"
    );
    // The band is the 400px viewport plus the row window's own slack on each
    // edge — about twenty 36px rows. The bound leaves room for the rows to
    // measure shorter or taller, and still fails if the window falls back to
    // the unit run's 640px overscan, which alone would reach eighteen rows.
    assert!(
        built.len() <= 32,
        "the windowed body built {} of {group_rows} rows: {built:?}",
        built.len()
    );
    let first = built.iter().copied().min().unwrap_or_default();
    let last = built.iter().copied().max().unwrap_or_default();
    assert_eq!(
        built.len(),
        last - first + 1,
        "the body builds one contiguous window: {built:?}"
    );
    // The fixture follows the newest output, so the window must sit at the end
    // of the group rather than on its leading rows.
    assert_eq!(
        last + 1,
        group_rows,
        "the window should reach the last row, built {built:?}"
    );
}
