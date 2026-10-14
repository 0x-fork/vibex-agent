//! The elicitation form: the Agent's question, and the answers that go back.
//!
//! The desktop draws this card inline in the timeline and the terminal draws
//! the same card as a modal, so the two clients answer a question the same way:
//! walk the fields, choose or type, submit. These tests pin what the reader can
//! actually *do* with it — open it, walk it, type into it, choose from it,
//! submit it, refuse it — because "the card exists" is not the same as "the
//! card works", and the difference is what a blocked Agent waits on.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_core::{
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationOption,
    ElicitationRequest, ElicitationRequestStatus, ElicitationResolutionAction, RequestId,
    TimelineItemKind, TimelinePayload, VibexSessionId,
};
use vibex_tui::action::Intent;
use vibex_tui::app::{App, AppOptions, Effect, Overlay, Page};
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::{Locale, SeatKind};

fn app(columns: u16, rows: u16) -> App {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Remote,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
            theme_id: Some("vibex-dark".to_string()),
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.terminal_images = vibex_tui::terminal_image::ImageSupport::forced(
        vibex_tui::terminal_image::ImageProtocol::None,
    );
    app.resize(columns, rows);
    app
}

fn text_field(id: &str, title: &str, required: bool) -> ElicitationField {
    ElicitationField {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        required,
        kind: ElicitationFieldKind::Text {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            default: None,
            options: Vec::new(),
        },
    }
}

fn choice_field(id: &str, title: &str, options: &[(&str, &str)]) -> ElicitationField {
    ElicitationField {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        required: true,
        kind: ElicitationFieldKind::Text {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            default: None,
            options: options
                .iter()
                .map(|(value, title)| ElicitationOption {
                    value: value.to_string(),
                    title: title.to_string(),
                    description: None,
                })
                .collect(),
        },
    }
}

fn multi_field(id: &str, title: &str, options: &[&str]) -> ElicitationField {
    ElicitationField {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        required: true,
        kind: ElicitationFieldKind::MultiSelect {
            options: options
                .iter()
                .map(|value| ElicitationOption {
                    value: value.to_string(),
                    title: value.to_string(),
                    description: None,
                })
                .collect(),
            min_items: Some(1),
            max_items: None,
            default: Vec::new(),
        },
    }
}

fn boolean_field(id: &str, title: &str) -> ElicitationField {
    ElicitationField {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        required: true,
        kind: ElicitationFieldKind::Boolean { default: None },
    }
}

/// A session with one pending question on it, which is what the Agent raises
/// when it needs an answer before it can carry on.
fn app_with_question(fields: Vec<ElicitationField>, columns: u16, rows: u16) -> App {
    let session_id = VibexSessionId::new();
    let request = ElicitationRequest {
        id: RequestId::new(),
        session_id: session_id.clone(),
        provider_request_id: None,
        tool_call_id: None,
        message: "The agent needs your input.".to_string(),
        title: Some("Grok needs an answer".to_string()),
        description: None,
        fields,
        status: ElicitationRequestStatus::Pending,
        requested_at_ms: 1_000,
    };
    app_with_items(
        session_id,
        vec![(
            TimelineItemKind::ElicitationRequest,
            TimelinePayload::ElicitationRequest(request),
        )],
        columns,
        rows,
    )
}

/// An app on the Agent page whose session already holds these timeline items.
fn app_with_items(
    session_id: VibexSessionId,
    items: Vec<(TimelineItemKind, TimelinePayload)>,
    columns: u16,
    rows: u16,
) -> App {
    let mut app = app(columns, rows);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    let items = items
        .into_iter()
        .enumerate()
        .map(|(index, (kind, payload))| vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: index as i64 + 1,
            timestamp_ms: 1_000 + index as i64,
            source: vibex_core::TimelineSource::Agent,
            kind,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload,
        })
        .collect::<Vec<_>>();
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id, items);
    app.sync_transcript();
    app
}

