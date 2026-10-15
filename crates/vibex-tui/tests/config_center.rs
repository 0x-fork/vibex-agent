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

/// The spinner frame drawn on the line that says `text`, if it wears one.
///
/// Found by the words rather than by the row's number: the table's rows move
/// with the module and the filter, and what the test is about is the mark that
/// stands beside the sentence a write put there.
fn spinner_before(screen: &str, text: &str) -> Option<&'static str> {
    let line = screen.lines().find(|line| line.contains(text))?;
    vibex_tui::glyphs::spinner_frames(vibex_tui::glyphs::GlyphTier::Full)
        .iter()
        .copied()
        .find(|frame| line.contains(frame))
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
fn a_write_in_flight_keeps_the_marker_turning_until_it_answers() {
    // The working row replaces its status mark with a spinner, and that spinner
    // is drawn from the animation phase. A clock that stopped under the write
    // left the marker standing on one frame for the whole round trip, which is
    // the one thing a write marker must not do: it is there to say the press
    // landed.
    let mut app = loaded(140, 40);
    assert!(
        !app.chrome_animating(),
        "the idle centre is asking for frames"
    );

    app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    let verb = app
        .config_center
        .pending
        .get("codex")
        .expect("the row is waiting")
        .verb;
    let working = verb.running(app.strings);

    let mut drawn = Vec::new();
    for _ in 0..4 {
        let screen = text(&mut app, 140, 40);
        drawn.push(
            spinner_before(&screen, working)
                .unwrap_or_else(|| panic!("the row says it works with nothing turning:\n{screen}"))
                .to_string(),
        );
        assert!(
            app.chrome_animating(),
            "the centre stopped asking for frames under a write"
        );
        // Each frame is held for a few ticks, so a frame that moved is a clock
        // that ran rather than one the renderer happened to sample late.
        for _ in 0..vibex_tui::glyphs::SPINNER_TICKS_PER_FRAME {
            assert!(
                app.advance_transcript_animation(),
                "the clock stopped under a write"
            );
        }
    }
    assert!(
        drawn.iter().any(|frame| *frame != drawn[0]),
        "the marker never moved: {drawn:?}"
    );

    // The answer releases the clock with the marker: an answered record is a
    // quiet page again.
    app.config_center.finish_pending("codex");
    let screen = text(&mut app, 140, 40);
    assert!(
        spinner_before(&screen, working).is_none(),
        "the row kept turning after its answer:\n{screen}"
    );
    assert!(
        !app.chrome_animating(),
        "the answered centre keeps asking for frames"
    );
    assert!(!app.advance_transcript_animation());
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

/// One Agent other than the built-in `codex`, so a matrix has two rows.
fn second_agent() -> vibex_core::AgentSnapshotEntry {
    serde_json::from_value(serde_json::json!({
        "id": "claude",
        "label": "Claude",
        "runtimeKind": "acp",
        "sourceKind": "builtin",
        "added": true,
        "enabled": true,
        "orderIndex": 1,
        "installed": true,
        "configured": true,
        "installStatus": "installed",
        "managedInstall": {"managed": false, "status": "external"},
        "configStatus": "configured",
        "runtimeStatus": "ready",
        "models": [],
        "modes": [],
        "capabilityHints": [],
        "env": {},
        "params": {},
        "diagnostics": [],
        "nativeConfigPaths": []
    }))
    .expect("agent fixture")
}

/// One model provider profile, as the Agent's own list reports it.
fn provider_profile(id: &str, name: &str) -> vibex_core::ProviderProfileSummary {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "agentId": "codex",
        "kind": "codex",
        "displayName": name,
        "status": "enabled",
        "accountAlias": "work",
        "defaultModel": "gpt-5",
        "configuredModels": [{
            "id": "gpt-5",
            "enabled": true,
            "capabilities": {}
        }],
        "secretSetupState": "missing",
        "updatedAtMs": 1
    }))
    .expect("provider fixture")
}

