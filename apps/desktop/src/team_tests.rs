use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use std::sync::{Mutex, mpsc as sync_mpsc};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendEventSubscription, BackendFuture, DisconnectedBackend,
};
use vibex_core::*;
use vibex_db::{
    GroupPresentationRepository, SessionOwnershipRepository, SessionRepository,
    WorkspaceRepository, open_database,
};

enum Observed {
    Message(HumanAgentMessageRequest),
    Permission(ResolvePermissionRequest),
    Timeline(VibexSessionId),
    Delegation(MutationRequest<HumanDelegationRequest>),
}

struct TeamAgent {
    sessions: BTreeMap<String, AgentSession>,
    observed: sync_mpsc::Sender<Observed>,
    received: Mutex<Vec<HumanAgentMessageRequest>>,
}

fn selection(model: &str) -> SessionRuntimeSelection {
    SessionRuntimeSelection::provider(
        AgentId::parse("codex").unwrap(),
        ProviderProfileId::parse("provider_test").unwrap(),
        model,
    )
}

fn selection_state(model: &str) -> AgentSessionRuntimeSelectionState {
    AgentSessionRuntimeSelectionState {
        desired: selection(model),
        effective: selection(model),
        status: SessionRuntimeSelectionStatus::Ready,
        session_revision: 1,
        selection_revision: 1,
        current_binding_id: None,
        activation_generation: 1,
        pending_switch_id: None,
        actionable_error: None,
    }
}

macro_rules! offline_methods {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {$ (
        fn $name(&self, $($arg: $ty),*) -> BackendFuture<'_, $result> {
            Box::pin(async { Err(BackendError::offline("test_offline", "Not used by this fixture")) })
        }
    )*};
}

impl vibex_backend::AgentBackend for TeamAgent {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Err(BackendError::offline("test_offline", "No event stream"))
    }
    fn open_session(&self, id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
        let session = self.sessions.get(id.as_str()).cloned();
        Box::pin(async move {
            session
                .ok_or_else(|| BackendError::failed("session_not_found", "Missing fixture session"))
        })
    }
    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
        let _ = self
            .observed
            .send(Observed::Timeline(request.session_id.clone()));
        Box::pin(async move {
            Ok(TimelinePage {
                session_id: request.session_id,
                items: Vec::new(),
                start_sequence: None,
                end_sequence: None,
                has_older: false,
                has_newer: false,
            })
        })
    }
    fn runtime_selection(
        &self,
        _: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        Box::pin(async { Ok(selection_state("worker-model")) })
    }
    fn send_message_with_mentions(
        &self,
        request: MutationRequest<HumanAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        self.received.lock().unwrap().push(request.payload.clone());
        let _ = self.observed.send(Observed::Message(request.payload));
        Box::pin(async { Ok(Vec::new()) })
    }
    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let _ = self.observed.send(Observed::Permission(request.payload));
        Box::pin(async {
            Err(BackendError::offline(
                "test_offline",
                "Permission was captured",
            ))
        })
    }
    fn delegate_session(
        &self,
        request: MutationRequest<HumanDelegationRequest>,
    ) -> BackendFuture<'_, HumanDelegationResult> {
        let _ = self.observed.send(Observed::Delegation(request));
        Box::pin(async { Err(BackendError::offline("test_retry", "Retry this request")) })
    }
    offline_methods! {
        list_sessions(_archived: bool) -> Vec<AgentSession>;
        create_session(_request: MutationRequest<CreateAgentSessionRequest>) -> AgentSession;
        send_message(_request: MutationRequest<SendAgentMessageRequest>) -> Vec<TimelineItem>;
        continue_turn(_request: MutationRequest<ContinueAgentTurnRequest>) -> Vec<TimelineItem>;
        interrupt(_request: MutationRequest<VibexSessionId>) -> bool;
        resolve_elicitation(_request: MutationRequest<ResolveElicitationRequest>) -> TimelineItem;
        replace_user_message(_request: MutationRequest<ReplaceUserMessagePayload>) -> Vec<TimelineItem>;
        rename_session(_request: MutationRequest<RenameAgentSessionRequest>) -> AgentSession;
        archive_session(_request: MutationRequest<VibexSessionId>) -> ();
        unarchive_session(_request: MutationRequest<VibexSessionId>) -> AgentSession;
        delete_session(_request: MutationRequest<VibexSessionId>) -> ();
        list_runtime_options() -> SessionRuntimeOptionCatalog;
        probe_agent_runtime_options(_request: MutationRequest<AgentRuntimeOptionProbeRequest>) -> AgentRuntimeOptionProbeResult;
        ensure_default_agent_auth_context(_request: MutationRequest<AgentId>) -> AgentAuthContext;
        refresh_agent_auth_methods(_request: MutationRequest<AgentId>) -> AgentAuthCatalog;
        set_desired_runtime(_request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>) -> AgentSessionRuntimeSelectionState;
        cancel_runtime_switch(_request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>) -> AgentSessionRuntimeSelectionState;
    }
}

struct TeamSidebarHost {
    workbench: Entity<VibexWorkbench>,
    _subscription: Subscription,
}

impl Render for TeamSidebarHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w_80().h_full().child(
            self.workbench
                .update(cx, |workbench, cx| workbench.render_agent_sidebar(cx)),
        )
    }
}

fn session(index: usize) -> AgentSession {
    AgentSession {
        id: VibexSessionId::parse(format!("session_team_{index}")).unwrap(),
        title: format!("Session {index}"),
        project_id: ProjectId::parse("project_team").unwrap(),
        workspace_id: WorkspaceId::parse("workspace_team").unwrap(),
        workspace_root: "/team-test".into(),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        agent_id: AgentId::parse("codex").unwrap(),
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: index as i64,
        updated_at_ms: index as i64,
        last_message_at_ms: index as i64,
        archived_at_ms: None,
        deleted_at_ms: None,
    }
}