fn screen(app: &mut App, columns: u16, rows: u16) -> String {
    let backend = TestBackend::new(columns, rows);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The resolution the reducer asked the worker to send, if it asked for one.
fn resolution(outcome: &vibex_tui::reduce::Outcome) -> Option<vibex_core::ElicitationResolution> {
    outcome.effects.iter().find_map(|effect| match effect {
        Effect::ResolveElicitation { resolution, .. } => Some(resolution.clone()),
        _ => None,
    })
}

#[test]
fn a_pending_question_opens_its_form_by_itself() {
    // The desktop's card is simply there in the timeline; the terminal has no
    // room for controls in a transcript row, so the card has to come to the
    // reader. A question nobody can find is a question nobody answers.
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    assert!(app.overlay.is_none());
    assert!(app.sync_pending_question());
    assert!(matches!(app.overlay, Some(Overlay::Elicitation { .. })));
    let screen = screen(&mut app, 100, 30);
    assert!(screen.contains("How should I proceed?"), "{screen}");
    assert!(screen.contains("The agent needs your input."), "{screen}");
}

#[test]
fn putting_the_form_down_keeps_it_down() {
    // `Esc` means "later", and a card that reopened on the next frame would
    // make the key useless. A second question is still news.
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    assert!(app.sync_pending_question());
    let outcome = app.perform(Intent::CloseOverlay);
    assert!(outcome.effects.is_empty());
    assert!(app.overlay.is_none());
    assert!(
        !app.sync_pending_question(),
        "a question the reader put down came back"
    );
}

#[test]
fn enter_on_the_question_row_asks_for_the_form_again() {
    // The row is the only place the question is visible without the card, so
    // it is also the way back to it.
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    app.sync_pending_question();
    app.perform(Intent::CloseOverlay);
    app.set_selection(vibex_tui::keymap::Scope::Agent, 0);
    let outcome = app.perform(Intent::OpenBlockDetails);
    assert!(outcome.effects.is_empty());
    assert!(
        matches!(app.overlay, Some(Overlay::Elicitation { .. })),
        "the pending row opened something else: {:?}",
        app.overlay
    );
}

#[test]
fn typing_into_a_text_field_is_what_gets_submitted() {
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    app.sync_pending_question();
    for character in "ship it".chars() {
        app.elicitation_type_char(character);
    }
    let screen = screen(&mut app, 100, 30);
    assert!(screen.contains("ship it"), "{screen}");

    let outcome = app.perform(Intent::ElicitationSubmit);
    let resolution = resolution(&outcome).expect("the answers were sent");
    assert_eq!(resolution.action, ElicitationResolutionAction::Accept);
    assert_eq!(
        resolution.answers.get("q"),
        Some(&ElicitationAnswerValue::String("ship it".into()))
    );
    assert!(
        app.overlay.is_none(),
        "the form stayed open after submitting"
    );
}

#[test]
fn a_required_field_left_blank_keeps_the_form_open() {
    // The Agent is blocked on the answer, so a form that closed on an empty
    // required field would leave it blocked with nothing on screen saying why.
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    app.sync_pending_question();
    let outcome = app.perform(Intent::ElicitationSubmit);
    assert!(resolution(&outcome).is_none());
    assert!(matches!(app.overlay, Some(Overlay::Elicitation { .. })));
    assert!(app.toast.is_some(), "the refusal was silent");
}

#[test]
fn the_arrows_choose_an_option_and_the_digits_choose_one_outright() {
    let mut app = app_with_question(
        vec![choice_field(
            "q",
            "Where should I work?",
            &[
                ("shared", "In the shared tree"),
                ("scratch", "In a scratch copy"),
            ],
        )],
        100,
        30,
    );
    app.sync_pending_question();
    // The cursor opens on the first option, and `Space` takes it.
    let outcome = app.perform(Intent::OverlayToggleValue);
    assert!(outcome.effects.is_empty());
    // The second option is one step down, and a digit picks it outright.
    app.perform(Intent::SelectNext);
    let screen = screen(&mut app, 100, 30);
    assert!(screen.contains("In a scratch copy"), "{screen}");
    app.perform(Intent::OverlayToggleValue);
    let outcome = app.perform(Intent::ConfirmOverlay);
    let resolution = resolution(&outcome).expect("the answers were sent");
    assert_eq!(
        resolution.answers.get("q"),
        Some(&ElicitationAnswerValue::String("scratch".into()))
    );
}

#[test]
fn a_multi_select_ticks_each_option_it_is_given() {
    let mut app = app_with_question(
        vec![multi_field("q", "Which checks?", &["unit", "lint", "docs"])],
        100,
        30,
    );
    app.sync_pending_question();
    app.perform(Intent::OverlayToggleValue);
    app.perform(Intent::SelectNext);
    app.perform(Intent::SelectNext);
    app.perform(Intent::OverlayToggleValue);
    let outcome = app.perform(Intent::ElicitationSubmit);
    let resolution = resolution(&outcome).expect("the answers were sent");
    assert_eq!(
        resolution.answers.get("q"),
        Some(&ElicitationAnswerValue::StringArray(vec![
            "unit".into(),
            "docs".into()
        ]))
    );
}

#[test]
fn a_boolean_is_a_switch_the_same_key_flips() {
    let mut app = app_with_question(vec![boolean_field("q", "Run the migration?")], 100, 30);
    app.sync_pending_question();
    // The cursor's field has no options, so `Space` reaches the switch rather
    // than a list, and the first press is the reader's answer.
    app.perform(Intent::OverlayToggleValue);
    let outcome = app.perform(Intent::ElicitationSubmit);
    let resolution = resolution(&outcome).expect("the answers were sent");
    assert_eq!(
        resolution.answers.get("q"),
        Some(&ElicitationAnswerValue::Boolean(true))
    );
}

#[test]
fn the_form_walks_every_field_and_the_arrows_do_not_stop_at_one() {
    // Two questions, the first with options: `↓` walks the options and then
    // into the next question, which is the one gesture the reader expects from
    // a form. `←` walks back out of it.
    let mut app = app_with_question(
        vec![
            choice_field(
                "first",
                "Where?",
                &[("shared", "Shared"), ("scratch", "Scratch")],
            ),
            text_field("second", "Why?", true),
        ],
        100,
        30,
    );
    app.sync_pending_question();
    // Down past the two options, which is where the next question starts.
    app.perform(Intent::SelectNext);
    app.perform(Intent::SelectNext);
    for character in "because".chars() {
        app.elicitation_type_char(character);
    }
    let screen = screen(&mut app, 100, 30);
    assert!(screen.contains("because"), "{screen}");
    // Back up one field, and `Space` takes the option the cursor lands on.
    app.perform(Intent::ElicitationFieldPrevious);
    app.perform(Intent::OverlayToggleValue);
    let outcome = app.perform(Intent::ElicitationSubmit);
    let resolution = resolution(&outcome).expect("the answers were sent");
    assert_eq!(
        resolution.answers.get("first"),
        Some(&ElicitationAnswerValue::String("shared".into()))
    );
    assert_eq!(
        resolution.answers.get("second"),
        Some(&ElicitationAnswerValue::String("because".into()))
    );
}

#[test]
fn refusing_the_question_sends_a_decline_with_no_answers() {
    // The desktop's Decline button, on the chord the answer fields cannot
    // swallow: a refusal carries no answers, and the request's own validation
    // is what says so.
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    app.sync_pending_question();
    assert_eq!(
        app.keymap.resolve(
            &[vibex_tui::keymap::Scope::Overlay],
            vibex_tui::keymap::Chord::ctrl('d')
        ),
        Some(Intent::ApprovalDeny),
        "Ctrl+D no longer reaches the refusal"
    );
    let outcome = app.perform(Intent::ApprovalDeny);
    let resolution = resolution(&outcome).expect("the refusal was sent");
    assert_eq!(resolution.action, ElicitationResolutionAction::Decline);
    assert!(resolution.answers.is_empty());
    assert!(app.overlay.is_none());
}

#[test]
fn a_request_answered_elsewhere_takes_its_card_with_it() {
    // A phone, or the desktop, can answer the same question. The card must go
    // with the request: it cannot answer a question nobody waits on, and a
    // modal with nothing left to draw would swallow every key in front of it.
    let mut app = app_with_question(vec![text_field("q", "Why?", true)], 100, 30);
    app.sync_pending_question();
    let request_id = app
        .elicitation_form()
        .expect("the form opened")
        .request
        .id
        .clone();

    // The answer lands while the card is open: the projection replaces the
    // pending request with its resolution, the way a live event leaves it.
    let session_id = app.selected_session_id().cloned().expect("a session");
    app.agent.state.timeline.replace_authoritative(
        session_id.clone(),
        vec![vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: 2,
            timestamp_ms: 2_000,
            source: vibex_core::TimelineSource::Agent,
            kind: TimelineItemKind::ElicitationResolution,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload: TimelinePayload::ElicitationResolution(vibex_core::ElicitationResolution {
                request_id,
                session_id,
                action: ElicitationResolutionAction::Accept,
                answers: std::collections::BTreeMap::new(),
                responder_device_id: None,
                resolved_at_ms: 2_000,
            }),
        }],
    );
    assert!(app.sync_pending_question());
    assert!(app.overlay.is_none(), "the card outlived its request");
}