/// One registry entry, as the market page lists it.
fn market_entry(id: &str) -> vibex_core::McpMarketEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": "Trace tools",
        "description": "Inspect a trace",
        "transport": "stdio",
        "command": "npx",
        "args": ["-y", "@example/trace"],
        "env": [{
            "name": "TRACE_TOKEN",
            "required": true,
            "secret": true,
            "defaultValue": "from-registry"
        }],
        "version": "1.0.0",
        "author": "example",
        "packageKind": "npm",
        "status": "active"
    }))
    .expect("market fixture")
}

/// One installable Skill, as the Skill index lists it.
fn skill_market_entry(id: &str) -> vibex_core::SkillMarketEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "slug": id,
        "ownerHandle": "example",
        "name": "Trace reader",
        "summary": "Read a trace",
        "version": "1.0.0",
        "installs": 3,
        "stars": 1,
        "downloads": 9
    }))
    .expect("skill market fixture")
}

/// The action the menu offers, chosen the way the reader chooses it.
fn choose_action(app: &mut App, action: ConfigAction) {
    let Some(Overlay::EntryActions { mut entry }) = app.overlay.clone() else {
        panic!("the action menu was not open");
    };
    entry.selected = entry
        .actions
        .iter()
        .position(|offered| *offered == action)
        .unwrap_or_else(|| panic!("the record does not offer {action:?}"));
    app.overlay = Some(Overlay::EntryActions { entry });
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
}

/// `Edit` in the action menu is the editor, not a verb with no write behind it.
///
/// The menu is the surface's front door — `Enter` opens it on every record — so
/// a verb it offers and the reducer does not answer is a key that does nothing
/// at all. It is offered by four modules, and each of them has an editor to
/// open.
#[test]
fn the_action_menu_opens_the_editor_for_the_record_it_was_opened_on() {
    for (module, section, expected) in [
        (ConfigModule::Mcp, ConfigSection::Servers, "Filesystem"),
        (ConfigModule::Prompts, ConfigSection::Prompts, "Review"),
    ] {
        let mut app = loaded(140, 40);
        app.show_config_module(module, Some(section));
        app.perform(vibex_tui::action::Intent::ConfigEntryActions);
        choose_action(&mut app, ConfigAction::Edit);
        let Some(Overlay::Form { form }) = app.overlay.clone() else {
            panic!("{module:?}: Edit opened no editor");
        };
        assert_eq!(
            form.value("name").as_deref(),
            Some(expected),
            "{module:?}: the editor opened on another record"
        );
    }
}

/// A provider's secret is written to the profile the reader pressed on.
///
/// The prompt is one line of text with no room for the record it is about, so
/// the target travels with it. Reading it back from the list cursor instead
/// would send the key to whichever profile happened to be under the cursor when
/// the reader pressed `Enter` — and a read landing while they type is enough to
/// move it.
#[test]
fn a_provider_secret_is_written_to_the_profile_it_was_asked_for() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    let outcome = app.run_config_action_by_key("provider_second", ConfigAction::Secret);
    assert!(outcome.effects.is_empty(), "the prompt wrote something");
    let Some(Overlay::Prompt { target, .. }) = app.overlay.clone() else {
        panic!("the secret prompt did not open");
    };
    assert_eq!(target.as_deref(), Some("provider_second"));

    // The list moving under the prompt must not move the write.
    app.set_selection(vibex_tui::keymap::Scope::Providers, 0);
    if let Some(Overlay::Prompt { value, .. }) = app.overlay.as_mut() {
        *value = "sk-sentinel".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::WriteProviderSecret { profile_id, secret }] => {
            assert_eq!(profile_id.as_str(), "provider_second");
            assert_eq!(secret, "sk-sentinel");
        }
        other => panic!("the secret produced {other:?}"),
    }
}