fn node(index: usize, parent: Option<usize>, children: usize) -> SessionTreeNode {
    SessionTreeNode {
        session_ref: VibexUseRef::session(&session(index).id),
        parent_session_ref: parent.map(|id| VibexUseRef::session(&session(id).id)),
        title: format!("Session {index}"),
        agent_id: Some(session(index).agent_id),
        agent_label: Some("Codex".into()),
        task_ref: None,
        task_title: None,
        task_phase: None,
        completion_policy: None,
        child_count: children,
        has_more_children: false,
        blocked_on: None,
        active_descendants: 0,
        blocked_descendants: 0,
        current_task_ref: None,
        updated_at_ms: 1,
    }
}

fn fixture(
    cx: &mut TestAppContext,
    count: usize,
) -> (
    Entity<VibexWorkbench>,
    &mut VisualTestContext,
    sync_mpsc::Receiver<Observed>,
) {
    // Production Backend calls complete on Tokio's executor.
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
        cx.set_reduce_motion(true);
    });
    let (sender, receiver) = sync_mpsc::channel();
    let agent = Arc::new(TeamAgent {
        sessions: (0..count)
            .map(|index| {
                let session = session(index);
                (session.id.to_string(), session)
            })
            .collect(),
        observed: sender,
        received: Mutex::new(Vec::new()),
    });
    let offline = Arc::new(DisconnectedBackend);
    let facade = BackendFacade::new(
        BackendCapabilitySnapshot::disconnected_v1(),
        agent,
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline,
    );
    let mut workbench = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = VibexWorkbench::with_initial_state(
                None,
                DesktopUiStateV1::default(),
                None,
                window,
                cx,
            );
            view.backend = Some(facade);
            view.sessions = vec![session(0)];
            view.sessions_loaded = true;
            view.selected_session_id = Some(session(0).id);
            view.workspaces = vec![(
                ProjectRecord {
                    id: session(0).project_id,
                    name: "Team project".into(),
                    root_path: "/team-test".into(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                WorkspaceRecord {
                    id: session(0).workspace_id,
                    project_id: session(0).project_id,
                    root_path: "/team-test".into(),
                    mode: WorkspaceMode::CurrentCheckout,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            )];
            for index in 1..count {
                view.delegated_sessions
                    .insert(session(index).id.to_string(), session(index));
            }
            view.borrow_session_view(&session(0).id);
            view.runtime_selection = Some(selection_state("root-model"));
            view
        });
        workbench = Some(view.clone());
        let host = cx.new(|cx| TeamSidebarHost {
            _subscription: cx.observe(&view, |_, _, cx| cx.notify()),
            workbench: view,
        });
        Root::new(host, window, cx)
    });
    (workbench.unwrap(), cx, receiver)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }
}

/// Puts the pointer on a row so the branch toggle it keeps hidden is painted.
///
/// A session row reveals its disclosure instead of parking it at the left edge,
/// and an unpainted element takes no hit box: the click that follows only lands
/// once the row it belongs to is hovered and the frame has been drawn again.
fn reveal_branch_toggle(cx: &mut VisualTestContext, point: gpui::Point<gpui::Pixels>) {
    cx.update(|window, cx| {
        window.simulate_mouse_move(point, cx);
        let _ = window.draw(cx);
    });
}

/// Sends the mouse-down/up pair a double click on a row produces.
///
/// The platform counts the clicks; the test states the count outright so the
/// gesture does not depend on how fast the harness delivers two clicks.
fn double_click(cx: &mut VisualTestContext, point: gpui::Point<gpui::Pixels>) {
    cx.simulate_event(gpui::MouseDownEvent {
        position: point,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: point,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
}

fn group(workbench: &mut VibexWorkbench, count: usize) {
    let members: Vec<_> = (0..count)
        .map(|index| session(index).id.to_string())
        .collect();
    let workspaces = members
        .iter()
        .map(|id| (id.clone(), "workspace_team".into()))
        .collect();
    assert!(workbench.ui_state.sidebar.organization.create_group(
        "team",
        "Team",
        "project_team",
        "workspace_team",
        &members,
        &workspaces,
        None
    ));
}

async fn attach_team_runtime(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
) -> (Arc<DesktopRuntime>, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().to_path_buf();
    let runtime = cx
        .update(|_, cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home_path))
                    .await
                    .unwrap()
            })
        })
        .await
        .unwrap();
    let conn = open_database(&runtime.config().database_path).unwrap();
    let (project, workspace) = WorkspaceRepository::ensure(
        &conn,
        home.path().join("workspace"),
        WorkspaceMode::CurrentCheckout,
    )
    .unwrap();
    workbench.update(cx, |workbench, _| {
        workbench.runtime = Some(runtime.clone());
        workbench.workspaces = vec![(project.clone(), workspace.clone())];
        for session in workbench
            .sessions
            .iter_mut()
            .chain(workbench.delegated_sessions.values_mut())
        {
            session.project_id = project.id.clone();
            session.workspace_id = workspace.id.clone();
            session.workspace_root = workspace.root_path.clone();
            SessionRepository::insert(&conn, session).unwrap();
        }
    });
    (runtime, home)
}

fn queued_worker_message() -> ComposerQueueMessage {
    ComposerQueueMessage {
        id: 1,
        session_id: session(1).id,
        desired_runtime: selection("worker-model"),
        text: "Ask @Lead".into(),
        attachments: Vec::new(),
        command_invocation: None,
        scheduled_at_ms: Some(i64::MAX),
        mentions: vec![VibexUseMention::session(&session(0).id, Some("@Lead"))],
    }
}

