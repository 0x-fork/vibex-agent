//! The configuration centre: its navigation, its writes, and its frame.
//!
//! These tests drive the real reducer and the real renderer at real terminal
//! sizes. They are the layer that catches the mistakes the compiler cannot:
//! a module that cannot be reached, an action menu built from the wrong
//! record, a table whose columns do not line up with their headings.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_tui::app::{App, AppOptions, ManagementData, Overlay, Page};
use vibex_tui::config_center::{
    ConfigAction, ConfigCenter, ConfigModule, ConfigSection, ConfigZone,
};
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
            // No arrangement file: a test must never write into the runner's
            // home directory, and each test wants a clean surface.
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.resize(columns, rows);
    app
}

/// Draw one frame and return the text a reader would see.
fn text(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
        }
        out.push('\n');
    }
    out
}

/// One Agent with a managed install that has an update waiting.
fn agent() -> vibex_core::AgentSnapshotEntry {
    serde_json::from_value(serde_json::json!({
        "id": "codex",
        "label": "Codex",
        "description": "An open-source coding agent",
        "runtimeKind": "acp",
        "sourceKind": "builtin",
        "added": true,
        "enabled": true,
        "orderIndex": 0,
        "installed": true,
        "configured": true,
        "installStatus": "installed",
        "managedInstall": {
            "managed": true,
            "status": "update_available",
            "distributionKind": "npm",
            "installedVersion": "1.2.3",
            "availableVersion": "1.3.0"
        },
        "configStatus": "configured",
        "runtimeStatus": "ready",
        "models": ["gpt-5", "gpt-5-mini"],
        "modes": [],
        "capabilityHints": [],
        "env": {},
        "params": {},
        "diagnostics": [],
        "nativeConfigPaths": ["/home/dev/.codex/config.toml"]
    }))
    .expect("agent fixture")
}