/// `Validate` on a registry entry checks the launcher that would be installed.
///
/// The entry has no record yet, so the check has to carry the candidate the
/// registry published. A menu that offers the verb and answers nothing is worse
/// than one that does not offer it: the reader is told the entry is fine by a
/// client that never looked.
#[test]
fn a_registry_entry_can_be_checked_before_it_is_installed() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let outcome = app.run_config_action_by_key("registry_trace", ConfigAction::Validate);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            assert_eq!(mutation.label(), "mcp_market_validate");
            let vibex_tui::app::ConfigMutation::ValidateMcpMarketEntry { candidate, .. } = mutation
            else {
                panic!("the check did not carry a candidate: {mutation:?}");
            };
            assert_eq!(candidate.command.as_deref(), Some("npx"));
            assert_eq!(
                candidate
                    .env
                    .iter()
                    .map(|entry| (entry.name.as_str(), entry.value.as_str()))
                    .collect::<Vec<_>>(),
                vec![("TRACE_TOKEN", "from-registry")]
            );
        }
        other => panic!("the check produced {other:?}"),
    }
}

/// An Agent the record never held can be switched on in the matrix form.
///
/// The form draws every Agent the client knows about, so it is the set the
/// matrix is rebuilt from: rebuilding it out of the record's own entries would
/// silently drop the one press the reader made, which is the only reason they
/// opened the form.
#[test]
fn the_matrix_form_enables_an_agent_the_record_did_not_hold() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent(), second_agent()];
    app.show_config_module(ConfigModule::Mcp, None);
    // The fixture server is assigned to `codex` alone.
    app.perform(vibex_tui::action::Intent::ConfigEntryActions);
    choose_action(&mut app, ConfigAction::Matrix);
    let Some(Overlay::Form { form }) = app.overlay.as_mut() else {
        panic!("the matrix form did not open");
    };
    let field = form
        .fields
        .iter_mut()
        .find(|field| field.key == "agent:claude")
        .expect("the form did not draw the second Agent");
    field.value = "on".to_string();

    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetMcpAgentMatrix { matrix, .. } = mutation else {
                panic!("the save was not a matrix write: {mutation:?}");
            };
            let claude = matrix
                .iter()
                .find(|entry| entry.agent_id.as_str() == "claude")
                .expect("the Agent that was switched on is missing from the write");
            assert!(claude.enabled);
            // And the assignment the record already had is left as it was.
            let codex = matrix
                .iter()
                .find(|entry| entry.agent_id.as_str() == "codex")
                .expect("the existing assignment was dropped");
            assert!(codex.enabled);
            assert_eq!(codex.updated_at_ms, 1);
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A registry page says how much of the registry it is, and lets the reader ask
/// for another one.
///
/// Forty rows of a catalogue the runtime is still walking must not read as the
/// whole of it, and the two narrowings the registries offer have to be reachable
/// — a control drawn with no key behind it is a promise the page does not keep.
/// The line belongs to the registry areas only: the installed areas have no
/// catalogue behind them.
#[test]
fn a_registry_page_says_how_much_of_the_registry_it_covers() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("showing 1 of 312 registry entries"),
        "the page did not say how much it covers:\n{screen}"
    );
    assert!(
        screen.contains("still indexing"),
        "the page claimed to be the whole registry:\n{screen}"
    );
    // The bar names the question the page was asked with.
    assert!(screen.contains("Transport"), "{screen}");
    assert!(screen.contains("All"), "{screen}");
    // 312 matches at 20 a page is sixteen pages, and the reader is on the first.
    assert!(screen.contains("1/16"), "{screen}");

    // The installed area has no catalogue behind it, so it has no such line.
    // The marker is the page indicator rather than the word "Transport": that
    // word is also the name of one of this table's own columns.
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("registry entries"), "{screen}");
    assert!(!screen.contains("1/16"), "{screen}");
}

/// `t` narrows the MCP page and `s` ranks the Skill one, each from the first
/// page and by asking the registry again.
#[test]
fn the_registry_narrowings_ask_the_registry_again() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    // MCP is narrowed by transport, and the cycle runs All → Local → Remote.
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(
        app.config_center.registry.transport,
        Some(vibex_tui::config_center::McpServerTransportFilter::Local)
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => assert_eq!(
            registry.transport_filter(),
            Some(vibex_core::McpMarketTransportFilter::Remote)
        ),
        other => panic!("the narrowing did not ask again: {other:?}"),
    }
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(app.config_center.registry.transport, None);

    // The transport key means nothing to the Skill index, which is narrowed by
    // ranking instead.
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(app.config_center.registry.transport, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistrySort);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => assert_eq!(
            registry.sort.wire(),
            vibex_core::SkillMarketSort::Downloads,
            "the ranking did not move"
        ),
        other => panic!("the ranking did not ask again: {other:?}"),
    }
}