#[gpui::test]
fn team_root_reconciliation_retains_unloaded_members_and_queued_mentions(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            group(workbench, 3);
            workbench.delegated_sessions.clear();
            workbench.selected_session_id = Some(session(1).id);
            workbench.sidebar_state.row_order =
                vec![session(0).id.to_string(), session(1).id.to_string()];
            let queued = queued_worker_message();
            workbench.composer_queue.push(queued.clone());
            workbench.composer_queue_restore_pending = true;
            workbench
                .pending_agent_turn_session_ids
                .insert(session(1).id.to_string());
            workbench.reconcile_sidebar_state();
            workbench.adopt_restored_composer_queue_if_ready(window, cx);
            assert_eq!(
                workbench
                    .ui_state
                    .sidebar
                    .organization
                    .group("team")
                    .unwrap()
                    .member_session_ids
                    .len(),
                3
            );
            assert_eq!(workbench.composer_queue, [queued]);
            assert_eq!(workbench.selected_session_id, Some(session(1).id));
            assert_eq!(
                workbench.sidebar_state.row_order,
                [session(0).id.to_string()]
            );
            assert!(
                workbench
                    .pending_agent_turn_session_ids
                    .contains(session(1).id.as_str())
            );
            assert!(
                !workbench
                    .ui_state
                    .sidebar
                    .organization
                    .placements
                    .iter()
                    .any(|placement| {
                        placement.item
                            == SidebarOrganizationItem::Session(session(1).id.to_string())
                    })
            );
            workbench.optimistically_remove_sessions(&BTreeSet::from([session(1).id.to_string()]));
            assert!(workbench.composer_queue.is_empty());
            assert_eq!(
                workbench
                    .ui_state
                    .sidebar
                    .organization
                    .group("team")
                    .unwrap()
                    .member_session_ids
                    .len(),
                2
            );
        })
    });
}

#[gpui::test]
fn team_worker_revision_rearms_its_queue_and_keeps_its_lifecycle_state(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    workbench.update(cx, |workbench, cx| {
        workbench.composer_queue.push(queued_worker_message());
        workbench.auto_continue_turn_statuses.insert(
            session(1).id.to_string(),
            AutoContinueTurnStatus {
                session_updated_at_ms: 1,
                ended_normally: Some(true),
            },
        );
        let mut worker = session(1);
        worker.updated_at_ms = 10;
        worker.state = AgentSessionState::Running;
        workbench.upsert_session_snapshot(worker.clone());
        assert!(workbench.agent_session_is_active(&worker.id));
        assert!(
            workbench
                .composer_queue_ready_after_continuation_session_ids
                .contains(worker.id.as_str())
        );
        assert!(
            !workbench
                .auto_continue_turn_statuses
                .contains_key(worker.id.as_str())
        );
        assert!(!workbench.sessions.iter().any(|root| root.id == worker.id));
        worker.state = AgentSessionState::Idle;
        worker.updated_at_ms = 11;
        workbench.upsert_session_snapshot(worker.clone());
        assert!(workbench.cache_auto_continue_turn_status(&worker.id, 11, Some(true), cx));
        assert_eq!(
            workbench
                .cached_auto_continue_turn_status(&worker.id, 11)
                .unwrap()
                .ended_normally,
            Some(true)
        );
        assert!(!workbench.agent_session_is_active(&worker.id));
    });
}

#[gpui::test]
async fn team_queued_session_recovery_uses_metadata_and_drops_only_confirmed_deletions(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.delegated_sessions.clear();
            workbench.composer_queue.push(queued_worker_message());
            let mut deleted = queued_worker_message();
            deleted.id = 2;
            deleted.session_id = session(2).id;
            workbench.composer_queue.push(deleted);
            for id in [1, 2] {
                workbench.load_queued_session(
                    session(id).id,
                    ComposerQueueDispatchBehavior::Automatic,
                    window,
                    cx,
                );
            }
        })
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.composer_queue_session_loads.is_empty()
    })
    .await;
    workbench.read_with(cx, |workbench, _| {
        assert!(
            workbench
                .registered_session(session(1).id.as_str())
                .is_some()
        );
        assert_eq!(workbench.composer_queue, [queued_worker_message()]);
        assert_eq!(workbench.sessions.len(), 1);
    });
    assert!(
        receiver.try_recv().is_err(),
        "recovering queued session identities does not open their timelines"
    );
}

#[gpui::test]
fn team_page_refresh_evicts_removed_descendant_branches(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, cx| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3]), (3, Vec::new())] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|id| session(id).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
        workbench.apply_delegated_page(
            Some(session(0).id.to_string()),
            false,
            SessionTreePage {
                nodes: vec![node(2, Some(0), 0)],
                ancestors: vec![node(0, None, 1)],
                registry: vec![session(0), session(2)],
                ..Default::default()
            },
            cx,
        );
        for id in [1, 3] {
            assert!(
                !workbench
                    .delegated_tree
                    .contains_key(session(id).id.as_str())
            );
            assert!(
                !workbench
                    .delegated_sessions
                    .contains_key(session(id).id.as_str())
            );
            assert!(
                !workbench
                    .delegated_pages
                    .contains_key(&Some(session(id).id.to_string()))
            );
        }
        assert_eq!(
            workbench.delegated_children(session(0).id.as_str()).len(),
            1
        );
    });
}

#[gpui::test]
fn team_tree_collapse_and_keyboard_order_follow_grouped_and_compact_rows(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3])] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|index| session(index).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
    });
    draw(cx);
    let expected: Vec<_> = [0, 1, 3, 2]
        .into_iter()
        .map(|index| session(index).id.to_string())
        .collect();
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected
    );
    // The toggle is painted only while its row is hovered, so the pointer has to
    // arrive before the button is there to click.
    let root_row = cx
        .debug_bounds("sidebar-session-row-session_team_0")
        .expect("the root session row is rendered");
    assert!(
        cx.debug_bounds("sidebar-delegated-toggle-session_team_0")
            .is_none(),
        "the branch toggle stays out of the row until the pointer arrives"
    );
    reveal_branch_toggle(cx, root_row.center());
    let disclosure = cx
        .debug_bounds("sidebar-delegated-toggle-session_team_0")
        .expect("hovering the row reveals its branch toggle");
    cx.simulate_click(disclosure.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [session(0).id.to_string()]
    );
    cx.simulate_click(disclosure.center(), Modifiers::none());
    draw(cx);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            window.focus(&workbench.delegated_row_focus[session(0).id.as_str()], cx);
        });
    });
    cx.simulate_keystrokes("down");
    cx.update(|window, cx| {
        assert!(workbench.read(cx).delegated_row_focus[session(1).id.as_str()].is_focused(window))
    });
    for hierarchy in [
        SidebarHierarchyMode::Compact,
        SidebarHierarchyMode::Detailed,
    ] {
        workbench.update(cx, |workbench, cx| {
            if workbench
                .ui_state
                .sidebar
                .organization
                .group("team")
                .is_none()
            {
                group(workbench, 4);
            }
            workbench.ui_state.sidebar.hierarchy_mode = hierarchy;
            workbench.invalidate_sidebar_projection_cache();
            cx.notify();
        });
        draw(cx);
        assert_eq!(
            workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
            expected
        );
    }
}

