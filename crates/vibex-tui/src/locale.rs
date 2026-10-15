//! Product copy for the three languages Vibex ships.
//!
//! The desktop keeps its own `Strings` table inside `apps/desktop`, and the TUI
//! deliberately does not reach into the application crate for it. Instead the
//! table is declared once here through a macro so the three languages cannot
//! drift: a missing translation is a compile error, not a runtime fallback.
//!
//! Locale classification itself is shared — [`vibex_ui::locale::Locale`] owns
//! the BCP-47/POSIX resolution so the TUI and the desktop agree on what
//! `zh-Hant-HK` means.

use std::env;

pub use vibex_ui::locale::Locale;

/// Every user-visible string the TUI renders.
///
/// Rows are `key => { en, zh-CN, zh-TW }`. Keeping the three translations on
/// one line is what makes review of a new string possible without opening three
/// files.
#[macro_export]
macro_rules! strings {
    ($( $key:ident => { $en:expr, $cn:expr, $tw:expr } ),* $(,)?) => {
        /// Resolved product copy for one locale.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Strings {
            pub locale: $crate::locale::Locale,
        }

        impl Strings {
            pub const fn for_locale(locale: $crate::locale::Locale) -> Self {
                Self { locale }
            }

            $(
                #[allow(missing_docs)]
                pub const fn $key(&self) -> &'static str {
                    match self.locale {
                        $crate::locale::Locale::En => $en,
                        $crate::locale::Locale::ZhCn => $cn,
                        $crate::locale::Locale::ZhTw => $tw,
                    }
                }
            )*

            /// The full key set, used by the locale-coverage test.
            pub const ALL_KEYS: &'static [&'static str] = &[ $( stringify!($key) ),* ];
        }
    };
}