/// Moving a page moves the question, and the cursor comes with it.
#[test]
fn the_registry_pages_step_within_what_the_registry_answered() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    app.config_center.set_cursor(3);

    // MCP is paged by the authority, so the next page is a read.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(app.config_center.registry.page, 1);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => {
            assert_eq!(registry.page, 1);
        }
        other => panic!("the page turn did not read: {other:?}"),
    }
    // The cursor belongs to the page it was chosen on.
    assert_eq!(app.config_center.cursor(10), 0);

    // And the first page has nothing before it.
    app.perform(vibex_tui::action::Intent::ConfigRegistryPagePrevious);
    assert_eq!(app.config_center.registry.page, 0);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPagePrevious);
    assert!(outcome.effects.is_empty(), "the page ran off the front");
    assert_eq!(app.config_center.registry.page, 0);
}

/// The Skill index hands one window over, so its pages are cut here.
#[test]
fn the_skill_registry_pages_the_window_it_was_handed() {
    let mut app = loaded(140, 40);
    app.management_data.skill_market = (0..45)
        .map(|index| skill_market_entry(&format!("skill_{index}")))
        .collect();
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(45),
        exhausted: true,
    });
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));

    // Three pages of twenty, twenty and five.
    assert_eq!(app.config_center.registry_pages(&app.management_data), 3);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        20
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert!(
        outcome.effects.is_empty(),
        "a page the client cuts is a redraw, not a read"
    );
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        20
    );
    app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        5
    );
    // The last page is the last one.
    app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(app.config_center.registry.page, 2);
}

/// Installing a registry entry is a form, not a press.
///
/// The registry publishes a launcher and the variables it needs; it never
/// publishes the reader's own token for them, and it has no idea which Agents
/// should receive the server. Both are asked for, the command that will be saved
/// is read back, and what is written is the entry the form showed rather than a
/// second reading of a registry that may have moved on.
#[test]
fn a_registry_entry_is_installed_through_a_form() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent(), second_agent()];
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let outcome = app.run_config_action_by_key("registry_trace", ConfigAction::Install);
    assert!(outcome.effects.is_empty(), "the install ran unasked");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the install form did not open");
    };
    assert_eq!(form.preview.as_deref(), Some("npx -y @example/trace"));
    let token = form
        .field("env:TRACE_TOKEN")
        .expect("the variable the registry asked for was not offered");
    assert!(
        token.secret,
        "a secret requirement was offered in the clear"
    );
    assert_eq!(token.value, "from-registry");
    assert_eq!(
        form.enabled_agents(),
        vec!["codex".to_string(), "claude".to_string()],
        "every added Agent should start switched on"
    );

    // Nothing secret is echoed: the value is masked where it is drawn.
    let screen = text(&mut app, 140, 40);
    assert!(
        !screen.contains("from-registry"),
        "a secret requirement was echoed to the screen:\n{screen}"
    );

    // The reader fills the token in and turns the second Agent off.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("env:TRACE_TOKEN")
            .expect("token field")
            .value = "s3cret".to_string();
        form.field_mut("agent:claude").expect("agent field").value = "off".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::InstallMcpMarketEntry {
                env_values,
                agent_ids,
                candidate,
                ..
            } = mutation
            else {
                panic!("the save was not an install: {mutation:?}");
            };
            assert_eq!(
                env_values
                    .iter()
                    .map(|entry| (entry.name.as_str(), entry.value.as_str()))
                    .collect::<Vec<_>>(),
                vec![("TRACE_TOKEN", "s3cret")]
            );
            assert_eq!(
                agent_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                vec!["codex"],
                "the Agent that was switched off is still in the write"
            );
            assert_eq!(
                candidate.env, *env_values,
                "the launcher and the request disagree about the environment"
            );
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// The command the form is about to save can be taken away.
#[test]
fn a_form_hands_over_the_command_it_is_about_to_save() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    app.run_config_action_by_key("registry_trace", ConfigAction::Install);

    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormCopy);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::Clipboard { text }] => {
            assert_eq!(text, "npx -y @example/trace");
        }
        other => panic!("the copy produced {other:?}"),
    }

    // A form with nothing to read back copies nothing.
    app.overlay = None;
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormCopy);
    assert!(
        outcome.effects.is_empty(),
        "a plain record editor copied something"
    );
}

