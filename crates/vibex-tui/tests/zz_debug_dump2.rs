//! Temporary debug dump: positional header/cell alignment for every section.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_tui::app::{App, AppOptions, ManagementData};
use vibex_tui::config_center::{ConfigModule as M, ConfigSection as S};
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::{Locale, SeatKind};

fn build(locale: Locale, width: u16, height: u16) -> App {
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
            locale,
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.resize(width, height);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(vibex_tui::action::Intent::GotoManagement);
    app.management_data = management();
    app
}

macro_rules! fixture {
    ($fun:ident, $ty:ty, $json:expr) => {
        fn $fun() -> $ty {
            serde_json::from_value($json).expect("fixture")
        }
    };
}

fixture!(
    agent,
    vibex_core::AgentSnapshotEntry,
    serde_json::json!({
        "id": "codex", "label": "Codex", "description": "An agent",
        "runtimeKind": "acp", "sourceKind": "builtin", "added": true, "enabled": true,
        "orderIndex": 0, "installed": true, "configured": true,
        "installStatus": "installed",
        "managedInstall": {"managed": true, "status": "installed", "distributionKind": "npm", "installedVersion": "1.2.3"},
        "configStatus": "configured", "runtimeStatus": "ready", "models": ["gpt-5"], "modes": [],
        "capabilityHints": [], "env": {}, "params": {}, "diagnostics": [], "nativeConfigPaths": []
    })
);
fixture!(
    mcp_server,
    vibex_core::McpServer,
    serde_json::json!({
        "id": "mcp_filesystem", "displayName": "Filesystem", "transportKind": "stdio",
        "status": "enabled", "scopeKind": "user", "command": "npx", "args": ["-y"],
        "env": [], "headers": [], "secretReferences": [], "providerMatrix": [],
        "description": "Files", "tags": ["files"],
        "agentMatrix": [{"agentId": "codex", "enabled": true, "sourceKind": "manual", "updatedAtMs": 1}],
        "createdAtMs": 1, "updatedAtMs": 1
    })
);
fixture!(
    prompt,
    vibex_core::Prompt,
    serde_json::json!({
        "id": "prompt_review", "displayName": "Review", "kind": "reusable_prompt",
        "status": "enabled", "scopeKind": "user", "body": "Review this workspace.",
        "tags": ["review"], "createdAtMs": 1, "updatedAtMs": 1
    })
);
fixture!(
    skill,
    vibex_core::Skill,
    serde_json::json!({
        "id": "skill_one", "displayName": "One", "status": "enabled",
        "sourceKind": "local_folder", "scopeKind": "user", "description": "A skill",
        "tags": ["a"], "providerMatrix": [], "agentMatrix": [],
        "createdAtMs": 1, "updatedAtMs": 1
    })
);
fixture!(
    profile,
    vibex_core::ProviderProfileSummary,
    serde_json::json!({
        "id": "provider_one", "agentId": "codex", "displayName": "One",
        "kind": "codex", "status": "enabled", "secretSetupState": "available",
        "defaultModel": "gpt-5", "configuredModels": [], "createdAtMs": 1, "updatedAtMs": 1
    })
);
fixture!(
    mcp_market,
    vibex_core::McpMarketEntry,
    serde_json::json!({
        "id": "mcp_market_one", "name": "Market One", "transport": "stdio",
        "version": "1.0.0", "author": "dev", "packageKind": "npm", "description": "d",
        "command": "npx", "homepage": "https://x", "repository": "https://r",
        "env": [], "diagnostics": [], "status": "active", "updatedAtMs": 1, "createdAtMs": 1
    })
);
fixture!(
    skill_market,
    vibex_core::SkillMarketEntry,
    serde_json::json!({
        "id": "skill_market_one", "slug": "one", "name": "Market One", "summary": "s",
        "version": "1.0.0", "ownerHandle": "dev", "installs": 3, "stars": 1, "downloads": 9, "diagnostics": []
    })
);
fixture!(
    mcp_discovery,
    vibex_core::McpServerDiscovery,
    serde_json::json!({
        "discoveryId": "d1", "importKey": "k1", "sourceAgentId": "codex",
        "sourcePath": "/tmp/x", "status": "importable", "diagnostics": [], "existingMcpServerId": null, "candidate": null
    })
);
fixture!(
    skill_discovery,
    vibex_core::SkillDiscovery,
    serde_json::json!({
        "discoveryId": "d2", "importKey": "k2", "displayName": "D", "sourceAgentId": "codex",
        "sourcePath": "/tmp/y", "status": "importable", "diagnostics": [], "existingSkillId": null, "commandName": "c", "contentPreview": "p"
    })
);