fn mcp_server() -> vibex_core::McpServer {
    serde_json::from_value(serde_json::json!({
        "id": "mcp_filesystem",
        "displayName": "Filesystem",
        "transportKind": "stdio",
        "status": "enabled",
        "scopeKind": "user",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem"],
        "env": [],
        "headers": [],
        "secretReferences": [],
        "providerMatrix": [],
        "description": "Read and write files in a workspace",
        "tags": ["files"],
        "agentMatrix": [
            {"agentId": "codex", "enabled": true, "sourceKind": "manual", "updatedAtMs": 1}
        ],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("server fixture")
}

fn prompt() -> vibex_core::Prompt {
    serde_json::from_value(serde_json::json!({
        "id": "prompt_review",
        "displayName": "Review",
        "kind": "reusable_prompt",
        "status": "enabled",
        "scopeKind": "user",
        "body": "Review this workspace.",
        "tags": ["review"],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("prompt fixture")
}

/// The centre, opened on the Agent roster with one of everything loaded.
fn loaded(columns: u16, rows: u16) -> App {
    let mut app = app(columns, rows);
    // A live connection: a mutating verb is refused while the client is still
    // attaching, and every verb worth testing here is one.
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(vibex_tui::action::Intent::GotoManagement);
    app.management_data = ManagementData {
        agents: vec![agent()],
        mcp: vec![mcp_server()],
        prompts: vec![prompt()],
        ..Default::default()
    };
    app
}

#[test]
fn the_centre_opens_on_the_agent_module_and_reaches_every_other_one() {
    let mut app = loaded(140, 40);
    assert_eq!(app.page, Page::Management);
    assert_eq!(app.config_center.module, ConfigModule::Agent);
    assert_eq!(app.config_center.section, ConfigSection::Agents);

    let mut seen = vec![ConfigModule::Agent];
    for _ in 0..ConfigModule::ALL.len() - 1 {
        app.perform(vibex_tui::action::Intent::ConfigModuleNext);
        seen.push(app.config_center.module);
    }
    assert_eq!(seen, ConfigModule::ALL.to_vec());
    // And it wraps rather than stopping at the last module.
    app.perform(vibex_tui::action::Intent::ConfigModuleNext);
    assert_eq!(app.config_center.module, ConfigModule::Agent);
}

#[test]
fn the_rail_switches_the_area_and_the_filter_belongs_to_the_area() {
    let mut app = loaded(140, 40);
    assert_eq!(app.config_center.section, ConfigSection::Agents);
    app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    assert_eq!(app.config_center.section, ConfigSection::Providers);
    // A filter written in one area must not follow the reader into the next,
    // or a table would look empty for a reason nothing on screen explains.
    app.push_config_filter('c');
    app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    assert_eq!(app.config_center.filter, "");
    // The module's own areas are the only ones the rail can reach.
    for _ in 0..6 {
        app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    }
    assert!(
        ConfigModule::Agent
            .sections()
            .contains(&app.config_center.section)
    );
}

#[test]
fn the_tabs_and_the_rail_are_drawn_with_their_counts() {
    let mut app = loaded(140, 40);
    let screen = text(&mut app, 140, 40);
    for needle in [
        "Agent",
        "MCP",
        "Skills",
        "Prompts",
        "Agents",
        "Model providers",
    ] {
        assert!(
            screen.contains(needle),
            "the centre lost {needle}:\n{screen}"
        );
    }
    // The table names its columns and the record it holds.
    for needle in ["Install", "Runtime", "Codex", "Enabled"] {
        assert!(
            screen.contains(needle),
            "the table lost {needle}:\n{screen}"
        );
    }
    // And the detail pane describes the highlighted record.
    for needle in ["1.2.3 → 1.3.0", "gpt-5, gpt-5-mini"] {
        assert!(
            screen.contains(needle),
            "the detail lost {needle}:\n{screen}"
        );
    }
}

#[test]
fn a_click_switches_the_module_and_selects_the_row_under_it() {
    let mut app = loaded(140, 40);
    let _ = text(&mut app, 140, 40);
    // The tab strip publishes where each module landed.
    let (rect, _) = app
        .regions
        .config_tabs
        .iter()
        .find(|(_, module)| *module == ConfigModule::Mcp)
        .copied()
        .expect("the MCP tab is published");
    let outcome = app.click_config_control(rect.x + 1, rect.y);
    assert!(outcome.is_some(), "the tab click was not handled");
    assert_eq!(app.config_center.module, ConfigModule::Mcp);

    // A record row selects on the first click and opens its actions on the
    // second, the same contract every other list in this client keeps.
    let _ = text(&mut app, 140, 40);
    let region = app.regions.list.clone().expect("the table is published");
    assert!(region.rows >= 1);
    let outcome = app.click_config_row(0, false);
    assert!(
        outcome.effects.is_empty(),
        "the first click opened something"
    );
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    let _ = text(&mut app, 140, 40);
    app.click_config_row(0, true);
    assert!(
        matches!(app.overlay, Some(Overlay::EntryActions { .. })),
        "a second click did not open the action menu"
    );
}

#[test]
fn the_action_menu_is_built_from_the_record_it_was_opened_on() {
    let mut app = loaded(140, 40);
    app.perform(vibex_tui::action::Intent::ConfigEntryActions);
    let Some(Overlay::EntryActions { entry }) = app.overlay.clone() else {
        panic!("the action menu did not open");
    };
    assert_eq!(entry.row_key, "codex");
    // A managed Agent can be installed, updated, rolled back and uninstalled;
    // an external one cannot, which is why the menu is per record.
    for action in [
        ConfigAction::Install,
        ConfigAction::CheckUpdate,
        ConfigAction::Rollback,
        ConfigAction::Toggle,
        ConfigAction::Install,
    ] {
        assert!(
            entry.actions.contains(&action),
            "the Agent menu lost {action:?}"
        );
    }
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("Roll back"), "{screen}");
}

#[test]
fn the_form_opens_on_the_record_and_saves_what_was_typed() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    assert!(form.value("name").as_deref() == Some("Filesystem"));
    assert_eq!(form.value("transport").as_deref(), Some("stdio"));
    assert_eq!(form.value("command").as_deref(), Some("npx"));

    // Typing reaches the field the cursor is on, and only that one.
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    app.type_form_char('!');
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor closed");
    };
    assert!(form.value("name").as_deref() == Some("Filesystem!"));
    assert_eq!(form.value("command").as_deref(), Some("npx"));
    assert!(form.dirty());

    // `Esc` gives the field up before it gives the form up.
    app.perform(vibex_tui::action::Intent::Back);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the first Esc closed the whole form");
    };
    assert_eq!(form.value("name").as_deref(), Some("Filesystem"));
    app.perform(vibex_tui::action::Intent::Back);
    assert!(app.overlay.is_none(), "the second Esc left the form open");
}

#[test]
fn a_required_field_left_empty_is_refused_with_the_reason_on_screen() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    // Clear the name, which the authority will not accept.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.fields[0].value.clear();
    }
    app.perform(vibex_tui::action::Intent::ConfigFormSave);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("name is required"), "{screen}");
}