/// A registry whose index is still widening is asked again on its own clock.
///
/// The runtime walks the registry on its own time and nothing arrives to say it
/// grew, so a page that says "still indexing" and then never asks again is a
/// sentence the client stopped checking. A finished index, and an area that is
/// not a registry, stop the clock.
#[test]
fn a_widening_registry_is_asked_again_without_a_key() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let asked = |app: &mut App| {
        app.tick().effects.iter().any(|effect| {
            matches!(
                effect,
                vibex_tui::app::Effect::SearchRegistry { module, .. }
                    if *module == ConfigModule::Mcp
            )
        })
    };

    // The clock starts on the page that was just read rather than re-reading it.
    assert!(
        !asked(&mut app),
        "the page was read again the moment it opened"
    );
    let mut fired = 0;
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        if asked(&mut app) {
            fired += 1;
        }
    }
    assert_eq!(fired, 2, "the registry was not asked on its own clock");

    // A finished index stops the clock.
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: true,
    });
    app.config_center.registry_poll_ticks = 0;
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        assert!(!asked(&mut app), "a finished index was polled anyway");
    }

    // So does leaving the registry.
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        assert!(!asked(&mut app), "the installed list was polled");
    }
}

/// A profile can be made from nothing, which no other record in the centre can.
///
/// The reader picks the Agent the profile belongs to — the authority mints the
/// id and derives the kind from that Agent — names it, and lists the Models it
/// offers. The Agents offered are the ones the runtime says can hold a provider
/// configuration, so the form cannot ask for something the save would refuse.
#[test]
fn a_provider_profile_can_be_made_from_nothing() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("`n` on the Providers area opened no form");
    };
    assert!(matches!(
        form.purpose,
        vibex_tui::config_center::FormPurpose::NewProviderProfile { source: None }
    ));
    assert!(
        !form.value("agent").unwrap_or_default().is_empty(),
        "the form did not offer an Agent to belong to"
    );
    assert_eq!(form.value("models").as_deref(), Some(""));

    // The reader names it and lists two Models.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("name").unwrap().value = "Work account".to_string();
        form.field_mut("base_url").unwrap().value = "https://api.example/v1".to_string();
        form.field_mut("models").unwrap().value = "gpt-5\ngpt-5-mini".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::CreateProviderProfile(request) = mutation else {
                panic!("the save was not a creation: {mutation:?}");
            };
            assert_eq!(request.display_name, "Work account");
            assert_eq!(request.base_url.as_deref(), Some("https://api.example/v1"));
            assert_eq!(
                request
                    .configured_models
                    .iter()
                    .map(|model| (model.id.as_str(), model.enabled))
                    .collect::<Vec<_>>(),
                vec![("gpt-5", true), ("gpt-5-mini", true)],
                "the listed Models are not what would be written"
            );
        }
        other => panic!("the save produced {other:?}"),
    }

    // A profile with no name is refused before anything is sent.
    app.overlay = None;
    app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("name").unwrap().value.clear();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a nameless profile was written");
}