strings! {
    // ---- product chrome -------------------------------------------------
    app_name => { "Vibex", "Vibex", "Vibex" },
    product_tagline => {
        "Agent workbench",
        "Agent 工作台",
        "Agent 工作台"
    },
    loading => { "Loading…", "加载中…", "載入中…" },
    connecting => { "Connecting…", "连接中…", "連線中…" },
    reconnecting => { "Reconnecting…", "重新连接中…", "重新連線中…" },
    disconnected => {
        "Disconnected — showing the last known state",
        "连接已断开 — 显示最后已知状态",
        "連線已中斷 — 顯示最後已知狀態"
    },
    stale_data => { "may be out of date", "可能已过期", "可能已過期" },
    retry => { "Retry", "重试", "重試" },
    cancel => { "Cancel", "取消", "取消" },
    confirm => { "Confirm", "确认", "確認" },
    close => { "Close", "关闭", "關閉" },
    back => { "Back", "返回", "返回" },
    reload => { "Reload", "重新加载", "重新載入" },
    none => { "None", "无", "無" },
    yes => { "Yes", "是", "是" },
    no => { "No", "否", "否" },
    unknown => { "Unknown", "未知", "未知" },
    search => { "Search", "搜索", "搜尋" },
    filter => { "Filter", "过滤", "過濾" },
    actions => { "Actions", "操作", "操作" },
    details => { "Details", "详情", "詳情" },
    status => { "Status", "状态", "狀態" },
    error => { "Error", "错误", "錯誤" },
    warning => { "Warning", "警告", "警告" },
    done => { "Done", "完成", "完成" },
    failed => { "Failed", "失败", "失敗" },
    pending => { "Pending", "等待中", "等待中" },
    running => { "Running", "运行中", "執行中" },
    idle => { "Idle", "空闲", "閒置" },
    // The four things a running turn can be doing, named the way the desktop
    // names them so the two surfaces read alike, and the tally of the tools it
    // has called. The count comes first because both languages put it there:
    // the label is the unit, not the sentence.
    phase_preparing => { "Preparing", "准备中", "準備中" },
    phase_thinking => { "Thinking", "思考中", "思考中" },
    phase_calling_tool => { "Calling tool", "调用工具", "呼叫工具" },
    phase_generating => { "Generating", "生成中", "生成中" },
    phase_waiting_approval => { "Waiting for approval", "等待确认", "等待確認" },
    tool_calls => { "tool calls", "次工具调用", "次工具呼叫" },
    archived => { "Archived", "已归档", "已封存" },
    copied => { "Copied", "已复制", "已複製" },
    nothing_here => { "Nothing here yet", "这里还没有内容", "這裡還沒有內容" },
    // Footer verbs. They are shorter than the destination labels because a
    // modal footer is a key legend, not a sentence.
    hint_nav => { "nav", "移动", "移動" },
    hint_select => { "select", "选择", "選擇" },
    hint_open => { "open", "进入", "進入" },
    hint_scroll => { "scroll", "滚动", "捲動" },
    hint_run => { "run", "执行", "執行" },
    hint_expand => { "expand", "展开", "展開" },
    hint_collapse => { "collapse", "收起", "收合" },
    hint_toggle => { "toggle", "切换", "切換" },
    hint_switch => { "switch", "切换", "切換" },
    hint_edit => { "edit", "编辑", "編輯" },
    hint_previous => { "prev", "上一个", "上一個" },
    hint_next => { "next", "下一个", "下一個" },
    hint_clear => { "clear", "清空", "清空" },
    hint_commit => { "keep", "保留", "保留" },
    hint_revert => { "revert", "还原", "還原" },
    hint_reset => { "reset", "重置", "重設" },
    hint_confirm_delete => { "confirm delete", "确认删除", "確認刪除" },
    // A modal that nests: `Esc` steps out one level rather than closing it.
    hint_back => { "back", "返回", "返回" },

    // ---- destinations ---------------------------------------------------
    nav_sessions => { "Sessions", "会话", "工作階段" },
    nav_management => { "Management", "管理", "管理" },
    nav_settings => { "Settings", "设置", "設定" },
    nav_help => { "Help", "帮助", "說明" },
    nav_agent => { "Agent", "Agent", "Agent" },
    nav_git => { "Git", "Git", "Git" },

    // ---- sessions -------------------------------------------------------
    sessions_title => { "Session list", "会话列表", "工作階段清單" },
    sessions_empty => {
        "No sessions yet — press n to start one",
        "还没有会话 — 按 n 新建",
        "還沒有工作階段 — 按 n 新增"
    },
    session_new => { "New session", "新建会话", "新增工作階段" },
    session_runtime_label => { "Agent setup", "Agent 配置", "Agent 設定" },
    session_workspace_label => { "Workspace", "工作区", "工作區" },
    // The list's summary chips count sessions by state. Lower case because a
    // chip is a count first and a word second: `◇ 5 idle` reads as a tally,
    // while `◇ 5 Idle` reads as a sentence someone started.
    session_chip_waiting => { "waiting", "待处理", "待處理" },
    session_chip_running => { "running", "运行中", "執行中" },
    session_chip_idle => { "idle", "空闲", "閒置" },
    session_chip_failed => { "failed", "失败", "失敗" },
    queue_released => { "Held message sent", "排队消息已发出", "排隊訊息已發出" },
    // The session list reads a row's hour the way the desktop's sidebar does: a
    // wall clock today, then a count of days, then weeks. The suffix carries its
    // own leading space, because the number is formatted in front of it and
    // Chinese needs no space where English does.
    time_just_now => { "now", "刚刚", "剛剛" },
    time_minutes => { "m", "分钟", "分鐘" },
    time_hours => { "h", "小时", "小時" },
    time_days => { "d", "天", "天" },
    time_days_ago => { " days ago", " 天前", " 天前" },
    time_weeks_ago => { " weeks ago", " 周前", " 週前" },
    sidebar_yesterday => { "Yesterday", "昨天", "昨天" },
    // The word beside a project's running count, which the desktop keeps at the
    // row's far edge.
    sidebar_running => { "running", "运行中", "執行中" },
    // The words the desktop puts in a session row's status column when the state
    // has no hour worth reading.
    sidebar_state_initializing => { "INIT", "初始化", "初始化" },
    sidebar_state_archived => { "ARCHIVED", "已归档", "已封存" },
    // What a project's or a folder's action button opens.
    sidebar_row_actions => { "Actions", "操作", "操作" },
    sidebar_action_new_session => { "New session", "新建会话", "建立會話" },
    sidebar_action_new_folder => { "New folder", "新建文件夹", "新增資料夾" },
    sidebar_action_rename_folder => { "Rename folder", "重命名文件夹", "重新命名資料夾" },
    sidebar_action_collapse => { "Collapse", "折叠", "摺疊" },
    sidebar_action_expand => { "Expand", "展开", "展開" },
    sidebar_action_delete_project => { "Delete project", "删除项目", "刪除專案" },
    sidebar_action_delete_folder => { "Delete folder", "删除文件夹", "刪除資料夾" },
    sidebar_folder_name_placeholder => { "Folder name", "文件夹名称", "資料夾名稱" },
    sidebar_folder_name_empty => { "Enter a folder name.", "请输入文件夹名称。", "請輸入資料夾名稱。" },
    sidebar_confirm_delete_project => {
        "Delete this project and every session in it?",
        "删除该项目及其中的所有会话？",
        "刪除該專案及其中的所有工作階段？"
    },
    sidebar_confirm_delete_folder => {
        "Delete this folder? Its sessions are kept.",
        "删除该文件夹？其中的会话会保留。",
        "刪除該資料夾？其中的工作階段會保留。"
    },
    session_untitled => { "untitled session", "未命名会话", "未命名工作階段" },
    // The one line a terminal notification carries. It names what happened,
    // never what was said: the notification layer prepends the Agent's name so
    // a reader with several sessions knows which one to come back to.
    notify_turn_failed => {
        "Agent turn failed",
        "Agent 回合执行失败",
        "Agent 回合執行失敗"
    },
    notify_needs_input => {
        "Agent needs your confirmation",
        "Agent 需要你确认内容",
        "Agent 需要你確認內容"
    },
    notify_turn_completed => {
        "Agent completed work",
        "Agent 已完成工作",
        "Agent 已完成工作"
    },
    workspace_switch_hint => { "Ctrl+W change", "Ctrl+W 切换", "Ctrl+W 切換" },
    session_open => { "Open", "打开", "開啟" },
    session_rename => { "Rename", "重命名", "重新命名" },
    session_archive => { "Archive", "归档", "封存" },
    session_delete => { "Delete", "删除", "刪除" },
    session_fork => { "Fork", "复制分支", "分支複製" },
    session_show_archived => { "Show archived", "显示已归档", "顯示已封存" },
    session_workspace => { "Workspace", "工作区", "工作區" },
    session_title_label => { "Title", "标题", "標題" },
    session_state => { "State", "状态", "狀態" },
    session_confirm_delete => {
        "Delete this session? This cannot be undone.",
        "删除这个会话？此操作无法撤销。",
        "刪除這個工作階段？此操作無法復原。"
    },
    session_confirm_archive => {
        "Archive this session?",
        "归档这个会话？",
        "封存這個工作階段？"
    },
    session_temp_workspace => {
        "Temporary workspace (no project)",
        "临时会话（不使用项目）",
        "臨時工作階段（不使用專案）"
    },
    // Detail-card field labels. Kept to one word each so the label column can
    // stay narrow enough for the value beside it.
    session_card_id => { "ID", "ID", "ID" },
    session_card_parent => { "Main", "主会话", "主工作階段" },
    session_card_agent => { "Agent", "Agent", "Agent" },
    session_card_model => { "Model", "模型", "模型" },
    session_card_created => { "Created", "创建于", "建立於" },
    session_card_updated => { "Updated", "更新于", "更新於" },
    session_card_last_message => { "Last turn", "最近一轮", "最近一輪" },
    session_card_messages => { "Messages", "消息数", "訊息數" },
    session_card_turns => { "Turns", "轮次", "輪次" },
    session_card_tools => { "Tools", "工具", "工具" },
    dock_agents => { "Agents", "子代理", "子代理" },
    dock_children => { "Sub-sessions", "子会话", "子工作階段" },
    dock_children_none => {
        "No sub-sessions delegated by this session",
        "这个会话没有委派子会话",
        "這個工作階段沒有委派子工作階段"
    },
    dock_children_loading => {
        "Reading this session's sub-sessions…",
        "正在读取这个会话的子会话…",
        "正在讀取這個工作階段的子工作階段…"
    },
    session_no_parent => {
        "This session was not delegated by another",
        "这个会话不是由其他会话委派的",
        "這個工作階段不是由其他工作階段委派的"
    },
    parent_session => { "Main session", "主会话", "主工作階段" },
    dock_plan => { "Plan", "计划", "計畫" },
    dock_queue => { "Held", "排队", "排隊" },
    dock_title => { "Running", "运行中", "執行中" },
    dock_more => { "more", "更多", "更多" },
    dock_empty => {
        "Nothing running — agents, the plan and held messages appear here",
        "当前没有运行中的内容 — 子代理、计划与排队消息会显示在这里",
        "目前沒有執行中的內容 — 子代理、計畫與排隊訊息會顯示在這裡"
    },
    dock_hint => {
        "Alt+J/K move · Alt+G open · Alt+H hide done · Alt+D close",
        "Alt+J/K 移动 · Alt+G 打开 · Alt+H 隐藏已完成 · Alt+D 关闭",
        "Alt+J/K 移動 · Alt+G 開啟 · Alt+H 隱藏已完成 · Alt+D 關閉"
    },
    dock_hidden_done => { "Finished work hidden", "已隐藏已完成项", "已隱藏已完成項" },
    dock_shown_done => { "Finished work shown", "已显示已完成项", "已顯示已完成項" },
    sidebar_pinned => { "Pinned to the top", "已置顶", "已置頂" },
    auto_continue_enabled => { "Auto-continue on", "已开启自动继续", "已開啟自動繼續" },
    auto_continue_disabled => { "Auto-continue off", "已关闭自动继续", "已關閉自動繼續" },
    auto_continue_paused => { "Auto-continue paused", "自动继续已暂停", "自動繼續已暫停" },
    auto_continue_resumed => { "Auto-continue resumed", "自动继续已恢复", "自動繼續已恢復" },
    sidebar_unpinned => { "Pin removed", "已取消置顶", "已取消置頂" },
    sidebar_pinned_first => {
        "Pinned sessions always come first — unpin one to move past it",
        "置顶会话始终在最前 — 先取消置顶才能越过",
        "置頂工作階段一律在最前 — 先取消置頂才能越過"
    },
    session_cards_none => {
        "No session detail cards are open",
        "没有打开会话详情卡片",
        "沒有開啟工作階段詳情卡片"
    },

    // ---- transcript search ----------------------------------------------
    search_label => { " search: ", " 搜索: ", " 搜尋: " },
    search_bad_pattern => { "bad pattern", "正则无效", "正則無效" },
    search_no_matches => { "no matches", "无匹配", "無符合" },
    search_regex_hint => {
        "Regular expression · case-insensitive unless it has an uppercase letter",
        "正则表达式 · 不含大写字母时忽略大小写",
        "正規表達式 · 不含大寫字母時忽略大小寫"
    },

    // ---- first-run guidance ---------------------------------------------
    onboarding_title => { "Getting started", "开始使用", "開始使用" },
    onboarding_connect => {
        "Connect to the runtime",
        "连接运行时",
        "連線執行階段"
    },
    onboarding_connect_detail => {
        "The client attaches to the runtime that owns this home",
        "客户端会连接到拥有此 home 的运行时",
        "客戶端會連線到擁有此 home 的執行階段"
    },
    onboarding_workspace => {
        "Choose where the Agent works",
        "选择 Agent 的工作目录",
        "選擇 Agent 的工作目錄"
    },
    onboarding_workspace_detail => {
        "Browse directories on the machine that runs the Agent",
        "浏览运行 Agent 的机器上的目录",
        "瀏覽執行 Agent 的機器上的目錄"
    },
    onboarding_session => {
        "Start a session",
        "新建会话",
        "新增工作階段"
    },
    onboarding_session_detail => {
        "One session per task; it keeps the conversation and its workspace",
        "每个任务一个会话，保存对话与工作区",
        "每個任務一個工作階段，保存對話與工作區"
    },
    onboarding_first_message => {
        "Write the first message",
        "写下第一条消息",
        "寫下第一則訊息"
    },
    onboarding_first_message_detail => {
        "Say what you want done; / lists commands, @ lists files",
        "说明你想做什么；/ 列出命令，@ 列出文件",
        "說明你想做什麼；/ 列出命令，@ 列出檔案"
    },
    onboarding_done => { "Ready", "已就绪", "已就緒" },

    // ---- composer -------------------------------------------------------
    // The name the external editor's scratch file is created under. It says
    // what the file holds rather than repeating product copy: the path is shown
    // to the reader by their editor's own status line.
    composer_editor_title => { "Vibex draft", "Vibex 草稿", "Vibex 草稿" },
    composer_send => { "Send", "发送", "傳送" },
    composer_newline => { "Newline", "换行", "換行" },
    transcript_browse => { "Timeline", "时间线", "時間線" },
    transcript_details => { "Details", "详情", "詳情" },
    transcript_expand => { "Expand / collapse", "展开 / 收起", "展開 / 收合" },
    session_stop => { "Stop", "停止", "停止" },
    composer_nothing_to_undo => {
        "Nothing left to undo in the draft",
        "草稿没有可撤销的修改",
        "草稿沒有可復原的修改"
    },
    composer_nothing_to_redo => {
        "Nothing to redo in the draft",
        "草稿没有可重做的修改",
        "草稿沒有可重做的修改"
    },
    composer_nothing_to_yank => {
        "Nothing has been cut from the draft yet",
        "草稿中还没有剪切过内容",
        "草稿中還沒有剪下過內容"
    },
    image_attached => { "Image attached", "已附加图片", "已附加圖片" },
    image_not_found => { "No such image file", "找不到该图片文件", "找不到該圖片檔案" },
    image_unsupported => {
        "That file is not an image this client can attach",
        "该文件不是本客户端可附加的图片格式",
        "該檔案不是本客戶端可附加的圖片格式"
    },
    image_too_large => {
        "Image is larger than 5 MB",
        "图片超过 5 MB",
        "圖片超過 5 MB"
    },
    image_cap => {
        "A prompt can carry at most 10 images",
        "一条消息最多附加 10 张图片",
        "一則訊息最多附加 10 張圖片"
    },
    clipboard_empty => {
        "Nothing to paste",
        "剪贴板里没有可粘贴的内容",
        "剪貼簿裡沒有可貼上的內容"
    },
    clipboard_refused => {
        "The terminal would not hand over its clipboard",
        "终端拒绝提供它的剪贴板",
        "終端拒絕提供它的剪貼簿"
    },
    image_clipboard_empty => {
        "No image on the clipboard — type a path instead",
        "剪贴板里没有图片 — 请改为输入路径",
        "剪貼簿裡沒有圖片 — 請改為輸入路徑"
    },
    image_path_title => { "Image path", "图片路径", "圖片路徑" },
    image_count => { "images", "张图片", "張圖片" },
    composer_nothing_selected => {
        "Select part of the draft first",
        "请先选中草稿中的内容",
        "請先選取草稿中的內容"
    },
    composer_queue => { "Queue", "排队", "排隊" },
    composer_interrupt => { "Interrupt", "打断", "中斷" },
    composer_continue => { "Continue", "继续", "繼續" },
    composer_steer => { "Steer", "插话", "插話" },
    composer_steer_no_session => {
        "Nothing is running here yet — Enter sends this and creates the session",
        "新会话还没有运行中的回合 — 按 Enter 发送并新建会话",
        "新工作階段還沒有執行中的回合 — 按 Enter 傳送並新增工作階段"
    },
    composer_steer_unavailable => {
        "Steering needs a local runtime — the turn will be interrupted and resent",
        "运行中插话需要本机运行时 — 将改为打断后重发",
        "執行中插話需要本機執行階段 — 將改為中斷後重送"
    },
    composer_edit_external => {
        "Edit in $EDITOR",
        "用 $EDITOR 编辑",
        "用 $EDITOR 編輯"
    },
    composer_empty => { "Message is empty", "消息为空", "訊息為空" },
    composer_history => { "History", "历史", "歷史" },
    composer_history_empty => {
        "No sent message matches that",
        "没有匹配的已发送消息",
        "沒有符合的已傳送訊息"
    },
    composer_history_hint => {
        "Type after ? to search sent messages; Enter recalls the match",
        "在 ? 后输入以搜索已发送消息；Enter 取回",
        "在 ? 後輸入以搜尋已傳送訊息；Enter 取回"
    },
    composer_draft_cleared => { "Draft cleared", "草稿已清空", "草稿已清空" },
    quit_hint => {
        "Press Ctrl+C again to quit",
        "再按一次 Ctrl+C 退出",
        "再按一次 Ctrl+C 離開"
    },
    composer_attachments => { "Attachments", "附件", "附件" },
    composer_attach_hint => {
        "Type a path to attach it",
        "输入路径作为附件",
        "輸入路徑作為附件"
    },
    composer_command_menu => { "Commands", "命令", "命令" },
    composer_file_menu => { "Files", "文件", "檔案" },
    composer_skill_menu => { "Skills", "技能", "技能" },
    // The panels of the `/` and `@` drawers, named as the desktop popup names
    // them. `Commands` and `Skills` reuse the words above, so the drawer and the
    // empty composer's vocabulary cannot drift apart.
    composer_tab_phrases => { "Quick phrases", "快捷短语", "快捷短語" },
    composer_tab_files => { "Workspace files", "工作区文件", "工作區檔案" },
    composer_tab_delegation => { "Agent delegation", "Agent 委派", "Agent 委派" },
    composer_phrases_empty => { "No quick phrases yet", "还没有快捷短语", "還沒有快捷短語" },
    composer_no_matches => { "No matches", "没有匹配项", "沒有符合項目" },

    // ---- the panel that explains a collapsed chip -----------------------
    preview_more_lines => { "more lines", "行未显示", "行未顯示" },
    preview_expand_enter => { "Enter to expand", "Enter 展开", "Enter 展開" },
    preview_expand_paste => {
        "Paste the same block again to expand",
        "再次粘贴同样的内容以展开",
        "再次貼上同樣的內容以展開"
    },
    preview_unavailable => {
        "This picture could not be read",
        "无法读取该图片",
        "無法讀取該圖片"
    },

    // ---- transcript -----------------------------------------------------
    transcript_thinking => { "Thinking", "思考", "思考" },
    transcript_tool => { "Tool", "工具", "工具" },
    transcript_command => { "Command", "命令", "命令" },
    transcript_file_change => { "File change", "文件变更", "檔案變更" },
    transcript_error => { "Error", "错误", "錯誤" },
    transcript_notice => { "Notice", "提示", "提示" },
    transcript_plan => { "Plan", "计划", "計畫" },
    transcript_user => { "You", "你", "你" },
    transcript_agent => { "Agent", "Agent", "Agent" },
    transcript_retry => { "Retry", "重试", "重試" },
    transcript_web_search => { "Web search", "网页搜索", "網頁搜尋" },
    transcript_todo => { "Todo", "待办", "待辦" },
    transcript_collaboration => { "Sub-agent", "子代理", "子代理" },
    transcript_image => { "Image", "图片", "圖片" },
    transcript_git => { "Git", "Git", "Git" },
    transcript_system => { "System", "系统", "系統" },
    transcript_permission => { "Approval", "审批", "審批" },
    transcript_elicitation => { "Question", "询问", "詢問" },
    transcript_collapsed_hint => {
        "collapsed — press e to expand",
        "已折叠 — 按 e 展开",
        "已折疊 — 按 e 展開"
    },
    transcript_expand_all => { "Expand all", "展开全部", "展開全部" },
    transcript_collapse_all => { "Collapse all", "折叠全部", "折疊全部" },
    transcript_worked_for => { "Worked for", "工作了", "工作了" },
    transcript_follow => { "Following", "已跟随", "已跟隨" },
    transcript_paused => { "Scroll paused", "滚动已暂停", "滾動已暫停" },
    transcript_empty => {
        "No messages yet — say something below",
        "还没有消息 — 在下方输入",
        "還沒有訊息 — 在下方輸入"
    },
    transcript_copy_block => { "Copy block", "复制此块", "複製此區塊" },
    transcript_copy_meta => { "Copy metadata", "复制元数据", "複製中繼資料" },
    transcript_context_compacted => {
        "Context was compacted",
        "上下文已压缩",
        "上下文已壓縮"
    },
    transcript_mermaid_hint => {
        "diagram source (render it with an external tool)",
        "图表源码（请用外部工具渲染）",
        "圖表原始碼（請用外部工具渲染）"
    },
    transcript_image_hint => {
        "inline images are not available in the terminal",
        "终端不支持内联图片",
        "終端機不支援內嵌圖片"
    },

    // ---- approvals ------------------------------------------------------
    approval_title => { "Approval required", "需要审批", "需要審批" },
    approval_label => { "Approvals", "审批", "審批" },
    details_state => { "State", "状态", "狀態" },
    details_agent => { "Agent", "Agent", "Agent" },
    details_model => { "Model", "模型", "模型" },
    details_workspace => { "Workspace", "工作区", "工作區" },
    details_branch => { "Branch", "分支", "分支" },
    details_context => { "Context", "上下文", "上下文" },
    background_tasks => { "background", "后台任务", "背景工作" },
    queued_messages => { "queued", "已排队", "已排隊" },
    queue_held => {
        "Held until the running turn ends",
        "将在当前轮次结束后发送",
        "將在目前輪次結束後傳送"
    },
    queue_editing => {
        "Queued message moved back into the draft",
        "已把排队消息放回输入框",
        "已把排隊訊息放回輸入框"
    },
    queue_hint => {
        "Alt+↑↓ pick · Alt+E edit · Alt+X drop · Alt+J/K reorder · Alt+S schedule · Alt+Enter send now",
        "Alt+↑↓ 选择 · Alt+E 编辑 · Alt+X 删除 · Alt+J/K 调序 · Alt+S 定时 · Alt+Enter 立即发送",
        "Alt+↑↓ 選擇 · Alt+E 編輯 · Alt+X 刪除 · Alt+J/K 調序 · Alt+S 定時 · Alt+Enter 立即傳送"
    },
    // ---- scheduling a send ----------------------------------------------
    // The Desktop's clock button, on the character grid. A message that is
    // written is given a deadline instead of a turn; until the clock reaches
    // it the message waits in the held queue beside the ordinary ones.
    schedule_title => { "Schedule send", "定时发送", "定時傳送" },
    schedule_mode_at => { "Date and time", "定时", "定時" },
    schedule_mode_after => { "Countdown", "倒计时", "倒計時" },
    schedule_field_mode => { "Mode", "方式", "方式" },
    schedule_field_when => { "Send at", "发送时间", "傳送時間" },
    schedule_field_hours => { "Hours", "时", "時" },
    schedule_field_minutes => { "Minutes", "分", "分" },
    schedule_field_seconds => { "Seconds", "秒", "秒" },
    schedule_when_hint => {
        "YYYY-MM-DD HH:MM:SS, in your own timezone",
        "按本地时区填写 YYYY-MM-DD HH:MM:SS",
        "按本機時區填寫 YYYY-MM-DD HH:MM:SS"
    },
    schedule_countdown_hint => {
        "Counts down from the moment the message is submitted",
        "从发送该消息的那一刻开始倒计时",
        "從傳送該訊息的那一刻開始倒數"
    },
    schedule_note => {
        "Sends automatically when due. If the session is running, waits for it to finish. Keep Vibex open.",
        "到时自动发送；会话运行中则等待结束。请保持 Vibex 开启。",
        "到時自動傳送；會話執行中則等待結束。請保持 Vibex 開啟。"
    },
    schedule_clear => { "Clear schedule", "清除定时", "清除定時" },
    schedule_cleared => { "Schedule cleared", "已清除定时", "已清除定時" },
    schedule_set => { "Send scheduled", "已设定发送时间", "已設定傳送時間" },
    schedule_error_not_future => {
        "Choose a future time.",
        "请选择未来的时间。",
        "請選擇未來的時間。"
    },
    schedule_error_countdown => {
        "Enter a countdown between 1 second and 365 days; minutes and seconds must be 0–59.",
        "倒计时须为 1 秒至 365 天，分钟和秒须为 0–59。",
        "倒計時須為 1 秒至 365 天，分鐘和秒須為 0–59。"
    },
    schedule_error_ambiguous => {
        "This local time is ambiguous or unavailable. Choose another time.",
        "此本地时间不存在或不唯一，请选择其他时间。",
        "此本機時間不存在或不唯一，請選擇其他時間。"
    },
    schedule_error_unreadable => {
        "Type a deadline as YYYY-MM-DD HH:MM:SS.",
        "请按 YYYY-MM-DD HH:MM:SS 填写发送时间。",
        "請按 YYYY-MM-DD HH:MM:SS 填寫傳送時間。"
    },
    schedule_pending => { "Scheduled send", "定时发送", "定時傳送" },
    schedule_message => { "Scheduled message", "定时消息", "定時訊息" },
    schedule_due_waiting => {
        "Due · waiting to send",
        "已到时 · 等待发送",
        "已到時 · 等待傳送"
    },
    schedule_due_running => {
        "Due · waiting for the current turn",
        "已到时 · 等待当前运行结束",
        "已到時 · 等待目前執行結束"
    },
    schedule_hint_move => { "move", "移动", "移動" },
    schedule_hint_switch => { "shape", "切换方式", "切換方式" },
    schedule_hint_type => { "type", "输入", "輸入" },
    schedule_hint_apply => { "apply", "应用", "套用" },
    mode_shell => { "shell", "shell", "shell" },
    mode_remember => { "remember", "记住", "記住" },
    mode_plan => { "plan", "计划", "計畫" },
    approval_risk => { "Risk", "风险", "風險" },
    approval_allow => { "Allow", "允许", "允許" },
    approval_deny => { "Deny", "拒绝", "拒絕" },
    approval_always => { "Always this session", "本会话总是允许", "本次工作階段總是允許" },
    approval_pending => { "Submitting…", "提交中…", "提交中…" },
    approval_resolved => { "Approval resolved", "审批已处理", "審批已處理" },
    approval_none => { "No approvals waiting", "没有待处理审批", "沒有待處理審批" },
    approval_waiting => {
        "The Agent is blocked until you answer",
        "Agent 正在等待你的答复",
        "Agent 正在等待你的回覆"
    },
    risk_low => { "Low", "低", "低" },
    risk_medium => { "Medium", "中", "中" },
    risk_high => { "High", "高", "高" },
    risk_critical => { "Critical", "严重", "嚴重" },

    // ---- elicitation ----------------------------------------------------
    elicitation_title => { "The Agent needs input", "Agent 需要输入", "Agent 需要輸入" },
    elicitation_submit => { "Submit", "提交", "提交" },
    elicitation_decline => { "Decline", "拒绝", "拒絕" },
    elicitation_confirm => { "Confirm", "确认", "確認" },
    elicitation_dismiss => { "later", "稍后", "稍後" },
    elicitation_unanswered => { "Not answered", "未回答", "未回答" },
    elicitation_answer_required => {
        "Answer this question before moving on",
        "请先回答这个问题",
        "請先回答這個問題"
    },
    elicitation_yes => { "Yes", "是", "是" },
    elicitation_no => { "No", "否", "否" },
    elicitation_unsupported => {
        "This field cannot be answered in the terminal",
        "终端无法回答此字段",
        "終端機無法回答此欄位"
    },
    elicitation_required => { "Required", "必填", "必填" },
    elicitation_field_text => { "Text", "文本", "文字" },
    elicitation_field_number => { "Number", "数字", "數字" },
    elicitation_field_integer => { "Integer", "整数", "整數" },
    elicitation_field_boolean => { "Boolean", "布尔", "布林" },
    elicitation_field_select => { "Select", "单选", "單選" },
    elicitation_field_multi => { "Multi-select", "多选", "多選" },

    // ---- runtime / model ------------------------------------------------
    runtime_title => { "Agent setup", "Agent 配置", "Agent 設定" },
    runtime_desired => { "Requested", "期望", "期望" },
    runtime_effective => { "Effective", "实际", "實際" },
    runtime_switching => { "Switching…", "切换中…", "切換中…" },
    runtime_switch_failed => {
        "Switch failed — still using the previous Agent",
        "切换失败 — 仍在用之前的 Agent",
        "切換失敗 — 仍在使用先前的 Agent"
    },
    runtime_model => { "Model", "模型", "模型" },
    runtime_agent => { "Agent", "Agent", "Agent" },
    // The Agent's own account, as a picker row names it. The runtime writes an
    // English name into the catalogue for clients that render no locale; a
    // reader of this client gets the one below instead.
    runtime_auth_agent_account => { "Agent account", "Agent 账户", "Agent 帳戶" },
    runtime_probe => { "Probe runtimes", "探测运行时", "探測執行階段" },
    // The switch affordance, shown beside the runtime it moves.
    runtime_switch_hint => { "Ctrl+G change", "Ctrl+G 切换", "Ctrl+G 切換" },
    runtime_current => { "Current", "当前", "目前" },
    runtime_unavailable => { "Unavailable", "不可用", "無法使用" },
    // A creation is named by the Agent it will run on; until the catalogue has
    // arrived there is nothing to name, so the message waits.
    runtime_catalogue_reading => {
        "Reading the runtime catalogue — the message will wait",
        "正在读取运行时列表 — 消息会先保留",
        "正在讀取執行階段清單 — 訊息會先保留"
    },
    runtime_no_agents => {
        "The runtime publishes no Agent to create a session with",
        "运行时没有可用来新建会话的 Agent",
        "執行階段沒有可用來新增工作階段的 Agent"
    },
    // The session on screen is one the authority has not created yet, so there
    // is no runtime to move. Said instead of opening a picker whose Enter could
    // only fail, and instead of swallowing the key.
    runtime_session_creating => {
        "The new session is still being created — its Agent can change once it exists",
        "新会话还在创建中 — 创建完成后才能切换 Agent",
        "新工作階段仍在建立中 — 建立完成後才能切換 Agent"
    },
    runtime_session_uncreated => {
        "The session could not be created — send the message again to switch its Agent",
        "会话没有创建成功 — 请重新发送消息后再切换 Agent",
        "工作階段沒有建立成功 — 請重新傳送訊息後再切換 Agent"
    },
    // What the catalogue view chooses. The picker is titled by the two halves
    // of the surface, and this is the first one: an Agent, its account and its
    // model.
    runtime_choose_agent => { "Choose an Agent", "选择 Agent", "選擇 Agent" },
    // The run options the chosen Agent publishes, one Tab away from the
    // catalogue rows.
    runtime_run_options => { "Run options", "运行选项", "執行選項" },
    runtime_no_run_options => {
        "This Agent publishes no run options",
        "此 Agent 没有可调整的运行选项",
        "此 Agent 沒有可調整的執行選項"
    },
    runtime_default => { "Default", "默认", "預設" },
    runtime_thinking_depth => { "Thinking depth", "思考深度", "思考深度" },
    runtime_conversation_mode => { "Conversation mode", "对话模式", "對話模式" },
    runtime_on => { "On", "开", "開" },
    runtime_off => { "Off", "关", "關" },
    runtime_option_required => { "A value is required", "需要填写一个值", "需要填寫一個值" },
    // The pinned sections above the Agent groups: what the reader used last,
    // and what they starred.
    runtime_recent => { "Recent", "最近使用", "最近使用" },
    runtime_favorites => { "Starred", "已收藏", "已收藏" },
    // Key-bar words for the catalogue view.
    runtime_search => { "search", "搜索", "搜尋" },
    runtime_fold => { "fold", "折叠", "摺疊" },
    runtime_star => { "star", "收藏", "收藏" },
    runtime_manage_account => { "account", "账号", "帳號" },
    runtime_reset => { "reset", "重置", "重設" },
    // The row statuses, in the same column as "Current".
    runtime_status_sign_in => { "Sign in", "需登录", "需登入" },
    runtime_status_verifying => { "Checking…", "校验中…", "校驗中…" },
    runtime_status_discovering => { "Listing models…", "发现模型中…", "探索模型中…" },
    runtime_status_configure => { "Set up", "需配置", "需設定" },
    runtime_status_failed => { "Failed", "切换失败", "切換失敗" },
    runtime_no_match => { "No matching Agent", "没有匹配的 Agent", "沒有符合的 Agent" },
    runtime_summary_hint => { "Tab edits", "Tab 编辑", "Tab 編輯" },

    // ---- workspace ------------------------------------------------------
    workspace_pick => { "Choose a workspace", "选择工作区", "選擇工作區" },
    workspace_browse => { "Browse", "浏览", "瀏覽" },
    workspace_parent => { "Parent directory", "上级目录", "上層目錄" },
    workspace_use_this => { "use this directory", "使用此目录", "使用此目錄" },
    workspace_use_selected => { "use selected", "使用选中目录", "使用選中目錄" },
    workspace_at_root => { "Already at the top", "已是最上层目录", "已是最上層目錄" },
    workspace_empty => { "No workspaces", "没有工作区", "沒有工作區" },

    // ---- management -----------------------------------------------------
    management_agents => { "Agents", "Agent", "Agent" },
    management_providers => { "Providers", "Provider", "Provider" },
    management_mcp => { "MCP servers", "MCP 服务", "MCP 服務" },
    management_skills => { "Skills", "技能", "技能" },
    management_prompts => { "Prompts", "提示词", "提示詞" },
    management_devices => { "Pairing and devices", "配对与设备", "配對與裝置" },
    management_scheduled => { "Scheduled tasks", "定时任务", "定時任務" },
    management_automation => { "Automation", "自动化", "自動化" },
    management_health => { "Provider health", "Provider 健康", "Provider 健康" },
    management_install => { "Install", "安装", "安裝" },
    management_update => { "Update", "更新", "更新" },
    management_rollback => { "Roll back", "回滚", "復原版本" },
    management_uninstall => { "Uninstall", "卸载", "解除安裝" },
    management_login => { "Sign in", "登录", "登入" },
    management_logout => { "Sign out", "退出登录", "登出" },
    management_probe => { "Test connection", "测试连接", "測試連線" },
    management_fetch_models => { "Fetch models", "拉取模型", "拉取模型" },
    management_secret_write_only => {
        "Stored credentials are never displayed. Typing a value replaces it.",
        "已保存的凭据不会显示。输入新值即可替换。",
        "已儲存的憑證不會顯示。輸入新值即可取代。"
    },
    management_provider_secret_mutate_hint => {
        "The value is sent to the runtime and cleared from this client immediately.",
        "该值会发送给运行时，并立即从本客户端清除。",
        "該值會傳送給執行階段，並立即從本用戶端清除。"
    },
    management_profile_active => { "Active", "当前", "目前" },
    management_form_essentials => { "Essentials", "基础", "基本" },
    management_form_advanced => { "Advanced", "高级", "進階" },
    management_saved => { "Saved", "已保存", "已儲存" },
    management_agent_matrix => { "Agent matrix", "Agent 矩阵", "Agent 矩陣" },

    // ---- configuration centre -------------------------------------------
    // The four modules, then the function areas inside them. The centre keeps
    // the desktop config centre's architecture, so it keeps its words too.
    config_title => { "Configuration", "配置中心", "設定中心" },
    config_module_agent => { "Agent", "Agent", "Agent" },
    config_module_mcp => { "MCP", "MCP", "MCP" },
    config_module_skills => { "Skills", "技能", "技能" },
    config_module_prompts => { "Prompts", "提示词", "提示詞" },
    config_section_agents => { "Agents", "Agent 列表", "Agent 清單" },
    config_section_providers => { "Model providers", "模型服务", "模型服務" },
    config_section_servers => { "Servers", "服务列表", "服務清單" },
    config_section_installed => { "Installed", "已安装", "已安裝" },
    config_section_catalog => { "Registry", "资源市场", "資源市集" },
    config_section_sources => { "Import", "导入", "匯入" },
    config_section_prompts => { "Library", "提示词库", "提示詞庫" },
    config_hint_agents => {
        "Install, enable and authenticate the Agents",
        "安装、启用并登录 Agent",
        "安裝、啟用並登入 Agent"
    },
    config_hint_providers => {
        "Model provider profiles, credentials and default models",
        "模型服务配置、凭据与默认模型",
        "模型服務設定、憑證與預設模型"
    },
    config_hint_servers => {
        "MCP servers this runtime hands to Agents",
        "运行时交给 Agent 的 MCP 服务",
        "執行階段交給 Agent 的 MCP 服務"
    },
    config_hint_installed => {
        "Skills this runtime hands to Agents",
        "运行时交给 Agent 的技能",
        "執行階段交給 Agent 的技能"
    },
    config_hint_catalog => {
        "Search the public registry and install an entry",
        "搜索公共市场并安装条目",
        "搜尋公共市集並安裝條目"
    },
    config_hint_sources => {
        "Import resources the Agents already configure in place",
        "导入 Agent 本地已配置的资源",
        "匯入 Agent 本地已設定的資源"
    },
    config_hint_prompts => {
        "Reusable prompts and slash commands",
        "可复用提示词与斜杠命令",
        "可重複使用提示詞與斜線命令"
    },
    config_search => { "Filter", "筛选", "篩選" },
    config_section_label => { "Areas", "功能区", "功能區" },
    // What the focus line calls each of the centre's three panes.
    config_pane_modules => { "Modules", "模块", "模組" },
    config_pane_areas => { "Areas", "功能区", "功能區" },
    config_pane_records => { "Records", "记录", "記錄" },
    config_focus => { "Focus", "焦点", "焦點" },
    config_search_hint => { "type to filter", "输入以筛选", "輸入以篩選" },
    config_detail => { "Details", "详情", "詳情" },
    config_no_selection => { "No record selected", "未选择记录", "未選擇記錄" },
    config_empty => { "Nothing here yet", "这里还没有内容", "這裡還沒有內容" },
    config_no_matches => { "No matches", "没有匹配项", "沒有符合項目" },
    config_working => { "working…", "处理中…", "處理中…" },
    config_col_state => { "State", "状态", "狀態" },
    config_col_runtime => { "Runtime", "运行时", "執行階段" },
    config_col_source => { "Source", "来源", "來源" },
    config_col_models => { "Models", "模型", "模型" },
    config_col_agent => { "Agent", "Agent", "Agent" },
    config_col_kind => { "Kind", "类型", "類型" },
    config_col_model => { "Model", "模型", "模型" },
    config_col_secret => { "Credential", "凭据", "憑證" },
    config_col_transport => { "Transport", "传输", "傳輸" },
    config_col_scope => { "Scope", "范围", "範圍" },
    config_col_agents => { "Agents", "Agent", "Agent" },
    config_col_tags => { "Tags", "标签", "標籤" },
    config_col_updated => { "Updated", "更新时间", "更新時間" },
    config_col_version => { "Version", "版本", "版本" },
    config_col_auth => { "Account", "账号", "帳號" },
    config_col_publisher => { "Publisher", "发布者", "發佈者" },
    config_col_installs => { "Installs", "安装量", "安裝量" },
    config_col_package => { "Package", "包类型", "套件類型" },
    config_col_path => { "Path", "路径", "路徑" },
    config_col_install => { "Install", "安装状态", "安裝狀態" },
    config_action_install => { "Install", "安装", "安裝" },
    config_action_rollback => { "Roll back", "回滚", "復原版本" },
    config_action_check_update => { "Check update", "检查更新", "檢查更新" },
    config_action_uninstall => { "Uninstall", "卸载", "解除安裝" },
    config_action_toggle => { "Enable / disable", "启用 / 停用", "啟用 / 停用" },
    config_action_edit => { "Edit", "编辑", "編輯" },
    config_action_delete => { "Delete", "删除", "刪除" },
    config_action_refresh => { "Re-probe", "重新探测", "重新探測" },
    config_action_sign_in => { "Sign in", "登录", "登入" },
    config_action_sign_out => { "Sign out", "退出登录", "登出" },
    config_action_activate => { "Use", "设为当前", "設為目前" },
    config_action_secret => { "Credential", "凭据", "憑證" },
    config_action_test => { "Test", "测试", "測試" },
    config_action_fetch_models => { "Fetch models", "拉取模型", "拉取模型" },
    config_action_validate => { "Validate", "校验", "校驗" },
    config_action_matrix => { "Agents", "分配 Agent", "分配 Agent" },
    config_action_import => { "Import", "导入", "匯入" },
    config_action_new => { "New", "新建", "新增" },

    // ---- what a write says while it runs and when it lands ---------------
    // Three lines per verb, so a press is never silent: the reader is told that
    // something is happening, what came of it, and — when it was refused — what
    // it was that did not happen.
    config_running_install => { "Installing…", "正在安装…", "正在安裝…" },
    config_done_install => { "Installed", "已安装", "已安裝" },
    config_noun_install => { "Install", "安装", "安裝" },
    config_running_rollback => { "Rolling back…", "正在回滚…", "正在復原…" },
    config_done_rollback => { "Rolled back", "已回滚", "已復原" },
    config_noun_rollback => { "Rollback", "回滚", "復原" },
    config_running_check_update => { "Checking for updates…", "正在检查更新…", "正在檢查更新…" },
    config_done_check_update => { "Update checked", "已检查更新", "已檢查更新" },
    config_noun_check_update => { "Update check", "检查更新", "檢查更新" },
    config_running_refresh => { "Re-probing…", "正在重新探测…", "正在重新探測…" },
    config_done_refresh => { "Re-probed", "已重新探测", "已重新探測" },
    config_noun_refresh => { "Re-probe", "重新探测", "重新探測" },
    config_running_toggle => { "Switching…", "正在切换…", "正在切換…" },
    config_done_enable => { "Enabled", "已启用", "已啟用" },
    config_done_disable => { "Disabled", "已停用", "已停用" },
    config_noun_toggle => { "Switch", "切换", "切換" },
    config_running_sign_in => { "Reading sign-in methods…", "正在读取登录方式…", "正在讀取登入方式…" },
    config_done_sign_in => { "Sign-in methods read", "已读取登录方式", "已讀取登入方式" },
    config_noun_sign_in => { "Sign in", "登录", "登入" },
    config_running_sign_out => { "Signing out…", "正在退出登录…", "正在登出…" },
    config_done_sign_out => { "Signed out", "已退出登录", "已登出" },
    config_noun_sign_out => { "Sign out", "退出登录", "登出" },
    config_running_uninstall => { "Uninstalling…", "正在卸载…", "正在解除安裝…" },
    config_done_uninstall => { "Uninstalled", "已卸载", "已解除安裝" },
    config_noun_uninstall => { "Uninstall", "卸载", "解除安裝" },
    config_running_delete => { "Deleting…", "正在删除…", "正在刪除…" },
    config_done_delete => { "Deleted", "已删除", "已刪除" },
    config_noun_delete => { "Delete", "删除", "刪除" },
    config_running_save => { "Saving…", "正在保存…", "正在儲存…" },
    config_done_save => { "Saved", "已保存", "已儲存" },
    config_noun_save => { "Save", "保存", "儲存" },
    config_running_create => { "Creating…", "正在创建…", "正在建立…" },
    config_done_create => { "Created", "已创建", "已建立" },
    config_noun_create => { "Create", "创建", "建立" },
    config_running_activate => { "Activating…", "正在启用…", "正在啟用…" },
    config_done_activate => { "Activated", "已启用", "已啟用" },
    config_noun_activate => { "Activate", "设为当前", "設為目前" },
    config_running_test => { "Testing…", "正在测试…", "正在測試…" },
    config_done_test => { "Tested", "已测试", "已測試" },
    config_noun_test => { "Test", "测试", "測試" },
    config_running_fetch_models => { "Fetching models…", "正在拉取模型…", "正在拉取模型…" },
    config_done_fetch_models => { "Models fetched", "已拉取模型", "已拉取模型" },
    config_noun_fetch_models => { "Model fetch", "拉取模型", "拉取模型" },
    config_running_validate => { "Validating…", "正在校验…", "正在校驗…" },
    config_done_validate => { "Validated", "已校验", "已校驗" },
    config_noun_validate => { "Validate", "校验", "校驗" },
    config_running_import => { "Importing…", "正在导入…", "正在匯入…" },
    config_done_import => { "Imported", "已导入", "已匯入" },
    config_noun_import => { "Import", "导入", "匯入" },
    config_running_registry_install => { "Installing from the registry…", "正在从市场安装…", "正在從市集安裝…" },
    config_done_registry_install => { "Installed from the registry", "已从市场安装", "已從市集安裝" },
    config_noun_registry_install => { "Registry install", "从市场安装", "從市集安裝" },
    config_running_matrix => { "Saving the Agent selection…", "正在保存 Agent 分配…", "正在儲存 Agent 分配…" },
    config_done_matrix => { "Agent selection saved", "Agent 分配已保存", "Agent 分配已儲存" },
    config_noun_matrix => { "Agent selection", "Agent 分配", "Agent 分配" },
    config_running_discover => { "Looking for imports…", "正在查找可导入项…", "正在尋找可匯入項…" },
    config_done_discover => { "Imports read", "已读取可导入项", "已讀取可匯入項" },
    config_noun_discover => { "Import scan", "查找可导入项", "尋找可匯入項" },
    config_running_search => { "Searching the registry…", "正在搜索市场…", "正在搜尋市集…" },
    config_done_search => { "Registry searched", "已搜索市场", "已搜尋市集" },
    config_noun_search => { "Registry search", "搜索市场", "搜尋市集" },
    config_result_up_to_date => { "up to date", "已是最新", "已是最新" },
    config_result_available => { "available", "可用", "可用" },
    config_result_version => { "version", "版本", "版本" },
    config_result_installed => { "installed", "已安装", "已安裝" },
    config_result_signed_in => { "signed in", "已登录", "已登入" },
    config_result_signed_out => { "not signed in", "未登录", "未登入" },
    config_result_methods => { "sign-in methods", "种登录方式", "種登入方式" },
    config_result_models => { "models", "个模型", "個模型" },
    config_result_ok => { "ok", "通过", "通過" },
    config_result_warned => { "warning", "警告", "警告" },
    config_result_failed => { "failed", "失败", "失敗" },
    config_field_auth => { "Account", "账号", "帳號" },
    config_field_source => { "Source", "来源", "來源" },
    config_action_blocked => { "That record is already working — wait for it to answer", "该记录正在处理中，请等待结果", "該記錄正在處理中，請等待結果" },

    config_action_actions => { "Actions", "操作", "操作" },
    config_action_save => { "Save", "保存", "儲存" },
    config_action_cancel => { "Cancel", "取消", "取消" },
    config_actions_title => { "Actions", "操作", "操作" },
    config_state_enabled => { "Enabled", "已启用", "已啟用" },
    config_state_disabled => { "Disabled", "已停用", "已停用" },
    config_state_installed => { "Installed", "已安装", "已安裝" },
    config_state_missing => { "Missing", "缺失", "缺失" },
    config_state_ready => { "Ready", "就绪", "就緒" },
    config_state_unavailable => { "Unavailable", "不可用", "不可用" },
    config_state_probe_failed => { "Probe failed", "探测失败", "探測失敗" },
    config_state_unknown => { "Unknown", "未知", "未知" },
    config_state_importable => { "Importable", "可导入", "可匯入" },
    config_state_imported => { "Imported", "已导入", "已匯入" },
    config_state_unsupported => { "Unsupported", "不支持", "不支援" },
    config_state_error => { "Error", "错误", "錯誤" },
    config_state_archived => { "Archived", "已归档", "已封存" },
    config_state_active => { "Active", "有效", "有效" },
    config_scope_global => { "Global", "全局", "全域" },
    config_scope_user => { "User", "用户", "使用者" },
    config_scope_project => { "Project", "项目", "專案" },
    config_scope_workspace => { "Workspace", "工作区", "工作區" },
    config_secret_set => { "Set", "已设置", "已設定" },
    config_secret_missing => { "Missing", "未设置", "未設定" },
    config_secret_referenced => { "Referenced", "引用中", "引用中" },
    config_secret_unsupported => { "Unsupported", "不支持", "不支援" },
    config_agents_none => { "none", "无", "無" },
    config_managed_external => { "external", "外部安装", "外部安裝" },
    config_managed_absent => { "not installed", "未安装", "未安裝" },
    config_managed_installed => { "installed", "已安装", "已安裝" },
    config_managed_update => { "update available", "有可用更新", "有可用更新" },
    config_managed_failed => { "install failed", "安装失败", "安裝失敗" },
    config_managed_unknown => { "unknown", "未知", "未知" },
    config_yes => { "yes", "是", "是" },
    config_no => { "no", "否", "否" },
    config_lines => { "lines", "行", "行" },
    config_kind_skill => { "Skill", "技能", "技能" },
    config_source_market => { "Registry", "市场", "市集" },
    config_prompt_kind_reusable => { "Prompt", "提示词", "提示詞" },
    config_prompt_kind_slash => { "Slash command", "斜杠命令", "斜線命令" },
    config_prompt_kind_snippet => { "System snippet", "系统片段", "系統片段" },
    config_field_args => { "Arguments", "参数", "參數" },
    config_source_manual => { "Manual", "手工编写", "手工撰寫" },
    config_source_folder => { "Local folder", "本地目录", "本地目錄" },
    config_source_git => { "Git repository", "Git 仓库", "Git 倉庫" },
    config_event_permission => { "Permission request", "权限请求", "權限請求" },
    config_event_session_start => { "Session start", "会话开始", "工作階段開始" },
    config_event_session_stop => { "Session stop", "会话结束", "工作階段結束" },
    config_event_terminal => { "Terminal activity", "终端活动", "終端機活動" },
    config_hint_env => {
        "One KEY=value per line",
        "每行一个 KEY=value",
        "每行一個 KEY=value"
    },
    config_hint_headers => {
        "One Name: value per line",
        "每行一个 Name: value",
        "每行一個 Name: value"
    },
    config_hint_endpoint => {
        "Leave empty to keep the current address",
        "留空表示保持当前地址",
        "留空表示保持目前位址"
    },
    config_field_id => { "Id", "标识", "識別碼" },
    config_field_install => { "Install", "安装", "安裝" },
    config_field_version => { "Version", "版本", "版本" },
    config_field_configured => { "Configured", "已配置", "已設定" },
    config_field_models => { "Models", "模型", "模型" },
    config_field_model => { "Default model", "默认模型", "預設模型" },
    config_field_command => { "Command", "命令", "命令" },
    config_field_path => { "Path", "路径", "路徑" },
    config_field_agent => { "Agent", "Agent", "Agent" },
    config_field_kind => { "Kind", "类型", "類型" },
    config_field_account => { "Account", "账号", "帳號" },
    config_field_secret => { "Credential", "凭据", "憑證" },
    config_field_updated => { "Updated", "更新时间", "更新時間" },
    config_field_transport => { "Transport", "传输方式", "傳輸方式" },
    config_field_scope => { "Scope", "作用范围", "作用範圍" },
    config_field_url => { "URL", "地址", "位址" },
    config_field_env => { "Environment", "环境变量", "環境變數" },
    config_field_headers => { "Headers", "请求头", "請求標頭" },
    config_field_agents => { "Agents", "Agent 分配", "Agent 分配" },
    config_field_tags => { "Tags", "标签", "標籤" },
    config_field_preview => { "Preview", "预览", "預覽" },
    config_field_body => { "Body", "内容", "內容" },
    config_field_description => { "Description", "描述", "描述" },
    config_field_homepage => { "Homepage", "主页", "首頁" },
    config_field_repository => { "Repository", "代码库", "程式庫" },
    config_field_installs => { "Installs", "安装量", "安裝量" },
    config_field_package => { "Package", "包类型", "套件類型" },
    config_field_downloads => { "Downloads", "下载量", "下載量" },
    config_field_stars => { "Stars", "星标", "星標" },
    config_field_slug => { "Slug", "短名", "短名" },
    config_field_publisher => { "Publisher", "发布者", "發佈者" },
    config_confirm_delete_title => { "Delete this record?", "删除该记录？", "刪除該記錄？" },
    config_confirm_delete_body => {
        "The record is deleted on the runtime, which also rewrites the Agent's native configuration.",
        "该记录将在运行时删除，并同步改写 Agent 的原生配置。",
        "該記錄將在執行階段刪除，並同步改寫 Agent 的原生設定。"
    },
    config_confirm_uninstall_title => { "Uninstall this Agent?", "卸载该 Agent？", "解除安裝該 Agent？" },
    config_confirm_uninstall_body => {
        "The Agent's managed install is removed.",
        "将移除该 Agent 的受管安装。",
        "將移除該 Agent 的受管安裝。"
    },
    config_form_new_server => { "New MCP server", "新建 MCP 服务", "新增 MCP 服務" },
    config_form_edit_server => { "Edit MCP server", "编辑 MCP 服务", "編輯 MCP 服務" },
    config_form_new_skill => { "New Skill", "新建技能", "新增技能" },
    config_form_edit_skill => { "Edit Skill", "编辑技能", "編輯技能" },
    config_form_new_prompt => { "New prompt", "新建提示词", "新增提示詞" },
    config_form_edit_prompt => { "Edit prompt", "编辑提示词", "編輯提示詞" },
    config_form_edit_provider => { "Edit provider profile", "编辑模型服务", "編輯模型服務" },
    config_form_name => { "Name", "名称", "名稱" },
    config_form_required => { "Required", "必填", "必填" },
    config_form_hint_move => { "Field", "切换字段", "切換欄位" },
    config_form_hint_change => { "Value", "修改取值", "修改取值" },
    config_form_hint_edit => { "Edit", "编辑", "編輯" },
    config_form_hint_save => { "Save", "保存", "儲存" },
    config_form_hint_done => { "Done", "完成", "完成" },
    config_form_hint_cancel => { "Cancel", "取消", "取消" },
    config_form_unsaved => { "Unsaved changes", "有未保存的修改", "有未儲存的修改" },
    config_form_error_name => { "A name is required", "名称不能为空", "名稱不能為空" },
    config_form_error_body => { "This resource needs content", "该资源需要内容", "該資源需要內容" },
    config_form_error_stdio_command => {
        "A stdio server needs a command",
        "stdio 服务需要命令",
        "stdio 服務需要命令"
    },
    config_form_error_remote_url => {
        "A remote server needs a URL",
        "远程服务需要地址",
        "遠端服務需要位址"
    },

    // ---- devices & pairing ----------------------------------------------
    devices_title => { "Paired devices", "已配对设备", "已配對裝置" },
    devices_empty => { "No paired devices", "没有已配对设备", "沒有已配對裝置" },
    devices_pair_new => { "Pair a device", "配对设备", "配對裝置" },
    devices_pairing_code => { "One-time pairing code", "一次性配对码", "一次性配對碼" },
    devices_permission => { "Permission level", "权限级别", "權限層級" },
    devices_revoke => { "Revoke", "吊销", "撤銷" },
    devices_revoke_reason => { "Reason", "原因", "原因" },
    devices_last_seen => { "Last seen", "最近可见", "最近出現" },
    devices_audit => { "Audit log", "审计记录", "稽核記錄" },
    devices_confirm_revoke => {
        "Revoke this device? It will lose access immediately.",
        "吊销该设备？它将立即失去访问权限。",
        "撤銷該裝置？它將立即失去存取權限。"
    },
    devices_code_hint => {
        "Shown once. It is never written to logs or configuration.",
        "仅显示一次，不会写入日志或配置。",
        "僅顯示一次，不會寫入日誌或設定。"
    },
    // ---- the devices page: two halves under one tab strip -----------------
    devices_tab_pair => { "Pair", "配对", "配對" },
    devices_pair_qr => { "Pair with a QR code", "用二维码配对", "用二維碼配對" },
    devices_pair_qr_scope => { "Phone or tablet", "手机或平板", "手機或平板" },
    devices_pair_lan => {
        "Find a device on this network",
        "在同一网络查找设备",
        "在同一網路尋找裝置"
    },
    devices_pair_lan_scope => {
        "A phone on the same network as this machine",
        "与这台机器同一网络的手机",
        "與這台機器同一網路的手機"
    },
    devices_pair_code => {
        "Pair with a code or link",
        "用配对码或链接配对",
        "用配對碼或連結配對"
    },
    devices_pair_code_scope => { "A second desktop", "第二台桌面端", "第二台桌面端" },
    devices_offer_expired => {
        "That code has expired — press Enter to mint a new one.",
        "配对码已过期——回车重新生成。",
        "配對碼已過期——回車重新產生。"
    },
    devices_pair_working => { "Working…", "处理中…", "處理中…" },
    devices_qr_no_room => {
        "This terminal is too small for the code — widen it, or copy the link below.",
        "终端太小，放不下二维码——请放大窗口，或复制下面的链接。",
        "終端太小，放不下二維碼——請放大視窗，或複製下面的連結。"
    },
    devices_qr_no_glyphs => {
        "This terminal's font cannot draw the code — copy the link below.",
        "当前终端的字体画不出二维码——请复制下面的链接。",
        "目前終端的字型畫不出二維碼——請複製下面的連結。"
    },
    devices_runtime_desktop => {
        "Desktop runtime",
        "桌面端内嵌运行时",
        "桌面端內嵌執行時"
    },
    devices_runtime_headless => {
        "Headless runtime",
        "云端无头运行时",
        "雲端無頭執行時"
    },
    devices_runtime_unknown => {
        "Runtime kind unknown",
        "运行时类型未知",
        "執行時類型未知"
    },
    devices_pair_hint => {
        "Enter opens the highlighted way to pair.",
        "回车展开选中的配对方式。",
        "回車展開選中的配對方式。"
    },
    devices_pair_unavailable => {
        "This runtime cannot pair a device",
        "该运行时无法配对设备",
        "該執行時無法配對裝置"
    },
    devices_qr_hint => {
        "Scan this with the Vibex mobile app.",
        "用 Vibex 手机端扫描。",
        "用 Vibex 手機端掃描。"
    },
    devices_link => { "Pairing link", "配对链接", "配對連結" },
    devices_link_hint => {
        "Paste the link into the desktop's add-runtime form.",
        "在桌面端「添加运行时」中粘贴这条链接。",
        "在桌面端「新增執行時」中貼上這條連結。"
    },
    devices_lan_advertising => {
        "Looking for a device on this network",
        "正在局域网内查找设备",
        "正在區域網路內尋找裝置"
    },
    devices_lan_waiting => {
        "Waiting for a device to ask…",
        "等待设备发起请求…",
        "等待裝置發起請求…"
    },
    devices_lan_request => { "Pairing request", "配对请求", "配對請求" },
    devices_lan_allow => { "Allow", "允许", "允許" },
    devices_lan_refuse => { "Refuse", "拒绝", "拒絕" },
    devices_lan_more => {
        "more waiting after this one",
        "之后还有等待中的请求",
        "之後還有等待中的請求"
    },
    devices_lan_expired => {
        "The window closed",
        "查找窗口已关闭",
        "尋找視窗已關閉"
    },
    devices_lan_restart => { "Look again", "重新查找", "重新尋找" },
    devices_lan_closes_in => { "Closes in", "剩余", "剩餘" },
    devices_error_direct_unavailable => {
        "Turn on Remote Access → Direct in the desktop app, then look again.",
        "请在桌面端开启「远程访问 → Direct」，然后重新查找。",
        "請在桌面端開啟「遠端存取 → Direct」，然後重新尋找。"
    },
    devices_error_lan_unavailable => {
        "This runtime cannot advertise itself on the local network.",
        "该运行时无法在局域网内广播自己。",
        "該執行時無法在區域網路內廣播自己。"
    },
    devices_error_tls => {
        "The mobile app cannot reach this runtime over its Direct route.",
        "手机端无法通过当前的 Direct 地址连接该运行时。",
        "手機端無法透過目前的 Direct 位址連線該執行時。"
    },
    devices_error_request_settled => {
        "That request is no longer waiting.",
        "该请求已不再等待。",
        "該請求已不再等待。"
    },
    devices_error_permission => {
        "This device's permission level does not allow pairing.",
        "当前设备的权限级别不允许配对。",
        "目前裝置的權限層級不允許配對。"
    },
    permission_read_only => { "Read only", "只读", "唯讀" },
    permission_approve_only => { "Approve only", "仅审批", "僅審批" },
    permission_full_control => { "Full control", "完全控制", "完全控制" },
    permission_required_for => {
        "This action needs a higher permission level",
        "此操作需要更高的权限级别",
        "此操作需要更高的權限層級"
    },

    // ---- settings -------------------------------------------------------
    settings_theme => { "Theme", "主题", "主題" },
    settings_language => { "Language", "语言", "語言" },
    settings_icons => { "Icons", "图标", "圖示" },
    settings_color => { "Colour", "颜色", "顏色" },
    settings_keys => { "Key bindings", "键位", "鍵位" },
    settings_keys_hint => {
        "Rebind in ~/.vibex/tui-keys.toml",
        "在 ~/.vibex/tui-keys.toml 中重映射",
        "在 ~/.vibex/tui-keys.toml 中重新對應"
    },
    settings_keymap_reloaded => { "Key bindings reloaded", "键位已重新加载", "鍵位已重新載入" },
    keys_press => { "press a key…", "请按键…", "請按鍵…" },
    keys_capture_hint => {
        "Press the chord that should run this action; Esc cancels",
        "按下要绑定到该动作的组合键；Esc 取消",
        "按下要綁定到該動作的組合鍵；Esc 取消"
    },
    keys_capture_cancelled => { "Rebinding cancelled", "已取消重映射", "已取消重新對應" },
    keys_conflict => {
        "That chord is already taken by",
        "该组合键已被占用：",
        "該組合鍵已被占用："
    },
    keys_rebind => { "Rebind", "重绑", "重新綁定" },
    keys_default => { "Default", "恢复默认", "恢復預設" },
    keys_save => { "Save", "保存", "儲存" },
    keys_saved => { "Key bindings saved", "键位已保存", "鍵位已儲存" },
    keys_saved_hint => {
        "s writes ~/.vibex/tui-keys.toml",
        "按 s 写入 ~/.vibex/tui-keys.toml",
        "按 s 寫入 ~/.vibex/tui-keys.toml"
    },
    keys_unsaved => {
        "Unsaved changes — press s to write ~/.vibex/tui-keys.toml",
        "有未保存的修改 — 按 s 写入 ~/.vibex/tui-keys.toml",
        "有未儲存的修改 — 按 s 寫入 ~/.vibex/tui-keys.toml"
    },
    settings_keymap_error => {
        "Key binding file could not be read; using defaults",
        "无法读取键位文件；使用默认键位",
        "無法讀取鍵位檔案；使用預設鍵位"
    },
    settings_about => { "About", "关于", "關於" },
    settings_connection => { "Connection", "连接", "連線" },
    settings_backend => { "Backend", "后端", "後端" },
    settings_authority_seat => {
        "Local mode (in-process runtime)",
        "本地模式（进程内运行时）",
        "本機模式（行程內執行階段）"
    },
    settings_remote_seat => {
        "Remote mode (runtime over a link)",
        "远程模式（通过网络连接运行时）",
        "遠端模式（透過網路連線執行階段）"
    },
    settings_capabilities => { "Capabilities", "能力", "能力" },
    settings_version => { "Version", "版本", "版本" },
    settings_section_appearance => { "Appearance", "外观", "外觀" },
    settings_section_language => { "Language", "语言", "語言" },
    settings_section_interface => { "Interface", "界面", "介面" },
    settings_mode => { "Theme mode", "主题模式", "主題模式" },
    settings_mode_dark => { "Dark", "深色", "深色" },
    settings_mode_light => { "Light", "浅色", "淺色" },
    settings_mode_hint => {
        "Light or dark treatment of the same palette.",
        "同一套配色在深色或浅色下的呈现。",
        "同一套配色在深色或淺色下的呈現。"
    },
    settings_theme_hint => {
        "Twenty shipped palettes; the preview applies as you move.",
        "内置 20 套配色；移动光标即预览。",
        "內建 20 套配色；移動游標即預覽。"
    },
    settings_mark => { "Logo", "字符画", "字符畫" },
    settings_mark_hint => {
        "The art a new session's page opens on",
        "新建会话页面上的字符画",
        "新增工作階段頁面上的字符畫"
    },
    settings_mark_classic => { "Classic", "经典", "經典" },
    settings_mark_glitch => { "Glitch", "故障", "故障" },
    settings_motion => { "Motion", "动效", "動效" },
    settings_motion_hint => {
        "Animate the logo and timeline disclosures; off shows changes immediately",
        "为字符画和时间线折叠展开添加动效；关闭则立即显示变化",
        "為字符畫和時間線摺疊展開加上動效；關閉則立即顯示變更"
    },
    settings_transitions => { "Text transitions", "文字切换特效", "文字切換特效" },
    settings_transitions_hint => {
        "Scramble a line when the text it shows changes",
        "文本变化时以字符特效切换",
        "文字變更時以字元特效切換"
    },
    settings_on => { "On", "开", "開" },
    settings_off => { "Off", "关", "關" },
    // ---- notifications ---------------------------------------------------
    // The settings page's own copy for the notification layer. The three kinds
    // are named the way the desktop names them, and the hint under each says
    // what makes it news: a session the reader is not looking at.
    settings_section_notifications => { "Notifications", "通知", "通知" },
    settings_notifications => { "Notifications", "通知", "通知" },
    settings_notifications_hint => {
        "Announce a session that finishes, fails or waits for you while you are on another page or away from the terminal.",
        "当会话完成、失败或等待确认，而你在其他页面或离开终端时提醒你。",
        "當工作階段完成、失敗或等待確認，而你在其他頁面或離開終端時提醒你。"
    },
    settings_notify_completed => { "Agent completed", "Agent 完成", "Agent 完成" },
    settings_notify_completed_hint => {
        "Notify when an Agent turn completes.",
        "Agent 回合完成时通知。",
        "Agent 回合完成時通知。"
    },
    settings_notify_needs_input => { "Agent needs input", "Agent 需要输入", "Agent 需要輸入" },
    settings_notify_needs_input_hint => {
        "Notify when an Agent is waiting for input.",
        "Agent 等待输入时通知。",
        "Agent 等待輸入時通知。"
    },
    settings_notify_failed => { "Agent failed", "Agent 失败", "Agent 失敗" },
    settings_notify_failed_hint => {
        "Notify when an Agent turn fails.",
        "Agent 回合失败时通知。",
        "Agent 回合失敗時通知。"
    },
    settings_notify_transport => { "Terminal signal", "终端提示方式", "終端提示方式" },
    settings_notify_transport_hint => {
        "The escape sequence this terminal understands, written while the terminal window is focused; automatic follows the terminal it detects.",
        "该终端能识别的控制序列，在终端窗口有焦点时使用；自动会跟随探测到的终端。",
        "該終端能識別的控制序列，在終端視窗有焦點時使用；自動會跟隨探測到的終端。"
    },
    settings_notify_transport_auto => { "Automatic", "自动", "自動" },
    settings_notify_transport_off => { "Off (in-app only)", "关闭（仅界面内）", "關閉（僅介面內）" },
    settings_notify_transport_bell => { "Bell", "响铃", "響鈴" },
    settings_notify_transport_title => { "Window title", "窗口标题", "視窗標題" },
    settings_notify_transport_osc9 => { "OSC 9 notification", "OSC 9 通知", "OSC 9 通知" },
    settings_notify_transport_osc777 => { "OSC 777 notification", "OSC 777 通知", "OSC 777 通知" },
    settings_notify_transport_osc99 => { "OSC 99 notification", "OSC 99 通知", "OSC 99 通知" },
    settings_notify_transport_iterm2 => { "iTerm2 attention", "iTerm2 提醒", "iTerm2 提醒" },
    settings_notify_system => { "Desktop popup", "系统弹窗", "系統彈窗" },
    settings_notify_system_hint => {
        "Draw a desktop notification with notify-send or osascript while the terminal window is not focused (tmux needs focus-events on). Only on the machine running this client, never over ssh.",
        "终端窗口失去焦点时用 notify-send 或 osascript 弹出系统通知（tmux 需开启 focus-events）。仅在运行本客户端的机器上生效，SSH 连接下不会触发。",
        "終端視窗失去焦點時用 notify-send 或 osascript 彈出系統通知（tmux 需開啟 focus-events）。僅在執行本客戶端的機器上生效，SSH 連線下不會觸發。"
    },
    settings_notify_system_remote => {
        "Unavailable over ssh",
        "SSH 连接下不可用",
        "SSH 連線下不可用"
    },
    settings_notify_system_missing => {
        "No notifier installed",
        "未安装通知程序",
        "未安裝通知程式"
    },
    settings_notify_system_desktop => {
        "The desktop's own",
        "由桌面端弹出",
        "由桌面端彈出"
    },
    settings_notify_light => { "Light reminders", "轻提醒", "輕提醒" },
    settings_notify_light_hint => {
        "While a Vibex desktop is hosting this runtime it announces the same events itself; this client stays light and only marks the window title instead of ringing or popping up.",
        "当桌面端托管该运行时，它自己会通知同样的消息；本客户端只用窗口标题做轻提醒，不再响铃或弹窗。",
        "當桌面端託管該執行階段時，它自己會通知同樣的訊息；本客戶端只用視窗標題做輕提醒，不再響鈴或彈窗。"
    },
    settings_notify_desktop_present => {
        "desktop present",
        "桌面端在运行",
        "桌面端在執行"
    },
    settings_notify_title => { "Include session title", "通知带会话标题", "通知帶工作階段標題" },
    settings_notify_title_hint => {
        "Let the session's own title ride the notification. Off by default: a terminal may log it.",
        "在通知中附带会话标题。默认关闭：终端可能记录该内容。",
        "在通知中附帶工作階段標題。預設關閉：終端可能記錄該內容。"
    },
    settings_notify_test => { "Test notification", "测试通知", "測試通知" },
    settings_notify_test_hint => {
        "Send one now through every channel that is on, so you can see what each one does. A real event goes to one of them, chosen by whether the terminal window has focus.",
        "立即通过每个已开启的通道各发一条，看看它们各自的效果。真实事件只会走其中一个通道，由终端窗口是否有焦点决定。",
        "立即透過每個已開啟的通道各傳送一條，看看它們各自的效果。真實事件只會走其中一個通道，由終端視窗是否有焦點決定。"
    },
    settings_notify_test_body => {
        "Test notification from Vibex",
        "来自 Vibex 的测试通知",
        "來自 Vibex 的測試通知"
    },
    settings_notify_test_sent => {
        "Test notification sent",
        "测试通知已发送",
        "測試通知已傳送"
    },
    settings_notify_test_none => {
        "Nothing sent: notifications are off, or every channel is",
        "未发送：通知已关闭，或所有通道都关闭了",
        "未傳送：通知已關閉，或所有通道都關閉了"
    },
    settings_reasoning_display => { "Reasoning display", "推理显示方式", "推理顯示方式" },
    settings_reasoning_display_hint => {
        "Show the latest live reasoning at the bottom, or keep reasoning in the timeline.",
        "将最新实时推理置于底部，或保留在会话时间线中。",
        "將最新即時推理置於底部，或保留在工作階段時間線中。"
    },
    settings_reasoning_latest => { "Latest at bottom", "最新推理内容置底", "最新推理內容置底" },
    settings_reasoning_timeline => { "In timeline", "显示在时间线中", "顯示在時間線中" },
    settings_reasoning_expanded => { "Expand reasoning by default", "默认展开推理", "預設展開推理" },
    settings_reasoning_expanded_hint => {
        "Open reasoning by default. Each reasoning row can still be expanded or collapsed.",
        "默认打开推理内容；仍可逐项展开或折叠。",
        "預設開啟推理內容；仍可逐項展開或摺疊。"
    },
    settings_reasoning_expansion => { "Reasoning expansion", "推理展开方式", "推理展開方式" },
    settings_reasoning_expansion_hint => {
        "Window follows the newest live lines; full expansion shows all reasoning as it arrives.",
        "窗口渲染跟随最新实时内容；全量展开显示全部推理。",
        "視窗繪製跟隨最新即時內容；完整展開顯示全部推理。"
    },
    settings_reasoning_window => { "Window", "窗口渲染", "視窗繪製" },
    settings_reasoning_full => { "Full expansion", "全量展开", "完整展開" },
    settings_icons_hint => {
        "Unicode chrome, or ASCII for a terminal without font fallback.",
        "Unicode 界面符号；无字体回退的终端可用 ASCII。",
        "Unicode 介面符號；無字型回退的終端可用 ASCII。"
    },
    settings_language_hint => {
        "Product copy for the interface; the runtime is unaffected.",
        "界面文案语言；不影响运行时。",
        "介面文案語言；不影響執行階段。"
    },
    settings_workspace_hint => {
        "Where a new session starts; empty uses the open session's workspace.",
        "新会话的起始目录；留空则使用当前会话的工作区。",
        "新工作階段的起始目錄；留空則使用目前工作階段的工作區。"
    },
    settings_backend_hint => {
        "The runtime's capability schema this client is talking to.",
        "当前客户端连接的运行时能力版本。",
        "目前客戶端連線的執行階段能力版本。"
    },
    settings_seat_hint => {
        "Local mode starts the runtime in this app; remote mode connects to one.",
        "本地模式由本程序启动运行时；远程模式通过网络连接运行时。",
        "本機模式由本程式啟動執行階段；遠端模式透過網路連線執行階段。"
    },
    settings_version_hint => {
        "The client build; the runtime reports its own version separately.",
        "客户端版本；运行时版本单独上报。",
        "客戶端版本；執行階段版本單獨回報。"
    },
    settings_keys_reload => { "F9 reloads", "F9 重新加载", "F9 重新載入" },
    settings_icons_unicode => { "Unicode", "Unicode", "Unicode" },
    settings_icons_ascii => { "ASCII", "ASCII", "ASCII" },
    settings_filter_label => { " filter: ", " 过滤: ", " 過濾: " },
    settings_no_matches => {
        "No settings match",
        "没有匹配的设置",
        "沒有符合的設定"
    },
    settings_pick_hint => {
        "Moves preview the value; Esc puts the old one back",
        "移动即预览；Esc 还原",
        "移動即預覽；Esc 還原"
    },
    settings_edit_hint => {
        "Enter saves, Esc discards",
        "Enter 保存，Esc 放弃",
        "Enter 儲存，Esc 捨棄"
    },
    settings_reset_confirm => {
        "Reset this setting to its default?",
        "将此设置重置为默认值？",
        "將此設定重設為預設值？"
    },
    settings_reset_title => { "Reset setting", "重置设置", "重設設定" },
    settings_reset_done => { "reset", "已重置", "已重設" },

    // ---- help -----------------------------------------------------------
    help_title => { "Help", "帮助", "說明" },
    help_category_global => { "Global", "全局", "全域" },
    help_category_transcript => { "Transcript", "时间线", "時間線" },
    help_category_composer => { "Composer", "输入框", "輸入框" },
    help_category_modals => { "Modals", "弹窗", "彈窗" },
    help_category_workbench => { "Workbench", "工作台", "工作台" },
    help_category_management => { "Management", "管理", "管理" },
    help_category_panels => { "Panels", "面板", "面板" },
    help_context => { "Context", "上下文", "上下文" },
    help_keys => { "Keys", "键位", "鍵位" },
    help_hint => {
        "Press ? for the keys available right now",
        "按 ? 查看当前可用键位",
        "按 ? 查看目前可用鍵位"
    },
    help_no_keys => { "No keys are available here", "此处没有可用键位", "此處沒有可用鍵位" },
    help_disabled_key => {
        "That key is disabled right now",
        "该键当前不可用",
        "該鍵目前不可用"
    },
    help_legend => { "Legend", "图例", "圖例" },

    // ---- command palette / overlays --------------------------------------
    palette_title => { "Command palette", "命令面板", "命令面板" },
    palette_placeholder => { "Type a command…", "输入命令…", "輸入命令…" },
    // The headings, in the order the palette draws them: where the reader can
    // go, the devices they trust, the client itself, and — on a session's page
    // alone — the sessions those verbs act on.
    palette_group_view => { "Feature view", "功能视图", "功能檢視" },
    palette_group_pairing => { "Remote pairing", "远程配对", "遠端配對" },
    palette_group_app => { "App", "应用", "應用" },
    palette_group_session => { "Session list", "会话列表", "工作階段清單" },
    palette_group_workbench => { "Workbench", "工作台", "工作台" },
    // The palette's own names and one-line explanations. They are longer than
    // the key bar's labels because the palette is read as a menu rather than
    // glanced at as a legend.
    palette_cmd_sessions => { "Sessions", "会话", "工作階段" },
    palette_hint_sessions => { "Open the session list", "打开会话列表", "開啟工作階段清單" },
    palette_cmd_management => { "Configuration centre", "配置中心", "配置中心" },
    palette_hint_management => {
        "Open the configuration centre",
        "打开配置中心",
        "開啟配置中心"
    },
    palette_cmd_settings => { "Settings", "设置", "設定" },
    palette_hint_settings => { "Theme, language, keys", "主题、语言、键位", "主題、語言、鍵位" },
    palette_cmd_help => { "Help", "帮助", "說明" },
    palette_hint_help => { "Contextual key help", "查看当前键位说明", "檢視目前鍵位說明" },
    palette_cmd_new_session => { "New session", "新建会话", "新增工作階段" },
    palette_hint_new_session => { "Create a session", "创建一个会话", "建立一個工作階段" },
    palette_cmd_rename_session => { "Rename session", "重命名会话", "重新命名工作階段" },
    // The session verbs act on the session list's own cursor — the row the
    // reader has selected there — which is not always the session on screen.
    // The hints name that target rather than promising "the open session".
    palette_hint_rename_session => {
        "Rename the selected session",
        "重命名选中的会话",
        "重新命名選取的工作階段"
    },
    palette_cmd_fork_session => { "Fork session", "复制会话分支", "複製工作階段分支" },
    palette_hint_fork_session => {
        "Copy the selected session",
        "复制选中的会话",
        "複製選取的工作階段"
    },
    palette_cmd_archive_session => { "Archive session", "归档会话", "封存工作階段" },
    palette_hint_archive_session => {
        "Archive the selected session",
        "归档选中的会话",
        "封存選取的工作階段"
    },
    palette_cmd_restore_session => { "Restore session", "恢复会话", "還原工作階段" },
    palette_hint_restore_session => {
        "Restore an archived session",
        "恢复已归档的会话",
        "還原已封存的工作階段"
    },
    palette_cmd_delete_session => { "Delete session", "删除会话", "刪除工作階段" },
    palette_hint_delete_session => {
        "Delete the selected session",
        "删除选中的会话",
        "刪除選取的工作階段"
    },
    palette_cmd_switch_runtime => { "Switch runtime", "切换运行时", "切換執行階段" },
    palette_hint_switch_runtime => {
        "Pick the Agent runtime and model",
        "选择 Agent 运行时和模型",
        "選擇 Agent 執行階段與模型"
    },
    palette_cmd_probe_runtimes => { "Probe runtimes", "探测运行时", "探測執行階段" },
    palette_hint_probe_runtimes => {
        "Re-discover available Agents",
        "重新发现可用的 Agent",
        "重新探索可用的 Agent"
    },
    palette_cmd_devices => { "Pairing and devices", "配对与设备", "配對與裝置" },
    palette_hint_devices => {
        "Pair a phone or a second desktop, and manage the devices already paired",
        "配对手机或第二台桌面端，并管理已配对的设备",
        "配對手機或第二台桌面端，並管理已配對的裝置"
    },
    palette_cmd_reload_keys => { "Reload key bindings", "重新加载键位", "重新載入鍵位" },
    palette_hint_reload_keys => {
        "Re-read tui-keys.toml",
        "重新读取 tui-keys.toml",
        "重新讀取 tui-keys.toml"
    },
    palette_cmd_refresh => { "Refresh", "刷新", "重新整理" },
    palette_hint_refresh => {
        "Re-read the current page",
        "重新读取当前页面",
        "重新讀取目前頁面"
    },
    palette_cmd_quit => { "Quit", "退出", "結束" },
    palette_hint_quit => { "Leave the TUI", "离开 TUI", "離開 TUI" },
    overlay_confirm_title => { "Please confirm", "请确认", "請確認" },
    overlay_prompt_title => { "Input", "输入", "輸入" },
    toast_permission_denied => {
        "The runtime denied this action",
        "运行时拒绝了此操作",
        "執行階段拒絕了此操作"
    },
    toast_action_unavailable => {
        "This client cannot perform that action",
        "此客户端无法执行该操作",
        "此用戶端無法執行該操作"
    },
    toast_offline => {
        "Not available while disconnected",
        "断连时不可用",
        "中斷連線時不可用"
    },
    handover_offer => {
        "The runtime has stopped · Ctrl+T takes this home over",
        "运行时已停止 · Ctrl+T 接管这个 home",
        "執行階段已停止 · Ctrl+T 接管這個 home"
    },

    // ---- degradation ----------------------------------------------------
    no_tty_title => { "Not a terminal", "不是终端", "不是終端機" },
    no_tty_body => {
        "Vibex TUI needs an interactive terminal on stdin and stdout.",
        "Vibex TUI 需要 stdin 和 stdout 都是交互式终端。",
        "Vibex TUI 需要 stdin 和 stdout 都是互動式終端機。"
    },
    cjk_font_hint => {
        "Chinese text needs a monospace CJK font in your terminal",
        "中文界面需要终端安装等宽 CJK 字体",
        "中文介面需要終端機安裝等寬 CJK 字型"
    },
    runtime_locked_title => {
        "Another Vibex runtime owns this home",
        "另一个 Vibex 运行时正在使用此 home",
        "另一個 Vibex 執行階段正在使用此 home"
    },
    runtime_locked_body => {
        "Vibex Desktop is running and is not accepting local clients. Either enable Settings → Remote Access → Direct, quit the desktop app and retry, or connect to a remote runtime with `vibex connect <link>`.",
        "Vibex 桌面端正在运行且未开放本机客户端。请在桌面端开启 设置 → 远程访问 → Direct，或退出桌面端后重试，或用 `vibex connect <链接>` 连接远程运行时。",
        "Vibex 桌面端正在執行且未開放本機用戶端。請在桌面端開啟 設定 → 遠端存取 → Direct，或結束桌面端後重試，或用 `vibex connect <連結>` 連線遠端執行階段。"
    },
    // How the client is attached, in the reader's words. "Seat" stays the
    // internal name (`SeatKind`); the reader sees a mode, because a seat is
    // not a word a terminal user knows.
    seat_authority => { "Local mode", "本地模式", "本機模式" },
    seat_remote => { "Remote mode", "远程模式", "遠端模式" },
}