/// The branch a session row owns answers to a double click on the row itself.
///
/// The pair is a toggle rather than a fold-only verb: the same gesture opens the
/// branch again, and the single click inside the pair still selects the row, so
/// the gesture never costs the navigation the first click performs.
#[gpui::test]
fn double_clicking_a_session_row_toggles_its_branch(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3])] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|index| session(index).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
    });
    draw(cx);
    let expected: Vec<_> = [0, 1, 3, 2]
        .into_iter()
        .map(|index| session(index).id.to_string())
        .collect();
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected
    );
    let root_row = cx
        .debug_bounds("sidebar-session-row-session_team_0")
        .expect("the root session row is rendered");
    double_click(cx, root_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [session(0).id.to_string()],
        "a double click folds the branch under the row"
    );
    double_click(cx, root_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected,
        "the same gesture opens the branch again"
    );
    // A child row owns a branch of its own, so the gesture reads the same there.
    let child_row = cx
        .debug_bounds("sidebar-delegated-row-session_team_1")
        .expect("the delegated row is rendered");
    double_click(cx, child_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [0, 1, 2].map(|index| session(index).id.to_string()),
        "a double click on a child row folds its own children"
    );
}

/// A delegated row is the session row's own shape one level down.
///
/// The branch's trunk leaves the parent row's Agent logo, its elbow lands on the
/// child's logo, and the child's status column ends on the parent's right edge —
/// the three things that make a delegated session read as a session parked under
/// its parent instead of a different kind of row.
#[gpui::test]
fn delegated_rows_branch_from_the_parent_logo_and_share_its_columns(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, _| {
        for node in [node(0, None, 2), node(1, Some(0), 0), node(2, Some(0), 0)] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        workbench.delegated_pages.insert(
            Some(session(0).id.to_string()),
            DelegatedBranchPage {
                session_ids: vec![session(1).id.to_string(), session(2).id.to_string()],
                ..Default::default()
            },
        );
    });
    draw(cx);
    let parent_logo = cx
        .debug_bounds("sidebar-session-logo-session_team_0")
        .expect("the root row draws its Agent logo");
    let parent_status = cx
        .debug_bounds("sidebar-session-status-session_team_0")
        .expect("the root row keeps its status column");
    let child_logo = cx
        .debug_bounds("delegated-logo-session_team_1")
        .expect("the delegated row draws its Agent logo");
    let child_status = cx
        .debug_bounds("delegated-status-session_team_1")
        .expect("the delegated row keeps a status column");
    let trunk = cx
        .debug_bounds("delegated-trunk-session_team_1-0")
        .expect("a child row draws the trunk of its own level");
    let elbow = cx
        .debug_bounds("delegated-elbow-session_team_1-0")
        .expect("a child row draws the elbow into its logo");

    assert!(
        (trunk.left() - parent_logo.center().x).abs() <= px(0.5),
        "the trunk leaves the center of the parent row's Agent logo: {trunk:?} vs {parent_logo:?}"
    );
    assert!(
        (elbow.right() - child_logo.left()).abs() <= px(0.5),
        "the elbow stops where the child's Agent logo begins: {elbow:?} vs {child_logo:?}"
    );
    assert!(
        (elbow.center().y - child_logo.center().y).abs() <= px(1.0),
        "the elbow meets the child's logo on its own line"
    );
    assert!(
        (child_status.right() - parent_status.right()).abs() <= px(0.5),
        "both rows end their status on one vertical line: {child_status:?} vs {parent_status:?}"
    );
    assert_eq!(
        child_status.size.width, parent_status.size.width,
        "a delegated row reserves the session row's status column"
    );

    // A branch's trunk carries on to the next sibling; the last child's stops at
    // its own elbow instead of running past the branch.
    let next_trunk = cx
        .debug_bounds("delegated-trunk-session_team_2-0")
        .expect("the last child still draws its elbow's trunk");
    let next_elbow = cx
        .debug_bounds("delegated-elbow-session_team_2-0")
        .expect("the last child draws its elbow");
    assert!(
        trunk.size.height > next_trunk.size.height,
        "the last child stops its trunk at the elbow"
    );
    assert!(
        next_trunk.bottom() <= next_elbow.center().y + px(1.0),
        "the last trunk never runs past the branch it closes"
    );
}

#[gpui::test]
fn team_presentation_recovers_missing_groups_preserves_manual_layout_and_respects_authority(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, cx| {
        let mut command = GroupPresentationCommand {
            group_id: SessionGroupId::parse("group_team").unwrap(),
            operation_id: None,
            name: "Team".into(),
            workspace_ref: VibexUseRef::new(VibexUseResourceKind::Workspace, "workspace_team"),
            member_session_refs: vec![
                VibexUseRef::session(&session(0).id),
                VibexUseRef::session(&session(1).id),
            ],
            layout: SessionGroupLayoutIntent::default(),
            expected_revision: None,
            activation_policy: PresentationActivationPolicy::IfCurrentTeam,
            focus_session_ref: Some(VibexUseRef::session(&session(1).id)),
            present: true,
            presentation_only: true,
        };
        workbench.selected_session_id = Some(session(2).id);
        let result = workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(result.state, PresentationState::Deferred);
        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("group_team")
                .is_some()
        );
        assert_eq!(workbench.selected_session_id, Some(session(2).id));
        workbench.selected_session_id = Some(session(0).id);
        workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(workbench.selected_session_id, Some(session(1).id));
        workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("group_team")
            .unwrap()
            .merge_panes();
        workbench.sync_user_session_group("group_team");
        let layout = workbench
            .ui_state
            .sidebar
            .organization
            .group("group_team")
            .unwrap()
            .layout
            .clone();
        command.layout.preset = SessionGroupLayoutPreset::Grid;
        command.focus_session_ref = None;
        workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("group_team")
                .unwrap()
                .layout,
            layout
        );
        workbench.ui_state.sidebar.switch_authority("server:other");
        assert_eq!(
            workbench
                .apply_group_presentation(&command, cx)
                .unwrap()
                .reason,
            Some(VibexUseUnavailableReason::ForeignAuthority)
        );
        assert!(workbench.ui_state.sidebar.organization.groups.is_empty());
    });
}