/// One profile as the authority holds it, which is what an editor needs.
fn provider_profile_detail() -> vibex_core::ProviderProfile {
    serde_json::from_value(serde_json::json!({
        "id": "provider_first",
        "agentId": "codex",
        "kind": "codex",
        "displayName": "First",
        "status": "enabled",
        "accountAlias": "work",
        "baseUrl": "https://api.example/v1",
        "defaultModel": "gpt-5",
        "configuredModels": [{"id": "gpt-5", "enabled": true, "capabilities": {}}],
        "sandboxDefaults": {
            "permissionMode": "default",
            "askOnRisk": true,
            "bypassAllPermissions": false
        },
        "networkDefaults": {"allowNetwork": true},
        "permissionDefaults": {
            "fileRead": "allow",
            "fileWrite": "ask",
            "command": "ask",
            "network": "ask"
        },
        "providerOptions": {
            "schemaVersion": 1,
            "entries": [{"key": "ccSwitchWebsiteUrl", "value": "https://example.test"}]
        },
        "secrets": [],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("profile detail fixture")
}

/// A copy is written from the profile itself, not from the summary of it.
///
/// The list answers with a summary, which carries the name, the account and the
/// Models but neither the endpoint nor the request options. A copy written from
/// that would be a profile that launches nowhere, so the surface waits for the
/// full read: the press asks for it, and the answer is what opens the form.
#[test]
fn a_copy_is_written_from_the_whole_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // The press is a read first: the record it is about has not been read.
    let outcome = app.run_config_action_by_key("provider_first", ConfigAction::Duplicate);
    match outcome.effects.as_slice() {
        [
            vibex_tui::app::Effect::LoadProviderProfile {
                agent_id,
                profile_id,
            },
        ] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(profile_id.as_str(), "provider_first");
        }
        other => panic!("the copy did not read the profile first: {other:?}"),
    }
    assert_eq!(
        app.pending_provider_open.as_ref().map(|(_, open)| *open),
        Some(vibex_tui::app::ProviderOpen::Duplicate)
    );

    // The answer is what opens the form, and it carries what the summary could
    // not: the endpoint, and the request options besides.
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_provider_open(
        &vibex_core::ProviderProfileId::parse("provider_first").unwrap(),
        vibex_tui::app::ProviderOpen::Duplicate,
    );
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the answer opened no form");
    };
    assert_eq!(form.value("name").as_deref(), Some("First copy"));
    assert_eq!(form.value("account_alias").as_deref(), Some("work"));
    assert_eq!(form.value("models").as_deref(), Some("gpt-5"));
    assert_eq!(
        form.value("base_url").as_deref(),
        Some("https://api.example/v1"),
        "the copy could not read the endpoint it is copying"
    );

    // And the save writes them, rather than dropping what the form has no field
    // for.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::CreateProviderProfile(request) = mutation else {
                panic!("the copy was not a creation: {mutation:?}");
            };
            assert_eq!(request.base_url.as_deref(), Some("https://api.example/v1"));
            assert_eq!(
                request
                    .provider_options
                    .as_ref()
                    .map(|options| options.entries.len()),
                Some(1),
                "the request options were dropped by the copy"
            );
            assert!(
                request.secret_references.is_empty(),
                "a copy must not share the credential it was copied from"
            );
        }
        other => panic!("the copy produced {other:?}"),
    }
}

/// Editing a profile needs the same read, for the same reason.
#[test]
fn editing_a_profile_reads_it_before_it_opens() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    // A summary has no endpoint, so the field is empty and the form says so:
    // an empty field leaves what is stored alone.
    assert_eq!(form.value("base_url").as_deref(), Some(""));
    assert!(form.field("base_url").unwrap().hint.is_some());
}