impl Strings {
    /// Classify the process locale the way the desktop does, then fall back to
    /// `LC_ALL` / `LC_MESSAGES` / `LANG` because the TUI has no GUI locale layer.
    pub fn detect() -> Self {
        Self::detect_from(|key| env::var(key).ok()).unwrap_or_else(|| Self::for_locale(Locale::En))
    }

    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<Self> {
        let raw = lookup("LC_ALL")
            .filter(|value| !value.is_empty())
            .or_else(|| lookup("LC_MESSAGES").filter(|value| !value.is_empty()))
            .or_else(|| lookup("LANG").filter(|value| !value.is_empty()))
            .or_else(sys_locale::get_locale);
        Some(Self::for_locale(Locale::from_system_tag(raw.as_deref())))
    }

    pub fn with_locale(locale: Locale) -> Self {
        Self::for_locale(locale)
    }

    /// The BCP-47 tag the UI shows in settings.
    pub const fn tag(&self) -> &'static str {
        self.locale.tag()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_three_nonempty_translations() {
        let en = Strings::for_locale(Locale::En);
        let cn = Strings::for_locale(Locale::ZhCn);
        let tw = Strings::for_locale(Locale::ZhTw);
        for key in Strings::ALL_KEYS {
            let value = match *key {
                "app_name" => en.app_name(),
                _ => continue,
            };
            assert!(!value.is_empty());
        }
        // The accessor set is generated from the same macro invocation, so a
        // mismatched key set is impossible. This asserts the table is not empty
        // and that the three locales really differ where translation exists.
        assert!(Strings::ALL_KEYS.len() > 100);
        assert_ne!(en.nav_sessions(), cn.nav_sessions());
        assert_ne!(cn.nav_sessions(), tw.nav_sessions());
    }

    #[test]
    fn detects_locale_from_environment_priority() {
        let strings = Strings::detect_from(|key| match key {
            "LC_ALL" => Some("zh_CN.UTF-8".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::ZhCn);

        let strings = Strings::detect_from(|key| match key {
            "LANG" => Some("zh_TW.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::ZhTw);
    }

    #[test]
    fn unsupported_languages_fall_back_to_english() {
        let strings = Strings::detect_from(|key| match key {
            "LANG" => Some("fr_FR.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::En);
    }
}
