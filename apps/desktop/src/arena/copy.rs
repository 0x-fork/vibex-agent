use super::guardian::Guardian;
use crate::locale::text;
fn title() -> &'static str {
    text("AI Souls", "AI 之魂", "AI 之魂")
}
pub(super) fn entry_label(guardian: Guardian) -> String {
    format!("{} · {}", title(), guardian.agent())
}
pub(super) fn entry_help(guardian: Guardian) -> String {
    let controls = text(
        "WASD / arrows move · Hold J / left mouse, release to shoot · K / right mouse recall · Space roll, hold to sprint · P pause · Esc return",
        "WASD / 方向键移动 · J / 鼠标左键蓄力，松开射箭 · K / 右键召回 · 空格翻滚，按住冲刺 · P 暂停 · Esc 返回",
        "WASD / 方向鍵移動 · J / 滑鼠左鍵蓄力，鬆開射箭 · K / 右鍵召回 · 空格翻滾，按住衝刺 · P 暫停 · Esc 返回",
    );
    let hint = match guardian {
        Guardian::Claude => text(
            "Every hit splits the spark. Keep striking the one that carries the heart; after four splits the heart lies bare.",
            "每次命中都会让火花分裂。紧盯带着心脏的那一个，分裂四次后心脏裸露，一击制胜。",
            "每次命中都會讓火花分裂。緊盯帶著心臟的那一個，分裂四次後心臟裸露，一擊制勝。",
        ),
        Guardian::Codex => text(
            "Lure its leap onto a forge plate. While the brazier burns, shoot through the flame to melt the knot, then hit the cube between shield pulses.",
            "引它跳压到熔炉踏板上。火盆燃起时让箭穿过火焰，用火箭熔开绳结，再趁护盾间隙射中方核。",
            "引它跳壓到熔爐踏板上。火盆燃起時讓箭穿過火焰，用火箭熔開繩結，再趁護盾間隙射中方核。",
        ),
        Guardian::Pi => text(
            "One stem strikes, the other guards the eye. Cross its midline to force a switch, and shoot the bare eye. The south gate is out of reach.",
            "一根立柱砸击，另一根守护眼睛。穿过它的中线逼它换手，趁眼睛裸露射击。南门边是它够不着的地方。",
            "一根立柱砸擊，另一根守護眼睛。穿過它的中線逼它換手，趁眼睛裸露射擊。南門邊是它搆不著的地方。",
        ),
        Guardian::OpenCode => text(
            "Keep moving when it vanishes. When it gapes at you and fires blocks, sidestep and shoot into the open mouth from the front.",
            "它消失时保持移动。当它朝你张开大口喷射代码块时，侧移躲开并从正面射入口中。",
            "它消失時保持移動。當它朝你張開大口噴射程式碼塊時，側移躲開並從正面射入口中。",
        ),
        Guardian::DeepSeek => text(
            "Lure each leap into open water, then shoot the raised tail from a floe. Before the discharge, get onto ice.",
            "把每次跃扑引到开阔水面，再站上浮冰射击翘起的尾巴。放电前务必回到冰面上。",
            "把每次躍撲引到開闊水面，再站上浮冰射擊翹起的尾巴。放電前務必回到冰面上。",
        ),
        Guardian::Copilot => text(
            "Orbs knock you back; a wall behind you is fatal. When its spin dash hits a wall it is stunned: dodge the falling ice and shoot the rear jet.",
            "能量球会把你击退，背后是墙就会撞死。旋转突进撞墙后它会眩晕：躲开落下的冰锥，射击背后的喷口。",
            "能量球會把你擊退，背後是牆就會撞死。旋轉突進撞牆後它會暈眩：躲開落下的冰錐，射擊背後的噴口。",
        ),
    };
    format!("{} · {}\n{}", entry_label(guardian), hint, controls)
}
pub(super) fn battle_label(guardian: Guardian, paused: bool) -> String {
    let status = if paused {
        text("Paused", "已暂停", "已暫停")
    } else {
        text("Battle", "战斗中", "戰鬥中")
    };
    format!("{} · {} · {}", title(), guardian.agent(), status)
}