#[test]
fn a_long_form_scrolls_to_the_question_being_answered() {
    // A card taller than the terminal must follow the cursor: the field being
    // answered is the one row that may never be scrolled out of sight.
    let fields = (0..12)
        .map(|index| {
            text_field(
                &format!("q{index}"),
                &format!("Question number {index}"),
                true,
            )
        })
        .collect::<Vec<_>>();
    let mut app = app_with_question(fields, 80, 24);
    app.sync_pending_question();
    let first = screen(&mut app, 80, 24);
    assert!(first.contains("Question number 0"), "{first}");
    for _ in 0..11 {
        app.perform(Intent::ElicitationFieldNext);
    }
    let last = screen(&mut app, 80, 24);
    assert!(last.contains("Question number 11"), "{last}");
}

#[test]
fn the_form_names_the_keys_it_answers_to() {
    let mut app = app_with_question(
        vec![text_field("q", "How should I proceed?", true)],
        100,
        30,
    );
    app.sync_pending_question();
    let screen = screen(&mut app, 100, 30);
    for key in ["Enter", "Submit", "Decline", "Esc"] {
        assert!(screen.contains(key), "{key} is not named:\n{screen}");
    }
}

#[test]
fn a_letter_is_an_answer_in_a_text_field_and_a_shortcut_on_a_list() {
    // The one surface where the same key means two things, decided by the
    // field the cursor is on: `d` is a letter while an answer is being typed,
    // and "decline" while a list of options is under the cursor.
    use vibex_tui::reduce::ElicitationKey;

    let mut app = app_with_question(vec![text_field("q", "Why?", true)], 100, 30);
    app.sync_pending_question();
    assert_eq!(app.elicitation_key('d'), ElicitationKey::Text);
    assert_eq!(app.elicitation_key('1'), ElicitationKey::Text);

    let mut app = app_with_question(
        vec![choice_field(
            "q",
            "Where?",
            &[("shared", "Shared"), ("scratch", "Scratch")],
        )],
        100,
        30,
    );
    app.sync_pending_question();
    assert_eq!(app.elicitation_key('1'), ElicitationKey::Option(0));
    assert_eq!(app.elicitation_key('2'), ElicitationKey::Option(1));
    // A digit that names no option is not an answer to anything here, and
    // neither is a letter: the table decides, which is how `d` declines.
    assert_eq!(app.elicitation_key('3'), ElicitationKey::Elsewhere);
    assert_eq!(app.elicitation_key('d'), ElicitationKey::Elsewhere);
}