/// The Models a profile offers are edited as a list inside the record.
///
/// A profile declares the Models it offers and an endpoint answers with the ones
/// it serves, and those are two different things: an entry the endpoint lists and
/// the profile does not offer yet is a row the reader can turn on rather than a
/// line in a report. Every press writes through the profile it belongs to, so the
/// list is never a draft that a re-read can contradict.
#[test]
fn a_profiles_models_are_edited_through_the_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_models = vec![
        vibex_core::ProviderConfiguredModel {
            id: "gpt-5".to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: vibex_core::ProviderModelCapabilities::default(),
        },
        vibex_core::ProviderConfiguredModel {
            id: "gpt-5-mini".to_string(),
            display_name: Some("Mini".to_string()),
            enabled: true,
            wire_api: None,
            capabilities: vibex_core::ProviderModelCapabilities::default(),
        },
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::ConfigModels);
    let Some(Overlay::Models { view }) = app.overlay.clone() else {
        panic!("`m` opened no Model list");
    };
    assert_eq!(view.profile_id, "provider_first");

    // The declared Model comes first, and the catalogue entry the profile does
    // not offer is offered as an available row after it.
    let outcome = app.perform(vibex_tui::action::Intent::SelectNext);
    assert!(
        outcome.effects.is_empty(),
        "moving the cursor wrote something"
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelToggle);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the toggle was not a Model write: {mutation:?}");
            };
            assert_eq!(
                models
                    .iter()
                    .map(|model| (model.id.as_str(), model.enabled))
                    .collect::<Vec<_>>(),
                vec![("gpt-5", true), ("gpt-5-mini", true)],
                "turning on a catalogue entry did not declare it"
            );
        }
        other => panic!("the toggle produced {other:?}"),
    }
    // And the editor stays open over the write.
    assert!(matches!(app.overlay, Some(Overlay::Models { .. })));

    // A record carries one write at a time, so the next press waits for this
    // one to land. There is no worker here to answer it, so the test retires it
    // the way an answer would.
    app.config_center.pending.clear();

    // `d` takes a declared Model out.
    app.overlay = Some(Overlay::Models {
        view: vibex_tui::config_center::ModelListView {
            agent_id: "codex".to_string(),
            profile_id: "provider_first".to_string(),
            title: "First".to_string(),
            selected: 0,
            notice: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelRemove);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the removal was not a Model write: {mutation:?}");
            };
            assert!(models.is_empty(), "the Model was not taken out: {models:?}");
        }
        other => panic!("the removal produced {other:?}"),
    }

    // `f` asks the endpoint, and it is the profile that is asked about.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelFetch);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::FetchProviderModels { profile_id }] => {
            assert_eq!(profile_id.as_str(), "provider_first");
        }
        other => panic!("the fetch produced {other:?}"),
    }
}

/// Adding a Model by hand names the profile it belongs to.
#[test]
fn a_model_typed_by_hand_is_added_to_the_profile_it_was_asked_for() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelAdd);

    let Some(Overlay::Prompt { field, target, .. }) = app.overlay.clone() else {
        panic!("`a` opened no prompt");
    };
    assert_eq!(field, vibex_tui::app::PromptField::NewModelId);
    assert_eq!(target.as_deref(), Some("provider_first"));

    if let Some(Overlay::Prompt { value, .. }) = app.overlay.as_mut() {
        *value = "  gpt-6  ".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the add was not a Model write: {mutation:?}");
            };
            assert_eq!(models.len(), 2, "the typed Model was not added: {models:?}");
            assert_eq!(models[1].id, "gpt-6", "the id was not trimmed");
        }
        other => panic!("the add produced {other:?}"),
    }
    // The reader is put back where they were: the list they were editing.
    assert!(matches!(app.overlay, Some(Overlay::Models { .. })));
}

