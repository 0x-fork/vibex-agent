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
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
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
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
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

#[test]
fn the_three_panes_wrap_when_the_left_and_right_keys_walk_them() {
    let mut app = loaded(140, 40);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    // The three panes are the ones the surface draws, in reading order.
    assert_eq!(
        ConfigZone::ALL,
        [ConfigZone::Modules, ConfigZone::Rail, ConfigZone::Rows]
    );
    let mut walked = vec![app.config_center.zone];
    for _ in 0..2 {
        app.perform(vibex_tui::action::Intent::ConfigPaneNext);
        walked.push(app.config_center.zone);
    }
    assert_eq!(
        walked,
        [ConfigZone::Rows, ConfigZone::Modules, ConfigZone::Rail]
    );
    // The fourth press is the first again: `→` never dead-ends.
    app.perform(vibex_tui::action::Intent::ConfigPaneNext);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    // And `←` walks the other way, also wrapping.
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Rail);
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Modules);
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
}

#[test]
fn the_focused_pane_is_named_on_screen_and_the_others_are_not() {
    let mut app = loaded(140, 40);
    for zone in ConfigZone::ALL {
        while app.config_center.zone != zone {
            app.perform(vibex_tui::action::Intent::ConfigPaneNext);
        }
        let screen = text(&mut app, 140, 40);
        let expected = format!("Focus · {}", zone.label(app.strings));
        assert!(
            screen.contains(&expected),
            "the footer does not name the {zone:?} pane:\n{screen}"
        );
        // Exactly one pane is named: a second focus line would be a second
        // answer to "where are my keys going".
        let named = ["Focus · Modules", "Focus · Areas", "Focus · Records"]
            .iter()
            .filter(|needle| screen.contains(*needle))
            .count();
        assert_eq!(named, 1, "two panes claimed the focus:\n{screen}");
    }
}

#[test]
fn the_heading_row_sits_over_the_cells_it_names() {
    use ratatui::backend::TestBackend;

    let mut app = loaded(160, 40);
    // A wide glyph in the record's own name: it is two columns wide, and a
    // table padded by character count walks every cell after it out of line by
    // one column per glyph.
    if let Some(agent) = app.management_data.agents.first_mut() {
        agent.label = "代码助手".to_string();
    }
    if let Some(server) = app.management_data.mcp.first_mut() {
        server.display_name = "文件系统".to_string();
    }
    for (module, section) in [
        (ConfigModule::Agent, ConfigSection::Agents),
        (ConfigModule::Mcp, ConfigSection::Servers),
    ] {
        app.show_config_module(module, None);
        let columns = vibex_tui::config_center::columns(module, section, app.strings);
        let rows = app.config_center.visible(&app.management_data, app.strings);
        let first = rows
            .first()
            .unwrap_or_else(|| panic!("{module:?} has no row"));

        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| vibex_tui::view::render(frame, &mut app))
            .expect("frame draws");
        let buffer = terminal.backend().buffer().clone();

        // The columns are positions in the *drawn* frame, so the check is
        // positional: a heading and the cell it names must start in the same
        // terminal column, whatever either of them holds.
        let heading_row = (0..buffer.area.height)
            .find(|row| {
                buffer_find(&buffer, *row, columns[0]).is_some()
                    && buffer_find(&buffer, *row, "●").is_none()
                    && buffer_find(&buffer, *row, "○").is_none()
            })
            .unwrap_or_else(|| panic!("no heading row for {module:?}"));
        let record_row = (0..buffer.area.height)
            .find(|row| {
                buffer_find(&buffer, *row, "●").is_some()
                    || buffer_find(&buffer, *row, "○").is_some()
            })
            .unwrap_or_else(|| panic!("no record row for {module:?}"));

        let mut previous = 0u16;
        for (index, column) in columns.iter().enumerate() {
            let Some(cell) = first.cells.get(index) else {
                panic!("{module:?} has a heading with no cell under it: {column}");
            };
            let heading_at = buffer_find(&buffer, heading_row, column)
                .unwrap_or_else(|| panic!("{column} is not in the heading row"));
            // The cell is looked for *from* the heading's own column: a short
            // cell like `2` also appears inside a longer one to its left, and a
            // search from the start of the row would compare a position with
            // itself.
            let record_at = buffer_find_from(&buffer, record_row, cell, heading_at)
                .unwrap_or_else(|| panic!("{cell} is not under {column} in the record row"));
            assert_eq!(
                heading_at, record_at,
                "the {column} heading of {module:?} is not over its cell"
            );
            assert!(
                heading_at >= previous,
                "the headings of {module:?} are out of order"
            );
            previous = heading_at;
        }
    }
}

