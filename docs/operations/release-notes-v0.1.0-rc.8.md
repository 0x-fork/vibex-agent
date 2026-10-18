# Vibex v0.1.0-rc.8 Release Notes

- Released: 2026-10-18 · Range: `v0.1.0-rc.7...v0.1.0-rc.8` · 723 commits

---

## English

### 🚀 Highlights

- **Vibex TUI** — The whole workbench now fits in a terminal: sessions, streaming Markdown, approvals, the queue, settings and pairing. It ships inside every desktop package, works over SSH, and either starts its own runtime or attaches to the one on your desk.
- **Agent teams** — A delegation is now a task with a lifecycle: keep it open, follow up in the same child session, accept it or cancel it, and watch the ownership tree and the team status header as the work moves.
- **Scheduled messages** — Pick the hour for a message and it waits in the queue: across restarts, on every client, and editable by whichever client is holding it.
- **Custom workbench backgrounds** — Put a picture, still or animated, behind the desktop workbench, with fill, fit or stretch and a veil that keeps text readable over any image.
- **Agent computer use** — With your permission, an Agent can take over the host's desktop. It is installed and managed from Settings, behind a master switch you can turn off.
- **A rebuilt mobile client** — The phone now follows the desktop's model: session groups and the ownership tree, a right rail with the file tree and diffs, scheduled sends, worktrees, goals, haptics, and a rebuilt pairing and first-run flow.
- **Automatic updates** — Desktop updates are on by default, and iOS publishes an unsigned device IPA that installers re-sign with their own Apple ID.

### 🛠 New features

**Vibex TUI**

- The layout is one stack of full-width bands — status, transcript, turn status, composer, shortcuts — that reshapes itself for any terminal size, so there is no minimum size and no "window too small" notice.
- The transcript renders Markdown as it streams, folds finished turns behind a `▸ Worked for 12s` divider that is itself the control, pins a scrolled-away prompt above the viewport, and offers `/` smart-case regex search.
- The composer takes readline-style editing keys and collapses long pastes into `[Pasted: 42 lines]` chips; images attach as chips from the clipboard, a paste, or a dropped file.
- Agent requests arrive in place: approvals are cards over the page (`a`, `d`, `Ctrl+A`), questions a form of tabs in the composer band, and auto-continue keeps the desktop's five-second countdown (`t` stops it).
- `/`, `@` and `$` open the desktop's own completion panels — commands and Prompts, workspace files and the Agent delegation list, Skills.
- A command palette, an editable key cheatsheet, and notifications delivered through terminal escapes with the channel chosen per runtime, so a desktop that already owns the session is answered first.
- The session list mirrors the desktop's arrangement, and pins, renames, forks, archives and moves are written back to it; without one, your pins and order live in `~/.vibex/tui-sidebar.json`.
- Management is a desktop-style configuration centre — Agents, Providers, Models, MCP servers, Skills, Prompts, pairing and remote access — and Settings hold 20 themes, `en`/`zh-CN`/`zh-TW`, the icon set, the landing logo, motion, reasoning display and key bindings.
- Copying goes over OSC 52, so it works over SSH and inside tmux, and the client ships in every desktop package — `/usr/bin/vibex`, beside the macOS `.app`, `vibex.exe` — plus `docker exec -it <container> vibex-server tui` in containers.

**Agent teams**

- A delegated task can stay open after its round, so you can send a follow-up into the same child session, accept it or cancel it; the old single-turn path is unchanged.
- A retry resolves to the original acceptance instead of overwriting earlier work, and a changed intent is refused.
- Cancelling a task is permanent: it blocks new messages and new child work anywhere under the task it owns.
- A delegated session receives exactly the context the caller declared — no more, no less.
- Delegation shows up in the workbench: an ownership tree in the sidebar, a team status header on session groups, a delegation card that locates its pane, and `@` references that say whether they name a file or an Agent.

**Desktop**

- A user image behind the workbench, imported like a project icon and stored under the runtime home: still or animated, filled, fitted or stretched, with a veil.
- Scheduled sends: pick the hour, keep the schedule across restarts, edit it in a popover, and see a send another client is holding.
- Computer use is installed and managed from Settings, with a floating shell, a localized panel, and the driver's console kept off your desktop.
- Per-Agent built-in tool panels are toggled from the Agent list, with enabled tools lit in green.
- Markdown previews can be searched after rendering, and the timeline reads bounded windows, pages older history in without moving your place, and marks the viewport on a turn preview rail.
- A temporary session from the project menu, new-session actions on folder rows, and a regrouped settings navigation; failures and right-rail results now arrive as light top hints you can dismiss or copy.
- AI Souls grew pixel guardians, roaming boss encounters, a quiet battle overlay, and best clear times per guardian; the duel clock is bigger and now sits on its own bevelled plate, so the time stays readable on every sanctuary floor.