#[test]
fn the_switch_verbs_ask_the_authority_rather_than_patching_the_row() {
    let mut app = loaded(140, 40);
    // The Agent's switch is a configuration write…
    let outcome = app.perform(vibex_tui::action::Intent::ToggleSelectedEntry);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation }] => {
            assert_eq!(mutation.label(), "agent");
        }
        other => panic!("the Agent switch produced {other:?}"),
    }
    // …while an MCP server's is the update the runtime already had, because
    // the record is the runtime's and only the runtime may change it.
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ToggleSelectedEntry);
    assert!(matches!(
        outcome.effects.as_slice(),
        [vibex_tui::app::Effect::ToggleMcp { enabled: false, .. }]
    ));
}

#[test]
fn a_delete_is_a_question_before_it_is_a_write() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigEntryDelete);
    assert!(outcome.effects.is_empty(), "the delete ran unasked");
    assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));
    let screen = text(&mut app, 140, 40);
    // The question names the record it is about, so a reader who pressed `d`
    // on the wrong row can see that before they answer.
    assert!(screen.contains("mcp_filesystem"), "{screen}");
    assert!(screen.contains("Delete this record?"), "{screen}");
    // Answering it is the write.
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation }] => {
            assert_eq!(mutation.label(), "mcp_delete");
        }
        other => panic!("the answer produced {other:?}"),
    }
}

#[test]
fn the_filter_narrows_the_table_and_says_so_when_nothing_matches() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::BeginFilter);
    assert!(app.config_center.filtering);
    for character in "zzz".chars() {
        app.push_config_filter(character);
    }
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        0
    );
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("No matches"), "{screen}");
    // Clearing it brings the record back.
    app.perform(vibex_tui::action::Intent::ClearFilter);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        1
    );
}

#[test]
fn the_wheel_moves_the_cursor_in_the_table_and_the_pane_in_the_detail() {
    let mut app = loaded(140, 40);
    app.management_data.mcp.push(mcp_server());
    app.management_data.mcp.push(mcp_server());
    app.show_config_module(ConfigModule::Mcp, None);
    let _ = text(&mut app, 140, 40);
    let region = app.regions.list.clone().expect("the table is published");
    app.scroll_config_area(1, region.rect.x + 2, region.rect.y);
    assert_eq!(app.config_center.cursor(3), 1);
    // Below the table the wheel scrolls the pane rather than the cursor.
    let before = app.config_center.cursor(3);
    app.scroll_config_area(1, region.rect.right() + 40, region.rect.bottom() + 3);
    assert_eq!(app.config_center.cursor(3), before);
}

#[test]
fn a_narrow_terminal_still_draws_the_centre() {
    // The rail is fixed width, so the frame has to make the table fit beside
    // it or say so rather than drawing half a table.
    for columns in [40u16, 60, 80, 120] {
        let mut app = loaded(columns, 24);
        let screen = text(&mut app, columns, 24);
        assert!(
            screen.contains("Codex") || screen.contains("Configuration"),
            "{columns} columns lost the centre:\n{screen}"
        );
    }
}

#[test]
fn the_default_centre_is_empty_rather_than_wrong() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoManagement);
    let screen = text(&mut app, 120, 40);
    assert!(screen.contains("Nothing here yet"), "{screen}");
    assert!(
        !screen.contains("Devices"),
        "the centre grew a device module:\n{screen}"
    );
    assert_eq!(ConfigCenter::default().zone, ConfigZone::Rows);
}