/// The terminal column a word starts at in one drawn row, if it is there.
///
/// A word is matched against the cells themselves rather than against a
/// string built from them: a wide glyph occupies two cells and the second one
/// is empty, so a string index would drift one column per wide glyph and the
/// alignment this test is about would be invisible.
fn buffer_find(buffer: &ratatui::buffer::Buffer, row: u16, needle: &str) -> Option<u16> {
    buffer_find_from(buffer, row, needle, 0)
}

/// As [`buffer_find`], starting the search at `from`.
fn buffer_find_from(
    buffer: &ratatui::buffer::Buffer,
    row: u16,
    needle: &str,
    from: u16,
) -> Option<u16> {
    let symbols = (0..buffer.area.width)
        .map(|column| {
            buffer
                .cell((column, row))
                .map(|cell| cell.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let wanted = needle.chars().map(|ch| ch.to_string()).collect::<Vec<_>>();
    (usize::from(from)..symbols.len())
        .find(|start| {
            wanted
                .iter()
                .enumerate()
                .all(|(offset, expected)| symbols.get(start + offset) == Some(expected))
        })
        .map(|start| start as u16)
}

#[test]
fn the_agent_module_has_no_health_area_and_prompts_have_no_hooks() {
    assert_eq!(
        ConfigModule::Agent.sections(),
        [ConfigSection::Agents, ConfigSection::Providers]
    );
    assert_eq!(ConfigModule::Prompts.sections(), [ConfigSection::Prompts]);
    // Every section still in the model is reachable from some module, so a
    // variant cannot outlive the area that drew it.
    for section in [
        ConfigSection::Agents,
        ConfigSection::Providers,
        ConfigSection::Servers,
        ConfigSection::Installed,
        ConfigSection::Catalog,
        ConfigSection::Sources,
        ConfigSection::Prompts,
    ] {
        assert!(
            ConfigModule::ALL
                .iter()
                .any(|module| module.sections().contains(&section)),
            "{section:?} is unreachable"
        );
    }
    let mut app = loaded(140, 40);
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("Health"), "{screen}");
    assert!(!screen.contains("Hook"), "{screen}");
    app.show_config_module(ConfigModule::Prompts, None);
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("Hook"), "{screen}");
}

#[test]
fn the_page_keys_scroll_the_detail_that_the_cursor_describes() {
    let mut app = loaded(140, 40);
    // Nothing to scroll until the pane holds more lines than it has rows, so
    // give the highlighted record enough to overflow it.
    if let Some(agent) = app.management_data.agents.first_mut() {
        for index in 0..40 {
            agent
                .native_config_paths
                .push(format!("/tmp/config-{index}.toml"));
        }
    }
    let before = app.config_center.detail_scroll;
    app.perform(vibex_tui::action::Intent::ScrollPageDown);
    assert!(
        app.config_center.detail_scroll > before,
        "the page key did not move the detail pane"
    );
    // The cursor is untouched: the detail belongs to the records pane, and the
    // page keys move the pane rather than the row it describes.
    assert_eq!(app.config_center.cursor(1), 0);
    app.perform(vibex_tui::action::Intent::ScrollPageUp);
    assert_eq!(app.config_center.detail_scroll, before);
}

#[test]
fn every_agent_verb_says_what_it_is_doing_on_the_row_it_was_pressed_on() {
    // The complaint this covers: a verb ran and left the reader unable to tell
    // whether the key had done anything at all, let alone what. Each verb marks
    // the record it was about, and the action line says the verb rather than
    // the effect key the write travels under.
    use vibex_tui::config_center::ConfigVerb;
    for (action, verb) in [
        (ConfigAction::Install, ConfigVerb::Install),
        (ConfigAction::CheckUpdate, ConfigVerb::CheckUpdate),
        (ConfigAction::Rollback, ConfigVerb::Rollback),
        (ConfigAction::RefreshSnapshot, ConfigVerb::RefreshSnapshot),
        (ConfigAction::Auth, ConfigVerb::SignIn),
        (ConfigAction::Logout, ConfigVerb::SignOut),
        (ConfigAction::Uninstall, ConfigVerb::Uninstall),
        // The fixture's Agent is enabled, so the switch asks for "disable".
        (ConfigAction::Toggle, ConfigVerb::Disable),
    ] {
        let mut app = loaded(140, 40);
        // These verbs are the authority's, and a client that never saw it
        // advertise them refuses them: the row is marked only over work that
        // was actually asked for. The Agent's accounts are the agent domain's;
        // its install is the management domain's.
        app.capabilities.agent = vibex_backend::DomainCapabilities::available([
            vibex_backend::BackendOperation::AgentAuthRead,
            vibex_backend::BackendOperation::AgentAuthManage,
        ]);
        app.capabilities.management = vibex_backend::DomainCapabilities::available([
            vibex_backend::BackendOperation::ManagementAgents,
        ]);
        // The idle row wears its status mark; a working one wears a spinner
        // instead, so its absence is what says the marker moved.
        assert!(text(&mut app, 140, 40).contains('●'));
        app.run_config_action_by_key("codex", action);
        // Uninstall asks before it runs, and answering the question is what
        // starts the write: the row is not marked over a question.
        if matches!(app.overlay, Some(Overlay::Confirm { .. })) {
            app.perform(vibex_tui::action::Intent::ConfirmOverlay);
        }

        let pending = app
            .config_center
            .pending
            .get("codex")
            .unwrap_or_else(|| panic!("{action:?} left the row silent"));
        assert_eq!(pending.verb, verb, "{action:?} named the wrong verb");
        assert_eq!(
            pending.subject, "Codex",
            "{action:?} named the wrong record"
        );

        let screen = text(&mut app, 140, 40);
        assert!(
            screen.contains(verb.running(app.strings)),
            "{action:?} says nothing while it works:\n{screen}"
        );
        assert!(
            !screen.contains('●'),
            "{action:?} left the row looking idle while it worked:\n{screen}"
        );
    }
}

#[test]
fn a_second_press_on_a_working_record_is_refused_rather_than_queued() {
    // Two of the same call in flight is how a row ends up describing a write
    // that finished before the one it is waiting on. The second press is
    // answered rather than sent.
    let mut app = loaded(140, 40);
    let first = app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    assert_eq!(
        first.effects.len(),
        1,
        "the first press did not ask for work"
    );

    let second = app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    assert!(
        second.effects.is_empty(),
        "the same call was queued a second time"
    );
    let toast = app.toast.as_ref().expect("the refusal was silent");
    assert_eq!(toast.tone, vibex_tui::app::ToastTone::Warning);

    // Once it has answered, the record is free again.
    app.config_center.finish_pending("codex");
    let third = app.run_config_action_by_key("codex", ConfigAction::RefreshSnapshot);
    assert_eq!(
        third.effects.len(),
        1,
        "the record stayed blocked afterwards"
    );
}

#[test]
fn what_a_write_answered_is_drawn_under_the_table_after_the_toast_is_gone() {
    // The toast is gone in seconds; a reader who looked away would have to
    // repeat the action to find out what it did. The line under the table is
    // the same sentence, kept.
    use vibex_tui::config_center::ConfigTone;
    let mut app = loaded(140, 40);
    let answer = "Codex: Installed · version 9.9.9";
    assert!(!text(&mut app, 140, 40).contains(answer));

    app.config_center.set_notice(answer, ConfigTone::Ok);
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains(answer), "{screen}");
    // The verbs are still on the line: a notice reports the last write without
    // taking away the next one.
    assert!(screen.contains("Install"), "{screen}");
}