fn management() -> ManagementData {
    ManagementData {
        agents: vec![agent()],
        mcp: vec![mcp_server()],
        prompts: vec![prompt()],
        skills: vec![skill()],
        providers: vec![profile()],
        profiles: vec![profile()],
        mcp_market: vec![mcp_market()],
        skill_market: vec![skill_market()],
        mcp_discoveries: vec![mcp_discovery()],
        skill_discoveries: vec![skill_discovery()],
        ..Default::default()
    }
}

fn find_from(buffer: &ratatui::buffer::Buffer, row: u16, needle: &str, from: u16) -> Option<u16> {
    let symbols = (0..buffer.area.width)
        .map(|c| {
            buffer
                .cell((c, row))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let wanted = needle.chars().map(|c| c.to_string()).collect::<Vec<_>>();
    (usize::from(from)..symbols.len())
        .find(|start| {
            wanted
                .iter()
                .enumerate()
                .all(|(off, want)| symbols.get(start + off) == Some(want))
        })
        .map(|start| start as u16)
}

fn dump_rows(app: &mut App, width: u16, height: u16) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| vibex_tui::view::render(f, app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    for y in 0..9u16 {
        let s: String = (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        println!("DUMP {y:>2}|{s}|");
    }
}

fn check(locale: Locale, width: u16, module: M, section: S) {
    let mut app = build(locale, width, 40);
    app.show_config_module(module, Some(section));
    let columns = vibex_tui::config_center::columns(module, section, app.strings);
    if (locale == Locale::ZhCn || locale == Locale::En) && (width == 100 || width == 112) && module == M::Agent && section == S::Agents {
        dump_rows(&mut app, width, 40);
    }
    let rows = app.config_center.visible(&app.management_data, app.strings);
    let Some(first) = rows.first() else {
        println!("{locale:?} w{width} {module:?}/{section:?}: NO ROW");
        return;
    };
    let backend = TestBackend::new(width, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| vibex_tui::view::render(f, &mut app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let rowtext = |y: u16| -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect()
    };
    let heading_row = (0..buffer.area.height).find(|y| {
        find_from(&buffer, *y, columns[0], 0).is_some()
            && find_from(&buffer, *y, "●", 0).is_none()
            && find_from(&buffer, *y, "○", 0).is_none()
    });
    let record_row = (0..buffer.area.height).find(|y| {
        find_from(&buffer, *y, "●", 0).is_some() || find_from(&buffer, *y, "○", 0).is_some()
    });
    let (Some(h), Some(r)) = (heading_row, record_row) else {
        println!(
            "{locale:?} w{width} {module:?}/{section:?}: no heading/record ({heading_row:?}/{record_row:?})"
        );
        return;
    };
    let mut previous = 0u16;
    let mut problems = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        let Some(cell) = first.cells.get(index) else {
            problems.push(format!("{column}: no cell"));
            continue;
        };
        let Some(heading_at) = find_from(&buffer, h, column, 0) else {
            problems.push(format!(
                "{column}: heading not found in row {:?}",
                rowtext(h)
            ));
            continue;
        };
        if heading_at < previous {
            problems.push(format!("{column}: out of order"));
        }
        previous = heading_at;
        let cell_at = find_from(&buffer, r, cell, heading_at);
        if cell_at != Some(heading_at) {
            problems.push(format!(
                "{column}: heading@{heading_at} cell@{cell_at:?} ({cell:?})"
            ));
        }
    }
    if !problems.is_empty() {
        println!(
            "MISALIGNED {locale:?} w{width} {module:?}/{section:?}: {}",
            problems.join("; ")
        );
    }
}

#[test]
fn dump() {
    for locale in [Locale::En, Locale::ZhCn, Locale::ZhTw] {
        for width in [48u16, 60, 72, 80, 90, 100, 112, 120, 130, 140, 160, 200] {
            for (m, s) in [
                (M::Agent, S::Agents),
                (M::Agent, S::Providers),
                (M::Mcp, S::Servers),
                (M::Mcp, S::Catalog),
                (M::Mcp, S::Sources),
                (M::Skills, S::Installed),
                (M::Skills, S::Catalog),
                (M::Skills, S::Sources),
                (M::Prompts, S::Prompts),
            ] {
                check(locale, width, m, s);
            }
        }
    }
    println!("done");
}