#[gpui::test]
async fn team_batch_group_moves_and_removals_update_every_durable_definition(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 6);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let conn = open_database(&runtime.config().database_path).unwrap();
    let members = |indexes: &[usize]| {
        indexes
            .iter()
            .map(|index| session(*index).id)
            .collect::<Vec<_>>()
    };
    let session_ids = |indexes: &[usize]| {
        members(indexes)
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
    };
    workbench.update(cx, |workbench, _| {
        let (project, workspace) = &workbench.workspaces[0];
        let session_workspaces = workbench.sidebar_session_workspaces();
        for (group_id, indexes) in [
            ("group_destination", vec![0]),
            ("group_source", vec![1, 2]),
            ("group_emptied", vec![3]),
            ("group_untouched", vec![4, 5]),
        ] {
            GroupPresentationRepository::reserve_or_get(
                &conn,
                None,
                "session:team",
                group_id,
                &SessionGroupScope::Workspace {
                    workspace_ref: VibexUseRef::new(
                        VibexUseResourceKind::Workspace,
                        workspace.id.as_str(),
                    ),
                },
                group_id,
                &members(&indexes),
                &SessionGroupLayoutIntent::default(),
                group_id,
            )
            .unwrap();
            assert!(workbench.ui_state.sidebar.organization.create_group(
                group_id,
                group_id,
                project.id.as_str(),
                workspace.id.as_str(),
                &session_ids(&indexes),
                &session_workspaces,
                None,
            ));
            workbench
                .ui_state
                .sidebar
                .organization
                .group_mut(group_id)
                .unwrap()
                .set_automatic_layout(SessionGroupLayoutIntent::default(), 80.0);
        }
    });
    let revision = workbench.update(cx, |workbench, cx| {
        workbench.add_sessions_to_group_from_menu("group_destination", &session_ids(&[1, 3]), cx);
        assert!(workbench.persistence_note.is_none());
        workbench.sidebar_organization_view().revision
    });
    for (group_id, expected) in [
        ("group_destination", vec![0, 1, 3]),
        ("group_source", vec![2]),
    ] {
        let saved = GroupPresentationRepository::get(&conn, group_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.member_session_ids, members(&expected));
        assert!(!saved.created_by_caller);
        assert_eq!(saved.client_revision, Some(revision));
    }
    assert!(
        GroupPresentationRepository::get(&conn, "group_emptied")
            .unwrap()
            .is_none()
    );
    let untouched = GroupPresentationRepository::get(&conn, "group_untouched")
        .unwrap()
        .unwrap();
    assert_eq!(untouched.member_session_ids, members(&[4, 5]));
    assert!(untouched.created_by_caller);
    assert_eq!(untouched.client_revision, None);

    let revision = workbench.update(cx, |workbench, cx| {
        workbench.remove_sessions_from_groups_from_menu(&session_ids(&[0, 1, 2, 4]), cx);
        assert!(workbench.persistence_note.is_none());
        workbench.sidebar_organization_view().revision
    });
    for (group_id, expected) in [("group_destination", vec![3]), ("group_untouched", vec![5])] {
        let saved = GroupPresentationRepository::get(&conn, group_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.member_session_ids, members(&expected));
        assert!(!saved.created_by_caller);
        assert_eq!(saved.client_revision, Some(revision));
    }
    assert!(
        GroupPresentationRepository::get(&conn, "group_source")
            .unwrap()
            .is_none()
    );

    // Matching ids on another authority cannot remove the local definitions.
    workbench.update(cx, |workbench, _| {
        workbench.ui_state.sidebar.switch_authority("server:other");
        workbench.sync_user_session_groups(&BTreeSet::from([
            "group_destination".to_string(),
            "group_untouched".to_string(),
        ]));
    });
    assert!(
        GroupPresentationRepository::get(&conn, "group_destination")
            .unwrap()
            .is_some()
    );
    assert!(
        GroupPresentationRepository::get(&conn, "group_untouched")
            .unwrap()
            .is_some()
    );
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_capacity_rejected_transfers_keep_source_group_ownership(cx: &mut TestAppContext) {
    let limit = vibex_desktop_model::SESSION_GROUP_MEMBER_LIMIT;
    let (workbench, cx, _) = fixture(cx, limit + 2);
    workbench.update(cx, |workbench, cx| {
        group(workbench, limit);
        let session_workspaces = workbench.sidebar_session_workspaces();
        for (group_id, index) in [("accepted", limit), ("rejected", limit + 1)] {
            assert!(workbench.ui_state.sidebar.organization.create_group(
                group_id,
                group_id,
                session(0).project_id.as_str(),
                session(0).workspace_id.as_str(),
                &[session(index).id.to_string()],
                &session_workspaces,
                None,
            ));
            workbench
                .ui_state
                .sidebar
                .organization
                .group_mut(group_id)
                .unwrap()
                .set_automatic_layout(SessionGroupLayoutIntent::default(), 80.0);
        }
        let source_groups = workbench.ui_state.sidebar.organization.clone();
        let moving = [
            session(limit).id.to_string(),
            session(limit + 1).id.to_string(),
        ];
        workbench.add_sessions_to_group_from_menu("team", &moving, cx);
        assert_eq!(workbench.ui_state.sidebar.organization, source_groups);

        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .remove_sessions_from_group("team", &[session(limit - 1).id.to_string()])
        );
        workbench.add_sessions_to_group_from_menu("team", &moving, cx);
        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("accepted")
                .is_none()
        );
        assert_eq!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group_of_session(&moving[0]),
            Some("team"),
        );
        assert_eq!(
            workbench.ui_state.sidebar.organization.group("rejected"),
            source_groups.group("rejected"),
            "a capacity rejection must not transfer ownership of the source group",
        );
    });
}