/// One drawn row of a frame, one string per terminal column.
fn row_symbols(screen: &str, row: usize) -> Vec<String> {
    screen
        .lines()
        .nth(row)
        .expect("the row was drawn")
        .chars()
        .map(|character| character.to_string())
        .collect()
}

#[test]
fn a_notice_shifts_the_verb_buttons_without_losing_the_click() {
    // The notice is drawn in front of the verbs, so the buttons move right when
    // one is showing. Each frame republishes where they landed and a press is
    // resolved against that, which is what keeps a mouse press on the verb the
    // reader can see rather than on the one that used to be there.
    use vibex_tui::config_center::{ConfigTone, ConfigVerb};
    let mut app = loaded(140, 40);
    app.capabilities.management = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::ManagementAgents,
    ]);
    app.config_center
        .set_notice("Codex: Installed · version 1.3.0", ConfigTone::Ok);
    let screen = text(&mut app, 140, 40);

    let (rect, action) = app
        .regions
        .config_buttons
        .iter()
        .find(|(_, action)| *action == ConfigAction::CheckUpdate)
        .copied()
        .expect("the verbs were not published as buttons");
    let drawn: String = row_symbols(&screen, usize::from(rect.y))
        [usize::from(rect.x)..usize::from(rect.x + rect.width)]
        .concat();
    assert_eq!(drawn, format!(" {} ", action.label(app.strings)));

    app.click_config_control(rect.x + 2, rect.y)
        .expect("the press found no control");
    let pending = app
        .config_center
        .pending
        .get("codex")
        .expect("the press did not start the verb it was over");
    assert_eq!(pending.verb, ConfigVerb::CheckUpdate);
}