/// One Model's settings are edited on the Model, and written through the profile.
///
/// A Model is not a record: its name, its protocol, whether it reasons and the
/// limits it declares belong to the entry in the profile's list. The thinking
/// levels are read by the authority's own parser, so the form accepts exactly
/// what a thinking-depth control can consume and refuses the rest with a
/// sentence rather than writing a table dispatch would choke on.
#[test]
fn one_models_settings_are_edited_and_written_through_the_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelEdit);

    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("`e` opened no settings form");
    };
    assert_eq!(form.value("wire_api").as_deref(), Some("inherit"));
    assert_eq!(form.value("reasoning").as_deref(), Some("inherit"));

    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("display_name").unwrap().value = "GPT-5".to_string();
        form.field_mut("wire_api").unwrap().value = "anthropic_messages".to_string();
        form.field_mut("reasoning").unwrap().value = "reasoning".to_string();
        form.field_mut("context_tokens").unwrap().value = "200000".to_string();
        form.field_mut("reasoning_efforts").unwrap().value = "low, high, max".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the save was not a Model write: {mutation:?}");
            };
            assert_eq!(models.len(), 1, "the settings changed the number of Models");
            let model = &models[0];
            assert_eq!(model.display_name.as_deref(), Some("GPT-5"));
            assert_eq!(
                model.wire_api,
                Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
            );
            assert_eq!(model.capabilities.reasoning, Some(true));
            assert_eq!(model.capabilities.context_tokens, Some(200_000));
            assert_eq!(model.capabilities.output_tokens, None);
            let levels = model
                .capabilities
                .reasoning_efforts
                .as_ref()
                .expect("the levels were dropped")
                .declared_levels()
                .map(|(level, _)| level.as_str())
                .collect::<Vec<_>>();
            assert_eq!(levels, vec!["low", "high", "max"]);
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A limit that is not a number, and a level that is not a level, are refused
/// with the reason on screen rather than written as something else.
#[test]
fn a_model_setting_the_authority_could_not_read_is_refused() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelEdit);

    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("context_tokens").unwrap().value = "a lot".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad limit was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("whole numbers"), "{screen}");

    // A level the parser cannot read is refused too, and the form stays open.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("context_tokens").unwrap().value.clear();
        form.field_mut("reasoning_efforts").unwrap().value = "quite a lot".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad level was written");
    assert!(matches!(app.overlay, Some(Overlay::Form { .. })));
}

/// A profile is moved a place at a time, and the whole order is what is written.
///
/// The authority lists profiles in its own order and the reader's arrangement is
/// a separate record, so the rows follow that record when this client has read
/// it — and a move writes the list it produced rather than one index, because an
/// order that named only the moved profile would say nothing about the ones it
/// moved past.
#[test]
fn a_profile_is_moved_within_its_agents_order() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    // The reader's arrangement puts the second one first.
    app.management_data.provider_order = vec![
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_second",
            "orderIndex": 0,
            "updatedAtMs": 1
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_first",
            "orderIndex": 1,
            "updatedAtMs": 1
        }))
        .unwrap(),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // The rows follow the arrangement rather than the order the authority
    // happened to list them in.
    let rows = app.management_data.providers.clone();
    let visible = app.config_center.visible(&app.management_data, app.strings);
    assert_eq!(
        visible
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        vec!["provider_second", "provider_first"],
        "the rows did not follow the reader's arrangement (listed {rows:?})"
    );

    // Moving the first one down swaps the two, and the write is the whole order.
    let outcome = app.run_config_action_by_key("provider_second", ConfigAction::MoveDown);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetProviderOrder { agent_id, profiles } = mutation
            else {
                panic!("the move was not an order write: {mutation:?}");
            };
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(
                profiles.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                vec!["provider_first", "provider_second"]
            );
        }
        other => panic!("the move produced {other:?}"),
    }

    // At the end of the list there is nowhere to go.
    app.config_center.pending.clear();
    app.management_data.provider_order = vec![
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_second",
            "orderIndex": 0,
            "updatedAtMs": 1
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_first",
            "orderIndex": 1,
            "updatedAtMs": 1
        }))
        .unwrap(),
    ];
    let outcome = app.run_config_action_by_key("provider_first", ConfigAction::MoveDown);
    assert!(outcome.effects.is_empty(), "the last profile moved down");
}

/// The arrangement is read per Agent, beside the list it arranges.
#[test]
fn reading_the_providers_area_reads_the_arrangement_too() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    let outcome = app.perform(vibex_tui::action::Intent::ReloadManagement);
    let reads = outcome
        .effects
        .iter()
        .filter(|effect| matches!(effect, vibex_tui::app::Effect::LoadProviderOrder { .. }))
        .count();
    assert_eq!(
        reads, 1,
        "the arrangement was read {reads} times for one Agent: {:#?}",
        outcome.effects
    );
}