/// The Archived page has to answer the question an empty list raises: is the
/// inactivity window running at all, and how far is the oldest session from
/// it? The pass is silent by design, so the page reads the window's own
/// report — including the batch a narrowed window retires in one go.
#[gpui::test]
fn auto_archive_status_explains_the_window_and_the_last_check(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, cx| {
        let now_ms = 40 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        let mut oldest = session(0);
        oldest.created_at_ms = 0;
        oldest.last_message_at_ms = 0;
        let mut recent = session(1);
        recent.created_at_ms = 38 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        recent.last_message_at_ms = recent.created_at_ms;
        let mut pinned = session(2);
        pinned.last_message_at_ms = 0;
        let mut sessions = vec![oldest, recent, pinned.clone()];
        for index in 3..15 {
            let mut aged = session(index);
            aged.created_at_ms = 0;
            aged.last_message_at_ms = 0;
            sessions.push(aged);
        }
        workbench.sessions = sessions;
        workbench.sidebar_state.pinned_ids = BTreeSet::from([pinned.id.to_string()]);

        // Nothing to report while the window is off.
        assert_eq!(workbench.auto_archive_status(now_ms), None);

        // A pinned session is a deliberate keep, so it is not what the window
        // reports as waiting: the line names the oldest session the window can
        // still act on, which is what tells the reader the window has simply
        // not been reached yet.
        workbench.ui_state.session.auto_archive_after_days = Some(30);
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(
            status,
            AutoArchiveStatus {
                window_days: 30,
                eligible_sessions: 14,
                oldest_idle_days: Some(40),
                last_pass: None,
            }
        );
        let label = auto_archive_status_label(&status);
        assert!(label.contains("40") && label.contains("30"), "{label}");

        // Narrowing the window retires the backlog in one pass, and the page
        // names that batch instead of leaving the reader to notice the rows.
        workbench.ui_state.session.auto_archive_after_days = Some(7);
        assert!(workbench.apply_auto_archive_sessions(now_ms, cx));
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(
            status
                .last_pass
                .map(|pass| (pass.window_days, pass.archived)),
            Some((7, 13))
        );
        let label = auto_archive_status_label(&status);
        assert!(label.contains("13") && label.contains('7'), "{label}");

        // The batch belongs to the window that produced it: a reader who
        // widens the window again is back to "nothing archived yet" rather
        // than reading a count that no longer describes this policy.
        workbench.ui_state.session.auto_archive_after_days = Some(14);
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(status.last_pass, None);
    });
}

#[gpui::test]
async fn team_root_listing_and_worker_updates_keep_automatic_archive_off_unloaded_workers(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 2);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let conn = open_database(&runtime.config().database_path).unwrap();
    SessionOwnershipRepository::upsert(&conn, &session(1).id, &session(0).id, None).unwrap();
    let mut worker = SessionRepository::get(&conn, &session(1).id)
        .unwrap()
        .unwrap();
    let backend = Arc::new(NativeBackend::new(runtime.clone())).facade();
    let roots = cx
        .update(|_, cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                backend.agent().list_sessions(false).await.unwrap()
            })
        })
        .await
        .unwrap();
    assert_eq!(
        roots.iter().map(|session| &session.id).collect::<Vec<_>>(),
        [&session(0).id]
    );
    workbench.update(cx, |workbench, cx| {
        workbench.sessions = roots;
        workbench.delegated_sessions.clear();
        assert!(workbench.delegated_tree.is_empty());
        assert!(workbench.delegated_pages.is_empty());
        let now_ms = 40 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        assert!(workbench.auto_archive_candidates(now_ms).is_empty());
        workbench.ui_state.session.auto_archive_after_days = Some(30);
        assert_eq!(workbench.auto_archive_candidates(now_ms), [session(0).id]);

        // The first broadcast arrives before any ownership page. Later
        // broadcasts update that registry entry without promoting it to a root.
        for revision in [2, 3] {
            worker.updated_at_ms = revision;
            workbench.apply_desktop_event(DesktopEvent::SessionUpdated(worker.clone()), cx);
            assert_eq!(workbench.sessions.len(), 1);
            assert_eq!(
                workbench.registered_session(worker.id.as_str()),
                Some(&worker)
            );
            assert_eq!(workbench.auto_archive_candidates(now_ms), [session(0).id]);
            assert!(workbench.delegated_tree.is_empty());
        }
        assert!(workbench.delegated_roots_dirty);
    });
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_background_page_refresh_never_borrows_the_loading_row(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            // Ownership pages only load on an authority that can read the tree.
            workbench
                .backend
                .as_ref()
                .expect("fixture backend")
                .replace_capabilities(BackendCapabilitySnapshot::desktop_native_v1());
            workbench.delegated_pages.insert(
                None,
                DelegatedBranchPage {
                    session_ids: vec![session(1).id.to_string()],
                    next_cursor: Some("cursor_1".into()),
                    ..Default::default()
                },
            );

            // The periodic pass fetches silently: the sidebar's "Loading
            // sessions" row belongs to a reader who asked for the page, not to
            // a poll that runs every few seconds.
            workbench.load_delegated_page(None, false, DelegatedPageLoad::Background, cx);
            let page = &workbench.delegated_pages[&None];
            assert!(!page.loading, "a background pass must not claim the row");
            assert!(page.in_flight, "the pass still dedupes against itself");

            // An explicit load keeps the row it always had.
            workbench.delegated_pages.get_mut(&None).unwrap().in_flight = false;
            workbench.load_delegated_page(None, false, DelegatedPageLoad::Interactive, cx);
            assert!(workbench.delegated_pages[&None].loading);

            // A failed background pass over rows that are already on screen
            // stays quiet: alternating retry and data every few seconds is the
            // flicker this split exists to remove. An empty page and an
            // explicit load may still claim the retry row.
            workbench.delegated_pages.get_mut(&None).unwrap().in_flight = false;
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Background,
                "offline".into(),
            );
            assert!(workbench.delegated_pages[&None].error.is_none());
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Interactive,
                "offline".into(),
            );
            assert_eq!(
                workbench.delegated_pages[&None].error.as_deref(),
                Some("offline")
            );
            let empty = workbench.delegated_pages.get_mut(&None).unwrap();
            empty.error = None;
            empty.session_ids.clear();
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Background,
                "offline".into(),
            );
            assert_eq!(
                workbench.delegated_pages[&None].error.as_deref(),
                Some("offline"),
                "a branch with nothing to show still reports its failure"
            );
        });
    });
}