**Mobile**

- The client is rebuilt around the desktop's model: the sidebar's rows, groups and tabs match, and a session the desktop renamed or deleted is reflected on the phone.
- The right rail browses the desktop's folders with a nested tree, paged diffs, search and copyable paths, and the file viewer opens a picture and lands on the line.
- The composer carries its own runtime drawers — the queue with a picker for a waiting send's hour, drafts that survive a restart, recallable sent messages, and a trigger list that grows with both prompt fields.
- Session groups can be created and kept on the phone, delegated sessions hang off the desktop's tree, and base branches, temporary sessions, worktrees you can finish and a folder's archive window are all there.
- Goals can be rewritten, paused, resumed or cleared from the phone, and pairing and runtime switching became one rebuilt first screen with saved runtimes and their reachability.
- Notifications default to on and upgrade existing installs, gestures gained haptic feedback, and the type ladder is one scaling rem base with a smaller step.
- Signed Android updates install from the release manifest, an unsigned iOS device IPA ships beside the simulator bundle, and terminal tabs, the Return key, touch drag for selection and for moving the app, and mouse events forwarded to applications now behave, with find results that survive a grid resize.

**Agents, providers and remote**

- Reviewed Agent versions moved forward: DeepSeek Harness `0.4.37`, Codex on `codex-acp` `2.0.1`, Kimi on the Kimi Code CLI successor, and zcode `0.54.0`; an unfunded account now reads as unfunded instead of failing the model list.
- Provider profiles read their whole configuration back for editing, fetch models from an unsaved draft, take request-header overrides, and import from and export into the settings other CLI tools already hold.
- Browser-based Agent sign-in is no longer hidden: Cursor, Grok, Gemini, Cline, CodeBuddy and Antigravity advertise no logout, so their OAuth methods were treated as unusable — they are now recognized and offered.
- Agent-generated images appear in the session, terminal output streams as it is produced, and CodeBuddy can be steered mid-turn.
- A desktop can serve clients on the same machine — which is what lets a TUI attach locally — pairing opens on a LAN without a published route, and a paired client can restore or forget a device.
- Remote reports where each managed worktree stands, what a scheduled send will send, a folder's archive window, a session's suspension, and the runtime's own name.
- The Git surface separates pulling from the refresh fetch, and a pull puts the fetched commits into the checked-out branch.

### ⚡️ Performance

- The TUI only repaints what is in front of the reader, so idle surfaces cost nothing.
- The desktop timeline redraws far less: expanded groups are windowed, finished turns are kept, live labels stop repainting, and Markdown file previews are virtualized.
- The sidebar skips session queries while headings are toggled, rewinds a settled running cue instead of replaying spinner frames, and resolves a multi-selection once per frame.
- A child Agent's timeline no longer folds on every frame, a streamed tool call folds into one row, and oversized session-list responses are avoided.

### 🔧 Under the hood

- Versions bumped to `0.1.0-rc.8` across the workspace, the desktop packager inputs, the Android versionName fallback, the README, the deployment docs and the reviewed asset bundles.
- The license SBOM and third-party notices were regenerated for the current graph: 1085 Cargo packages, 9 asset and font bundles, and 5 native and installer inputs, all approved.
- Four workspace members arrived — `apps/tui` and `crates/vibex-tui` for the terminal client, `crates/computer` for computer use, `crates/vibex-brands` for shared brand marks — `crates/vibex-markdown` folded into the TUI, and the desktop now renders Markdown through `gpui-component`.
- The workspace moved to `gpui-kit` `0.7.3` and `gpui-pre` `0.3.9`, and the core protocol gained a `before` cursor so history can be paged backwards.
- macOS and Windows packaging remains build and package evidence, and PDFium distribution approval still covers linux-x86_64 only.

---

## 中文

### 🚀 核心亮点