#[test]
fn a_reader_mid_sentence_keeps_the_keyboard_until_they_leave_the_composer() {
    // The desktop's card does not take the reader's focus, and neither does
    // this one: a modal that opened over a half-written message would turn the
    // next words they type into an answer. The band keeps counting, and the
    // card is there the moment they step out of the draft.
    let mut app = app_with_question(vec![text_field("q", "Why?", true)], 100, 30);
    app.focus = vibex_tui::app::Focus::Composer;
    assert!(!app.sync_pending_question());
    assert!(app.overlay.is_none());
    app.focus = vibex_tui::app::Focus::Main;
    assert!(app.sync_pending_question());
    assert!(matches!(app.overlay, Some(Overlay::Elicitation { .. })));
}

#[test]
fn the_next_question_is_the_one_that_opens_after_an_answer() {
    // An Agent can raise more than one question. The form answers the request
    // it opened on — the draft is keyed by that request, so nothing typed for
    // one can be sent with another — and the next one takes its place.
    let session_id = VibexSessionId::new();
    let questions = ["First question", "Second question"].map(|title| ElicitationRequest {
        id: RequestId::new(),
        session_id: session_id.clone(),
        provider_request_id: None,
        tool_call_id: None,
        message: title.to_string(),
        title: Some(title.to_string()),
        description: None,
        fields: vec![text_field("q", title, true)],
        status: ElicitationRequestStatus::Pending,
        requested_at_ms: 1_000,
    });
    let mut app = app_with_items(
        session_id,
        questions
            .into_iter()
            .map(|request| {
                (
                    TimelineItemKind::ElicitationRequest,
                    TimelinePayload::ElicitationRequest(request),
                )
            })
            .collect(),
        100,
        30,
    );
    app.sync_pending_question();
    let first = screen(&mut app, 100, 30);
    assert!(first.contains("First question"), "{first}");

    app.elicitation_type_char('x');
    let outcome = app.perform(Intent::ElicitationSubmit);
    let resolution = resolution(&outcome).expect("the first answer was sent");
    assert_eq!(
        resolution.answers.get("q"),
        Some(&ElicitationAnswerValue::String("x".into()))
    );
    // The runtime has not answered yet, so the first request is still pending
    // and stays offered; the second is not, and opens on the next sync.
    assert!(app.sync_pending_question());
    let second = screen(&mut app, 100, 30);
    assert!(second.contains("Second question"), "{second}");
}