/// The pager is not a sidebar footer.
///
/// The root page pages ownership metadata for sessions the sidebar already
/// lists, so it draws no row of its own: a full-width "Load more sessions"
/// button under the project list read as a paged session list and did nothing
/// visible when clicked. A branch still pages under the row that owns it, and
/// a branch the branch limit refused is answered in the same place.
#[gpui::test]
fn team_delegated_pager_stays_under_its_branch(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        workbench
            .delegated_tree
            .insert(session(1).id.to_string(), node(1, Some(0), 0));
        workbench.delegated_pages.insert(
            Some(session(0).id.to_string()),
            DelegatedBranchPage {
                session_ids: vec![session(1).id.to_string()],
                next_cursor: Some("cursor_branch".into()),
                ..Default::default()
            },
        );
    });
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-session_team_0").is_some(),
        "a branch still pages under its own row"
    );

    // The branch limit refuses a branch by name, and the reader who asked for
    // it is told under that row instead of in a sidebar footer.
    workbench.update(cx, |workbench, cx| {
        workbench
            .delegated_pages
            .remove(&Some(session(0).id.to_string()));
        workbench.delegated_deferred_parent = Some(session(0).id.to_string());
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-session_team_0").is_some(),
        "the refusal belongs under the row that asked for the branch"
    );
}

/// One root page per background pass, with no button to press.
///
/// The pass used to re-read page one forever, so a tree with more roots than a
/// page kept the cursor set and the sidebar kept offering to load rows that
/// were already on screen. Following the cursor is what retires that row and
/// what puts the ownership metadata behind the later roots in place.
#[gpui::test]
async fn team_root_page_advances_in_the_background_without_a_footer_pager(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, DELEGATED_TREE_PAGE_SIZE + 1);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let backend = Arc::new(NativeBackend::new(runtime.clone())).facade();
    workbench.update(cx, |workbench, cx| {
        workbench.backend = Some(backend);
        workbench.refresh_delegated_sessions(cx);
    });
    let root_page_settled = |workbench: &VibexWorkbench| {
        workbench
            .delegated_pages
            .get(&None)
            .is_some_and(|page| !page.in_flight && !page.session_ids.is_empty())
    };
    cx.condition(&workbench, |workbench, _| root_page_settled(workbench))
        .await;
    workbench.read_with(cx, |workbench, _| {
        let page = &workbench.delegated_pages[&None];
        assert_eq!(page.session_ids.len(), DELEGATED_TREE_PAGE_SIZE);
        assert!(page.next_cursor.is_some(), "one root is still unread");
    });
    // The state the footer used to speak from: one page in hand, one to go.
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-roots").is_none(),
        "an unspent root cursor draws no sidebar footer"
    );
    workbench.update(cx, |workbench, cx| workbench.refresh_delegated_sessions(cx));
    cx.condition(&workbench, |workbench, _| {
        root_page_settled(workbench) && workbench.delegated_pages[&None].next_cursor.is_none()
    })
    .await;
    workbench.read_with(cx, |workbench, _| {
        assert_eq!(
            workbench.delegated_pages[&None].session_ids.len(),
            DELEGATED_TREE_PAGE_SIZE + 1,
            "the next pass reads the page the cursor points at"
        );
    });
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_hidden_tabs_materialize_only_the_four_live_conversations(cx: &mut TestAppContext) {
    let (workbench, cx, receiver) = fixture(cx, 64);
    workbench.update(cx, |workbench, cx| {
        group(workbench, 64);
        workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("team")
            .unwrap()
            .apply_layout_preset(SessionGroupLayoutPreset::Columns, None, Some(4));
        workbench.ensure_session_group_views("team", cx);
        workbench.ensure_session_group_views("team", cx);
    });
    let mut loaded = BTreeSet::new();
    for _ in 0..4 {
        let Observed::Timeline(id) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("expected a timeline fetch");
        };
        loaded.insert(id);
    }
    assert_eq!(loaded, (0..4).map(|index| session(index).id).collect());
    assert!(receiver.try_recv().is_err());
    cx.run_until_parked();
    workbench.update(cx, |workbench, cx| {
        let group = workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("team")
            .unwrap();
        let pane = group
            .layout
            .pane_containing_session(session(63).id.as_str())
            .unwrap();
        group.layout.focus_session(&pane, session(63).id.as_str());
        group.focus_pane(&pane);
        workbench.record_composer_geometry(Some(session(3).id.as_str()), |geometry| {
            geometry.input_bounds = Some(Bounds::new(
                point(px(10.0), px(20.0)),
                size(px(30.0), px(40.0)),
            ));
        });
        workbench.selected_session_id = Some(session(63).id);
        workbench.borrow_session_view(&session(63).id);
        workbench.ensure_session_group_views("team", cx);
        let mut resident: BTreeSet<_> = workbench.session_views.keys().cloned().collect();
        resident.extend(workbench.view_session_id.as_ref().map(ToString::to_string));
        assert_eq!(resident.len(), 4);
        assert!(resident.contains(session(63).id.as_str()));
        assert!(!resident.contains(session(3).id.as_str()));
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(3).id.as_str())
        );
    });
    let Observed::Timeline(id) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected lazy tab fetch");
    };
    assert_eq!(id, session(63).id);
}

