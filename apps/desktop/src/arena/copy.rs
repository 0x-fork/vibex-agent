use super::guardian::Guardian;
use crate::locale::text;
fn title() -> &'static str {
    text("AI Souls", "AI 之魂", "AI 之魂")
}
pub(super) fn entry_label(guardian: Guardian) -> String {
    format!("{} · {}", title(), guardian.agent())
}
/// The game name and controls only; each guardian is left to explore.
pub(super) fn battle_hint() -> String {
    let controls = text(
        "WASD move · Hold J / left mouse, release to shoot · K / right mouse recall · Space roll, hold to sprint · P pause · R retry · Esc return",
        "WASD 移动 · J / 左键蓄力，松开射箭 · K / 右键召回 · 空格翻滚，按住冲刺 · P 暂停 · R 重来 · Esc 返回",
        "WASD 移動 · J / 左鍵蓄力，鬆開射箭 · K / 右鍵召回 · 空格翻滾，按住衝刺 · P 暫停 · R 重來 · Esc 返回",
    );
    format!("{} · {}", title(), controls)
}
pub(super) fn exit_label() -> &'static str {
    text("Leave AI Souls", "退出 AI 之魂", "退出 AI 之魂")
}
pub(super) fn battle_label(guardian: Guardian, paused: bool) -> String {
    let status = if paused {
        text("Paused", "已暂停", "已暫停")
    } else {
        text("Battle", "战斗中", "戰鬥中")
    };
    format!("{} · {} · {}", title(), guardian.agent(), status)
}
/// The unlocked companion riding above an Agent's composer.
pub(super) fn companion_label(guardian: Guardian) -> String {
    let action = text("Double-click to challenge", "双击发起挑战", "雙擊發起挑戰");
    format!("{} · {} · {}", title(), guardian.agent(), action)
}