#[test]
fn an_approval_is_a_blocking_question_too() {
    // The same card, the same problem: the desktop draws an approval inline in
    // the timeline and the transcript here has no controls in it, so a pending
    // approval opens itself as well.
    let session_id = VibexSessionId::new();
    let request = vibex_core::PermissionRequest {
        id: RequestId::new(),
        session_id: session_id.clone(),
        project_id: None,
        workspace_id: None,
        provider_request_id: None,
        risk_category: vibex_core::PermissionRiskCategory::Command,
        title: "Run the test suite".to_string(),
        details: Vec::new(),
        allowed_responses: vec![
            vibex_core::PermissionResponseKind::Approve,
            vibex_core::PermissionResponseKind::Deny,
        ],
        response_options: Vec::new(),
        status: vibex_core::PermissionRequestStatus::Pending,
        requested_at_ms: 1_000,
        expires_at_ms: None,
    };
    let mut app = app_with_items(
        session_id,
        vec![(
            TimelineItemKind::PermissionRequest,
            TimelinePayload::PermissionRequest(request),
        )],
        100,
        30,
    );
    assert!(app.sync_pending_question());
    assert!(matches!(app.overlay, Some(Overlay::Approval { .. })));
    let screen = screen(&mut app, 100, 30);
    assert!(screen.contains("Run the test suite"), "{screen}");
}