#[gpui::test]
fn team_view_removal_and_eviction_discard_only_their_composer_measurements(
    cx: &mut TestAppContext,
) {
    let count = AGENT_SESSION_VIEW_CACHE_LIMIT + 2;
    let (workbench, cx, _) = fixture(cx, count);
    workbench.update(cx, |workbench, _| {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(30.0), px(40.0)));
        for index in 0..3 {
            workbench.ensure_session_view(&session(index).id);
            workbench.record_composer_geometry(Some(session(index).id.as_str()), |geometry| {
                geometry.input_bounds = Some(bounds);
            });
        }
        workbench.optimistically_remove_sessions(&BTreeSet::from([session(2).id.to_string()]));
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(2).id.as_str())
        );
        assert!(
            workbench
                .pending_composer_geometries
                .contains_key(session(1).id.as_str())
        );
        for index in 2..count {
            workbench.ensure_session_view(&session(index).id);
        }
        assert!(!workbench.session_views.contains_key(session(1).id.as_str()));
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(1).id.as_str())
        );
        assert!(
            workbench
                .pending_composer_geometries
                .contains_key(session(0).id.as_str())
        );
        workbench.adopt_composer_geometry();
        assert_eq!(workbench.composer_geometry.input_bounds, Some(bounds));
    });
}

#[gpui::test]
fn team_composer_and_permissions_keep_the_borrowed_panes_session_and_runtime(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(2).id, Some("@Reviewer"));
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            assert!(
                workbench
                    .registered_session(session(2).id.as_str())
                    .is_none()
            );
            workbench.borrow_session_view(&session(1).id);
            workbench.runtime_selection = Some(selection_state("worker-model"));
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value(
                    composer_content_with_mentions("Ask @Reviewer", std::slice::from_ref(&mention)),
                    window,
                    cx,
                )
            });
            let message = workbench.take_composer_message_remote(window, cx).unwrap();
            assert_eq!(message.session_id, session(1).id);
            assert_eq!(message.desired_runtime.model_id(), Some("worker-model"));
            workbench.dispatch_composer_message_remote(
                message,
                workbench.backend.clone().unwrap(),
                UserMessageDelivery::Prompt,
                window,
                cx,
            );
            workbench.resolve_permission(
                session(1).id,
                RequestId::new().to_string(),
                PermissionResponseKind::Approve,
                None,
                cx,
            );
        })
    });
    let mut sent = None;
    let mut permission = None;
    while sent.is_none() || permission.is_none() {
        match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            Observed::Message(message) => sent = Some(message),
            Observed::Permission(request) => permission = Some(request),
            Observed::Timeline(_) => {}
            Observed::Delegation(_) => panic!("unexpected delegation"),
        }
    }
    assert_eq!(sent.unwrap().mentions, [mention]);
    assert_eq!(permission.unwrap().session_id, session(1).id);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.selected_session_id.clone()),
        Some(session(0).id)
    );
}

#[gpui::test]
fn team_message_copy_paste_and_inline_edit_preserve_only_selected_references(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(1).id, Some("@Reviewer"));
    let text = "Ask @Reviewer, then @PlainName";
    let clipboard =
        user_message_clipboard_item_with_mentions(text, &[], std::slice::from_ref(&mention));
    let ClipboardEntry::String(value) = &clipboard.entries()[0] else {
        panic!("text clipboard");
    };
    let metadata = value.metadata_json::<VibexMessageClipboard>().unwrap();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            assert!(workbench.add_vibex_message_clipboard_to(metadata, false, window, cx));
            let input = workbench.ensure_composer_input(window, cx);
            assert_eq!(
                composer_mentions(input.read(cx).tokens()),
                std::slice::from_ref(&mention)
            );
            workbench.begin_inline_user_message_edit(
                "turn".into(),
                "row".into(),
                session(0).id,
                1,
                1,
                text.into(),
                Vec::new(),
                vec![mention.clone()],
                window,
                cx,
            );
            assert_eq!(
                composer_mentions(workbench.user_message_edit_input.read(cx).tokens()),
                std::slice::from_ref(&mention)
            );
            let range = workbench.user_message_edit_input.read(cx).tokens()[0].range();
            workbench.user_message_edit_input.update(cx, |input, cx| {
                input.set_selected_range(range, cx);
                input.replace("@PlainName", window, cx);
            });
            assert!(
                composer_mentions(workbench.user_message_edit_input.read(cx).tokens()).is_empty()
            );
        })
    });
}

#[gpui::test]
async fn team_immediate_delegation_keeps_context_and_retries_the_same_gesture(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(1).id, Some("@Reviewer"));
    let option =
        VibexUseRef::new(VibexUseResourceKind::RuntimeOption, "runtime-option-test").as_uri();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value(
                    composer_content_with_mentions(
                        "Review @Reviewer",
                        std::slice::from_ref(&mention),
                    ),
                    window,
                    cx,
                )
            });
            workbench.add_composer_path_to(
                std::path::Path::new("/team-test/context.md"),
                None,
                false,
                window,
                cx,
            );
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
        })
    });
    let Observed::Delegation(first) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected delegation");
    };
    assert_eq!(first.payload.parent_session_id, session(0).id);
    assert_eq!(first.payload.mentions, [mention]);
    assert_eq!(first.payload.attachments.len(), 1);
    assert_eq!(first.payload.context_refs[0].session_id, session(1).id);
    assert!(
        receiver.try_recv().is_err(),
        "a pending gesture cannot be submitted twice"
    );
    cx.condition(&workbench, |workbench, _| {
        !workbench.composer_delegations[session(0).id.as_str()].pending
    })
    .await;
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
        })
    });
    let Observed::Delegation(retry) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected retry");
    };
    assert_eq!(first.idempotency_key, retry.idempotency_key);
    cx.condition(&workbench, |workbench, _| {
        !workbench.composer_delegations[session(0).id.as_str()].pending
    })
    .await;
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value("A different task", window, cx)
            });
            workbench.start_immediate_delegation("codex".into(), option, window, cx);
        })
    });
    let Observed::Delegation(changed) = receiver.recv_timeout(Duration::from_secs(5)).unwrap()
    else {
        panic!("expected changed task");
    };
    assert_ne!(first.idempotency_key, changed.idempotency_key);
}