- **Vibex TUI（终端版）正式发布** — 整个工作台现在都能装进终端：会话、流式 Markdown、审批、队列、设置与配对。它随每个桌面安装包一起发布，支持 SSH，既能自己启动运行时，也能附着到桌面端正在运行的那一个。
- **Agent 团队化协作** — 委派变成有生命周期的任务：可以保持打开、在同一个子会话里追加跟进、接受或取消，侧栏的所有权树和会话组的团队状态头会同步显示进展。
- **消息定时发送** — 给消息选一个整点，它就待在队列里：跨重启保留，每个客户端都能看到，持有它的客户端还能随时改。
- **自定义工作台背景** — 在工作台后面放一张图（静态或动图），填充、适应或拉伸，再加一层遮罩，文字在任何背景下都清晰。
- **Agent 电脑操控（Computer Use）** — 在你授权之后，Agent 可以接管宿主机的桌面；在设置里安装和管理，总开关随时能关。
- **移动端重建，对齐桌面版** — 手机端现在跟随桌面端的模型：会话组与所有权树、带文件树和差异的右侧栏、定时发送、worktree、目标、触感反馈，以及重建的配对与首次运行流程。
- **自动更新** — 桌面端默认开启自动更新；iOS 发布未签名的真机 IPA，安装者用自己的 Apple ID 重新签名即可。

### 🛠 新功能

**Vibex TUI（终端版）**

- 界面上是一列全宽横带——状态、对话流、回合状态、输入框、快捷键栏——跟着终端窗口自动重排；没有最小尺寸，也不会再弹"窗口太小"。
- 对话流把 Markdown 边流边渲染，结束的回合折到 `▸ Worked for 12s` 分隔线后面（这条线本身就是开关），滚走的用户消息钉在视口上方，`/` 支持智能大小写正则搜索。
- 输入框支持 readline 式快捷键，长文本粘贴会收成 `[Pasted: 42 lines]` 芯片；图片可以从剪贴板、粘贴或拖进来的文件变成芯片附着。
- Agent 的请求就地呈现：审批是页面上的卡片（`a` 允许、`d` 拒绝、`Ctrl+A` 本会话一直允许），提问占据输入框横带；自动续跑保留桌面端的五秒倒计时（`t` 停止）。
- `/`、`@`、`$` 打开的就是桌面端自己的补全面板：命令与 Prompt、工作区文件与 Agent 委派列表、技能。
- 命令面板、可编辑的快捷键速查表，以及通过终端转义送达的通知——通道按运行时选择，拥有该会话的桌面端会被优先应答。
- 会话列表与桌面端保持一致，置顶、重命名、分叉、归档和移动都会写回去；读不到排布时，你自己的置顶与顺序保存在 `~/.vibex/tui-sidebar.json`。
- 管理页是桌面风格的配置中心——Agent、Provider、Model、MCP、技能、Prompt、配对与远程访问；设置里有 20 套主题、`en`/`zh-CN`/`zh-TW`、图标集、着陆 logo、动效、推理显示和键位绑定。
- 复制走 OSC 52，所以在 SSH 和 tmux 里都能用；客户端随每个桌面安装包发布（`/usr/bin/vibex`、macOS `.app` 旁、`vibex.exe`），容器里则用 `docker exec -it <container> vibex-server tui`。

**Agent 团队**

- 委派的任务在回合结束后可以保持打开，向同一个子会话追加跟进、接受或取消；旧的单回合路径不受影响。
- 重试会落到最初那次接受，不会覆盖早先的工作；意图变了则直接拒绝。
- 取消是永久的：一旦被接受，它名下任何位置的新消息和新子工作都会被阻断。
- 被委派的会话拿到的上下文正好是调用方声明的那些，不多也不少。
- 这些在界面上都能看到：侧栏的所有权树、会话组的团队状态头、能定位到窗格的委派卡片，以及会说明自己指的是文件还是 Agent 的 `@` 引用。

**桌面端**

- 工作台后面的用户图片：静态或动图、填充／适应／拉伸、可调遮罩，按项目图标同样的方式导入，存放在运行时 home 下。
- 定时发送：选定整点、跨重启保留、在弹出面板里编辑，还能看到另一个客户端正在持有的发送。
- 电脑操作（computer use）在设置里安装和管理，带浮动外壳、本地化面板，驱动的控制台窗口不会跑到你的桌面上。
- 每个 Agent 的内置工具面板可以在 Agent 列表里开关，启用的工具图标会亮成绿色。
- 渲染后的 Markdown 预览可以直接查找；对话流只读取有界窗口，向上翻页时把更早的历史前置而不移动你的位置，回合预览轨会标出视口覆盖的范围。
- 项目菜单提供临时会话，文件夹行新增新建会话操作，设置导航重新分组；失败信息和右侧栏结果统一变成顶部轻提示，可以关掉或复制。
- 会话首页的 AI Souls 多了像素守卫、各自巡游的 Boss、安静的战斗遮罩，以及每位守卫的最佳通关时间；决斗计时更大，并落在自带的斜面像素底座上，任何场地都读得清。

**移动端**

- 客户端围绕桌面端的模型重建：侧栏的行、分组与标签和桌面端一致，桌面端改名或删除的会话会同步到手机上。
- 右侧栏浏览桌面端的文件夹，带嵌套树、分页差异、搜索和可复制路径；文件查看器能打开图片并定位到行。
- 输入框自带运行时抽屉：队列（带等待发送改整点的选择器）、跨重启保留的草稿、可召回的已发消息，以及随两个输入区一起增高的触发列表。
- 会话组可以在手机上创建和保留，委派会话挂在桌面端的树形导轨上，基础分支、临时会话、可以收尾的 worktree 和文件夹归档窗口也都在。
- 目标可以在手机上改写、暂停、恢复或清除；配对与运行时切换合并成一个重建的首屏，列出已保存的运行时及其可达性。
- 通知默认开启，既有安装会升级到开启；手势增加触感反馈；字号梯度统一到一个 rem 基准，并多了一档更小的。
- Android 从发布清单安装签名更新，iOS 在模拟器包之外附带未签名真机 IPA；终端标签与 Return 键、触摸拖动（既能选文本也能拖应用）、转发给应用的鼠标事件都补齐了，网格缩放后查找结果也不会丢。

**Agent、Provider 与远程**

- 经过审核的 Agent 版本前移：DeepSeek Harness `0.4.37`、Codex 用上 `codex-acp` `2.0.1`、Kimi 换到 Kimi Code CLI 后继者、zcode `0.54.0`；未充值账号现在如实显示，而不是让模型列表失败。
- Provider 配置可以整体读回编辑、从还没保存的草稿抓取模型、覆盖请求头，并与其他 CLI 工具已有的设置互相导入导出。
- 浏览器授权的 Agent 登录不再被隐藏：Cursor、Grok、Gemini、Cline、CodeBuddy、Antigravity 都不声明 logout，它们的 OAuth 方式过去被当成不可用，现在会被识别并提供。
- Agent 生成的图片会出现在会话里，终端输出边产生边流出，CodeBuddy 可以在回合中途被引导。
- 一个桌面端可以在同一台机器上服务客户端——这正是 TUI 能本地附着的前提；配对窗口能在没有发布路由的局域网上打开，已配对的客户端可以恢复或遗忘设备。
- 远程会报告每个受管 worktree 的状态、定时发送将附带的内容、文件夹的归档窗口、会话的挂起状态，以及运行时自己的名字。
- Git 界面把"拉取"和"刷新抓取"分开，拉取会把抓到的提交放进当前检出的分支。

### ⚡️ 性能

- TUI 只重绘读者眼前的那一面，看不见的界面不消耗资源。
- 桌面端对话流大幅减少了重绘：展开的组按需开窗，已结束的回合被保留，实时标签不再持续重绘；Markdown 文件预览也改为虚拟化。
- 侧栏在折叠标题时跳过会话查询，结束的运行提示改为回绕而不是重放转圈帧，多选每帧只解析一次。
- 子 Agent 的对话流不再逐帧折叠，流式工具调用折叠成一行，过大的会话列表响应被避免。

### 🔧 底层

- 版本号统一升到 `0.1.0-rc.8`：工作区、桌面打包输入、Android versionName 回退值、README、部署文档，以及许可证策略里已审核的资源包版本。
- 许可证 SBOM 与第三方声明按当前依赖图重新生成：1085 个 Cargo 包、9 个资源／字体包、5 个原生与安装器输入，全部已批准。
- 工作区新增四个成员：`apps/tui` 与 `crates/vibex-tui`（终端客户端）、`crates/computer`（电脑操作）、`crates/vibex-brands`（共用品牌标记）；`crates/vibex-markdown` 并入 TUI，桌面端改由 `gpui-component` 渲染 Markdown。
- 工作区升级到 `gpui-kit` `0.7.3` 与 `gpui-pre` `0.3.9`；核心协议新增 `before` 游标，历史可以向更早翻页。
- macOS 与 Windows 打包仍属于构建与打包证据，PDFium 分发批准仍只覆盖 linux-x86_64。
