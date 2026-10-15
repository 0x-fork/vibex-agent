//! Telling the reader about a session the terminal is not showing.
//!
//! The client's own chrome already answers "what happened": a failed session
//! wears a `✗` on its row and says what it last did on its second line. That is
//! enough while the reader is reading the list. It is not enough when the
//! reader has stepped away from the terminal, and it is nothing at all when
//! they are looking at another tab — which is the case this module exists for.
//!
//! A terminal has no notification API. What it does have is a handful of
//! control sequences, and they are the only channel that survives `ssh`: a
//! client running on a cloud host writes the bytes, the local terminal (or the
//! multiplexer in front of it) interprets them, and the notification appears on
//! the machine the reader is actually sitting at.
//!
//! # The transports
//!
//! | transport | sequence | reaches |
//! | --- | --- | --- |
//! | [`Transport::Bell`] | `BEL` | every terminal; tmux flags the window |
//! | [`Transport::Title`] | `OSC 2` | every terminal's tab/window title |
//! | [`Transport::Osc9`] | `OSC 9` | iTerm2, WezTerm, Windows Terminal, Ghostty |
//! | [`Transport::Osc777`] | `OSC 777` | rxvt-unicode, VTE |
//! | [`Transport::Osc99`] | `OSC 99` | kitty |
//! | [`Transport::Iterm2Attention`] | `OSC 1337` | iTerm2 (bounces, no text) |
//!
//! Beside them sits one channel that is not an escape: [`SystemDelivery`] hands
//! the same news to the host's own notification service (`notify-send` on Linux,
//! `osascript` on macOS), which is what draws a real desktop bubble. It is the
//! only thing here that starts a process, so it is fenced twice: it never fires
//! on a seat reached over `ssh` — a bubble raised on a cloud host is one nobody
//! will ever see, while the client would report success — and it is a switch of
//! its own on the settings page, which also says so when the host has no program
//! to draw it with. The crate keeps its "linkable without a window server"
//! boundary either way: the bubble is drawn by a program the reader's desktop
//! already runs, not by this process.
//!
//! Which one to use is decided by [`TerminalProfile::detect`] from the
//! environment, the same way the client already decides whether it may write a
//! keyboard-protocol push: **optimistic and unidentified by probe**. Asking the
//! terminal what it supports would put a query in the input queue the interface
//! is about to read, and the answer changes nothing but the spelling of a
//! sequence the terminal ignores when it does not know it. A reader whose
//! terminal is not recognised, or is misrecognised, names the transport in
//! `~/.vibex/tui-notify.json`.
//!
//! # The multiplexer
//!
//! tmux and screen sit *between* the client and the terminal that would draw a
//! bubble, and both swallow OSC sequences they do not implement. `OSC 0`/`2` is
//! the exception — they implement it and re-emit it for the outer terminal —
//! and the bell is the other: tmux forwards it and flags the window
//! (`monitor-bell` is on by default). So a session inside a multiplexer is told
//! with the bell unless the reader configured a transport of their own, and an
//! OSC they did configure is wrapped in tmux's passthrough envelope (which
//! needs `allow-passthrough on`; without it the terminal simply never sees it,
//! which is a missing notification rather than a broken frame).
//!
//! # The policy
//!
//! A notification is news only when the reader is not already reading the thing
//! it is about. The same three questions the desktop asks are asked here: is the
//! client enabled at all, is this kind of event one the reader asked about, and
//! is the reader looking at that session right now. "Looking" means the
//! session's own page is in front *and* the terminal reports focus; a reader
//! sitting on the session list has not read the failure that just landed on it.
//!
//! # One reminder per event
//!
//! Two channels that both work would otherwise say the same thing twice — a bell
//! beside a bubble — which is what a reader who turned on both the popup and an
//! escape used to get. They are routed rather than duplicated, by the rule the
//! desktop already applies to its own window: a reader who is *at* the terminal
//! is told by the terminal ([`Transport`]), and a reader who has switched away
//! is told by the popup, which reaches across applications and carries words.
//! When no popup can be drawn — a remote seat, no program, the switch off — the
//! escape carries the news in both cases, because it is the only channel that
//! survives `ssh`. Focus that was never reported counts as "at the terminal", so
//! a terminal without DECSET 1004 keeps its escape and simply never raises a
//! bubble.
//!
//! The reader's own test notification is the exception, deliberately: it goes
//! through every live channel, because its job is to show which ones work.
//!
//! # A desktop on the other end
//!
//! When the runtime on the other end is a Vibex desktop
//! (`RemoteServerKind::Desktop`), that desktop announces the same events to the
//! same reader, and the routing above cannot see it: the second announcer is not
//! in this process. [`NotifyPreferences::light_when_desktop`] — on by default —
//! is the answer. While a desktop is on the other end this client steps back to
//! the lightest signal it has, the window title, and leaves the popup to the
//! desktop. It is a switch rather than an inference because a reader may be
//! driving a desktop they are not watching, and it is the reader's call which of
//! the two speaks.
//!
//! # What is in the message
//!
//! Only what the runtime already publishes plus the Agent's name: a kind and a
//! session id. The Agent's prose never rides a notification, because a terminal
//! may log an OSC payload and a title is read by anyone glancing at the screen.
//! The session title is therefore opt-in (`includeSessionTitle`).
//!
//! # The settings page
//!
//! Everything a reader can change here — the master switch, the three kinds, the
//! transport, the title, the desktop bubble — is a field of
//! [`NotifyPreferences`], and the settings surface is a second door onto the
//! same file the hand-written `~/.vibex/tui-notify.json` opens. [`Notifier`]
//! keeps the preferences it was built from and hands them back edited, so the
//! page and the file cannot disagree about what the reader chose.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use vibex_backend::BackendEvent;
use vibex_core::{AgentNotificationKind, TimelinePayload};

use crate::app::App;

/// How many `(session, event)` keys the client remembers to suppress a replay.
///
/// The live stream and the polling fallback both deliver the same committed
/// item, and a remote seat may deliver the runtime's notification intent beside
/// the timeline item that produced it. Both spell the same `source_event_id`, so
/// remembering a bounded window of them is what keeps one failure from being
/// announced twice. The window is the number of distinct events, not of
/// notifications: a quiet machine keeps a long history for free.
const LEDGER_CAPACITY: usize = 256;

/// The longest payload any transport is handed.
///
/// A terminal truncates an over-long title or notification anyway, and a
/// payload that is too long to read is worse than one that says less. The
/// bound is on the sanitized characters, not bytes.
const MAX_PAYLOAD_CHARS: usize = 120;

/// The name a marked title goes back to.
const APP_TITLE: &str = "Vibex";

/// What a notification is about, in the runtime's own three families.
///
/// The names are the runtime's notification kinds, not the timeline payloads
/// that also produce them: a timeline [`TimelinePayload::Error`] and an
/// [`AgentNotificationKind::TurnFailed`] are the same news, and the policy has
/// one switch per kind rather than one per source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Completed,
    NeedsInput,
    Failed,
}

impl NotificationKind {
    fn index(self) -> usize {
        match self {
            Self::Completed => 0,
            Self::NeedsInput => 1,
            Self::Failed => 2,
        }
    }
}

/// What one notification says, before a channel lays it out.
///
/// The parts are kept apart because the channels have different room. A control
/// sequence is one line whatever it carries — a control character inside an OSC
/// payload can end it, so the payload is flattened before it is written — while
/// a desktop bubble has a summary line and as much body as the reader will read.
/// Joining the parts early is what made a bubble read as one run-on sentence:
/// the Agent, the event and the session title all ran together with a `·`
/// between them and the reader had to parse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationMessage {
    /// The Agent the news is about, when it is about one. A test is not.
    pub agent: Option<String>,
    /// What happened, in the reader's language.
    pub sentence: String,
    /// The session's own title, when the reader lets it ride.
    pub session_title: Option<String>,
}

impl NotificationMessage {
    /// The one line a terminal sequence can carry.
    ///
    /// The Agent leads because it is the reader's only way to tell one session
    /// from another, and the session title trails because it is the part a
    /// reader who is scanning for the Agent does not need.
    fn line(&self) -> String {
        let mut line = String::new();
        if let Some(agent) = &self.agent {
            line.push_str(agent);
            line.push_str(" · ");
        }
        line.push_str(&self.sentence);
        if let Some(title) = &self.session_title {
            line.push_str(" · ");
            line.push_str(title);
        }
        line
    }

    /// The bubble's own line, above the body.
    ///
    /// The product is named here because the two popup programs disagree about
    /// where a name comes from — `notify-send` hands one to the notification
    /// service, `osascript` does not — and the Agent is the half a reader with
    /// several sessions actually needs.
    fn summary(&self, app: &str) -> String {
        match &self.agent {
            Some(agent) => format!("{app} · {agent}"),
            None => app.to_string(),
        }
    }

    /// The bubble's body: what happened, and below it which session.
    ///
    /// Two lines rather than one sentence, because a title read after the event
    /// is what tells the reader which of several sessions to come back to. Each
    /// part is flattened before it is placed, so the only line breaks in a body
    /// are the ones the layout put there: a newline inside a session's title is
    /// not a line the runtime gets to write.
    fn body(&self) -> String {
        match &self.session_title {
            Some(title) => format!("{}\n{}", sanitize(&self.sentence), sanitize(title)),
            None => sanitize(&self.sentence),
        }
    }
}

/// Which control sequence carries a notification to the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// The client answers in its own chrome only.
    Off,
    Bell,
    Title,
    Osc9,
    Osc777,
    Osc99,
    Iterm2Attention,
}

impl Transport {
    /// Every transport the settings page offers, in the order it offers them.
    ///
    /// `Off` leads because a reader looking for it should not have to walk the
    /// whole list, and the OSCs follow the order of the table above.
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Bell,
        Self::Title,
        Self::Osc9,
        Self::Osc777,
        Self::Osc99,
        Self::Iterm2Attention,
    ];

    /// The spelling the configuration file uses.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Bell => "bell",
            Self::Title => "title",
            Self::Osc9 => "osc9",
            Self::Osc777 => "osc777",
            Self::Osc99 => "osc99",
            Self::Iterm2Attention => "iterm2_attention",
        }
    }

    /// The transport a stored id names, when it names one this build knows.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "off" => Some(Self::Off),
            "bell" => Some(Self::Bell),
            "title" => Some(Self::Title),
            "osc9" => Some(Self::Osc9),
            "osc777" => Some(Self::Osc777),
            "osc99" => Some(Self::Osc99),
            "iterm2_attention" => Some(Self::Iterm2Attention),
            _ => None,
        }
    }

    /// Whether the sequence is an OSC tmux would swallow unless it is wrapped.
    const fn is_osc(self) -> bool {
        matches!(
            self,
            Self::Osc9 | Self::Osc777 | Self::Osc99 | Self::Iterm2Attention
        )
    }
}

/// A terminal multiplexer in front of the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Multiplexer {
    Tmux,
    Screen,
}

/// What the environment says about the terminal this client is writing to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalProfile {
    /// The sequence to use when the reader named none.
    pub transport: Transport,
    /// The multiplexer the sequences travel through, when there is one.
    pub multiplexer: Option<Multiplexer>,
    /// The terminal the environment identified, for a settings readout.
    pub terminal: Option<String>,
    /// Whether the client is running on a host the reader reached over `ssh`.
    ///
    /// A remote seat is the reason the escape transports exist and the reason
    /// the desktop bubble is switched off: the host this process runs on is not
    /// the host the reader is looking at.
    pub remote: bool,
}

impl TerminalProfile {
    /// Read the process environment.
    pub fn detect() -> Self {
        Self::detect_from(|key| std::env::var(key).ok())
    }

    /// Read a profile from a lookup, so the detection table is a test and not a
    /// comment. The lookup is called at most once per variable.
    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let mut get = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());

        let multiplexer = if get("TMUX").is_some() {
            Some(Multiplexer::Tmux)
        } else if get("STY").is_some() {
            Some(Multiplexer::Screen)
        } else {
            None
        };

        // `sshd` sets all three for an interactive session; any one of them is
        // enough, because a stripped environment may keep only one.
        let remote = get("SSH_CONNECTION").is_some()
            || get("SSH_TTY").is_some()
            || get("SSH_CLIENT").is_some();

        let term = get("TERM").unwrap_or_default();
        let program = get("TERM_PROGRAM").unwrap_or_default();
        let lc_terminal = get("LC_TERMINAL").unwrap_or_default();

        // Ordered most specific first: kitty sets `TERM=xterm-kitty` and a
        // window id, iTerm2 sets both `TERM_PROGRAM` and `LC_TERMINAL`, and a
        // Windows Terminal inside WSL sets no `TERM_PROGRAM` at all.
        let identified: Option<(&str, Transport)> =
            if get("KITTY_WINDOW_ID").is_some() || term.contains("kitty") {
                Some(("kitty", Transport::Osc99))
            } else if program.contains("iTerm") || lc_terminal.contains("iTerm") {
                Some(("iTerm2", Transport::Osc9))
            } else if program.contains("WezTerm") || term.contains("wezterm") {
                Some(("WezTerm", Transport::Osc9))
            } else if program.contains("ghostty") || term.contains("ghostty") {
                Some(("Ghostty", Transport::Osc9))
            } else if get("WT_SESSION").is_some() {
                Some(("Windows Terminal", Transport::Osc9))
            } else if term.contains("rxvt") {
                Some(("rxvt-unicode", Transport::Osc777))
            } else if get("VTE_VERSION").is_some() {
                Some(("VTE", Transport::Osc777))
            } else {
                None
            };

        let terminal = identified.map(|(name, _)| name.to_string());
        // Inside a multiplexer the identified terminal is the *outer* one, and
        // an OSC aimed at it has to be passed through — which tmux refuses
        // unless the reader turned passthrough on. The bell needs no such
        // permission, so it is the default there and a configured transport
        // overrides it.
        let transport = match (multiplexer, identified) {
            (Some(_), _) => Transport::Bell,
            (None, Some((_, transport))) => transport,
            (None, None) => Transport::Title,
        };
        Self {
            transport,
            multiplexer,
            terminal,
            remote,
        }
    }
}

/// What the client remembers about notifications between runs.
///
/// Every field is optional so a file written by another build is not an error:
/// an absent field is the shipped default, and an unknown transport id falls
/// back to detection rather than to "off".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// One of [`Transport::id`]; absent means "detect it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_agent_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_agent_needs_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_agent_failed: Option<bool>,
    /// Whether the session title may ride the payload. Off by default: a title
    /// is derived from what the reader typed, and a terminal may log it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_session_title: Option<bool>,
    /// Whether the host's own desktop may be asked to draw a bubble.
    ///
    /// On by default, because a reader who is away from the terminal is exactly
    /// the reader a bubble reaches — but only ever on a local seat, where the
    /// bubble lands on the machine they are sitting at, and only while the
    /// terminal window is not the focused one, so it does not say the same thing
    /// beside the escape the reader is looking at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_popup: Option<bool>,
    /// Whether this client steps back while a Vibex desktop is on the other end.
    ///
    /// On by default: the desktop hosting the runtime announces the same events
    /// itself, and two clients telling one reader about one failure is the
    /// duplicate this switch exists to stop. While it is on and the runtime is a
    /// desktop shell, the terminal channel is pinned to the title and the popup
    /// is left to the desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_when_desktop: Option<bool>,
}

impl NotifyPreferences {
    /// Read what a previous run wrote, or remember nothing.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Persist the choices. A failure is silent, like the interface's own: a
    /// preference is not worth interrupting the reader over.
    pub fn save(&self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, body);
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    fn transport(&self) -> Option<Transport> {
        self.transport.as_deref().and_then(Transport::from_id)
    }

    fn kind_enabled(&self, kind: NotificationKind) -> bool {
        match kind {
            NotificationKind::Completed => self.notify_agent_completed,
            NotificationKind::NeedsInput => self.notify_agent_needs_input,
            NotificationKind::Failed => self.notify_agent_failed,
        }
        .unwrap_or(true)
    }

    fn includes_session_title(&self) -> bool {
        self.include_session_title.unwrap_or(false)
    }

    fn includes_system_popup(&self) -> bool {
        self.system_popup.unwrap_or(true)
    }

    fn stays_light_for_desktop(&self) -> bool {
        self.light_when_desktop.unwrap_or(true)
    }
}

/// The program a native popup is handed to, when the host has one.
///
/// This is the one place in the module that names an external process. Which
/// program it is follows the platform: macOS ships `osascript` with the system,
/// while Linux and the BSDs get a bubble from `notify-send`, part of libnotify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDelivery {
    /// `notify-send`, from libnotify.
    NotifySend,
    /// `osascript`, which every macOS install has.
    OsaScript,
}

impl SystemDelivery {
    /// The delivery this host would use, when it has one.
    ///
    /// A missing program is not an error to report: it means the channel is
    /// simply not available here, which the settings page states rather than
    /// letting the reader wonder why nothing appeared.
    pub fn detect() -> Option<Self> {
        Self::detect_with(|program| find_in_path(program).is_some())
    }

    /// The detection table, with the search injected so it is a test.
    pub fn detect_with(mut found: impl FnMut(&str) -> bool) -> Option<Self> {
        if cfg!(target_os = "macos") && found("osascript") {
            return Some(Self::OsaScript);
        }
        if found("notify-send") {
            return Some(Self::NotifySend);
        }
        None
    }

    /// The command that draws one bubble.
    ///
    /// The caller spawns it: building the command is pure, which is what lets
    /// the arguments be asserted without a desktop to run them on.
    pub fn command(self, title: &str, body: &str) -> Command {
        match self {
            Self::NotifySend => {
                let mut command = Command::new("notify-send");
                command
                    .arg("--app-name=Vibex")
                    .arg("--icon=utilities-terminal")
                    .arg(title)
                    .arg(body);
                command
            }
            Self::OsaScript => {
                // The script is a fixed sentence with the two fields quoted in;
                // `quote` is what keeps a body from ending the string literal
                // and running a statement of its own.
                let mut command = Command::new("osascript");
                command.arg("-e").arg(format!(
                    "display notification {} with title {}",
                    quote(body),
                    quote(title)
                ));
                command
            }
        }
    }
}

/// One popup, on its way to the host's own notification service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotification {
    pub title: String,
    pub body: String,
}

/// What the desktop-popup setting can honestly say.
///
/// `On` and `Off` are choices the reader may change; `Desktop`, `Remote` and
/// `Unsupported` are facts about where the client runs, and the row reports them
/// instead of a switch position that would not do anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifySystemState {
    /// The reader asked for a bubble and this host will draw one.
    On,
    /// The reader asked for none.
    Off,
    /// The desktop shell hosting this runtime draws the notifications itself,
    /// so this client leaves the channel to it.
    Desktop,
    /// The seat is reached over `ssh`, so a bubble would be raised on the wrong
    /// machine.
    Remote,
    /// No `notify-send` and no `osascript` on this host.
    Unsupported,
}

/// The delivery this host would use, when it has one.
pub fn system_delivery() -> Option<SystemDelivery> {
    SystemDelivery::detect()
}

/// Hand one notification to the host's popup, without waiting for it.
///
/// The child is reaped on a thread of its own: a notification outlives the turn
/// that raised it, and a process left unreaped per notification would be a
/// zombie per notification. Every failure is silent — a desktop that is not
/// running is not news worth interrupting the reader over.
pub fn deliver_system(notification: &SystemNotification) {
    let Some(delivery) = SystemDelivery::detect() else {
        return;
    };
    let mut command = delivery.command(&notification.title, &notification.body);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Whether a program is reachable on this process's `PATH`.
fn find_in_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Whether a path names a file this process may run.
///
/// The permission bits are read rather than the file opened: the probe runs at
/// startup for the settings readout, and it must not disturb the file it looks
/// at.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// A string as an AppleScript expression.
///
/// AppleScript has no escape for a newline inside a literal, so a multi-line
/// body is built as one literal per line joined with `linefeed` — the language's
/// own name for the character — which is how a bubble gets its second line. The
/// two characters that could end a literal, the quote and the backslash, are
/// escaped; everything else has already been sanitized by the caller.
fn quote(value: &str) -> String {
    value
        .split('\n')
        .map(|line| {
            let mut quoted = String::with_capacity(line.len() + 2);
            quoted.push('"');
            for character in line.chars() {
                if matches!(character, '"' | '\\') {
                    quoted.push('\\');
                }
                quoted.push(character);
            }
            quoted.push('"');
            quoted
        })
        .collect::<Vec<_>>()
        .join(" & linefeed & ")
}

/// What the reader is doing while a notification is considered.
///
/// "Looking at the session" is two facts, not one: the session's own page has
/// to be the page in front, and the terminal has to be the focused window. A
/// reader on the session list is one keypress from the failure, which is not the
/// same as having read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyContext<'a> {
    /// The session the client has selected.
    pub selected_session_id: Option<&'a str>,
    /// Whether the page in front shows that session.
    pub session_page: bool,
}

/// The client's out-of-band notification state.
///
/// It owns no terminal I/O: a decision and the bytes it produces are collected
/// in `pending`, and the run loop writes them after the frame. A popup is
/// collected the same way in `pending_system`, and the run loop hands it to the
/// host after the frame. Keeping both out of the reducer is what lets every rule
/// here be a unit test.
#[derive(Debug)]
pub struct Notifier {
    /// The choices as they were read, kept so the settings page edits the same
    /// object the file holds rather than a parallel copy of it.
    preferences: NotifyPreferences,
    /// Where those choices are written back, when the reader's home is known.
    preferences_path: Option<std::path::PathBuf>,
    enabled: bool,
    /// The transport the reader chose, or detection picked. What is actually
    /// written is [`Notifier::transport`], which the desktop policy can pin.
    transport: Transport,
    /// The transport detection picked, for the "automatic" setting to name.
    detected: Transport,
    multiplexer: Option<Multiplexer>,
    terminal: Option<String>,
    /// Whether the seat is reached over `ssh`, which is what makes a local
    /// bubble useless rather than merely unwanted.
    remote: bool,
    delivery: Option<SystemDelivery>,
    system_popup: bool,
    /// Whether the runtime on the other end is a Vibex desktop shell, which
    /// announces the same events to the same reader.
    desktop_present: bool,
    light_when_desktop: bool,
    kind_switches: [bool; 3],
    include_session_title: bool,
    focused: bool,
    title_marked: bool,
    seen: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
    pending: Vec<u8>,
    pending_system: Vec<SystemNotification>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Notifier {
    /// Read the saved choices from `path` and detect the terminal.
    ///
    /// The path is the composition root's decision, like every other file this
    /// client keeps: a test harness must not read — or write — the home of
    /// whoever ran it.
    pub fn new(path: Option<std::path::PathBuf>) -> Self {
        let preferences = NotifyPreferences::load(path.as_deref());
        Self::from_preferences(preferences, path)
    }

    /// Build from already-read choices, detecting the terminal and the host.
    pub fn from_preferences(
        preferences: NotifyPreferences,
        path: Option<std::path::PathBuf>,
    ) -> Self {
        let profile = TerminalProfile::detect();
        Self::build(preferences, profile, system_delivery(), path)
    }

    /// Build from every detected fact, which is what makes the rules a test.
    fn build(
        preferences: NotifyPreferences,
        profile: TerminalProfile,
        delivery: Option<SystemDelivery>,
        path: Option<std::path::PathBuf>,
    ) -> Self {
        // Read every choice out before the preferences move into the struct:
        // the live switches are the resolved answers, and the preferences are
        // kept as they were written so the settings page edits the same object.
        let transport = preferences.transport().unwrap_or(profile.transport);
        let enabled = preferences.is_enabled();
        let system_popup = preferences.includes_system_popup();
        let light_when_desktop = preferences.stays_light_for_desktop();
        let kind_switches = [
            preferences.kind_enabled(NotificationKind::Completed),
            preferences.kind_enabled(NotificationKind::NeedsInput),
            preferences.kind_enabled(NotificationKind::Failed),
        ];
        let include_session_title = preferences.includes_session_title();
        Self {
            preferences,
            preferences_path: path,
            enabled,
            transport,
            detected: profile.transport,
            multiplexer: profile.multiplexer,
            terminal: profile.terminal,
            remote: profile.remote,
            delivery,
            system_popup,
            // Nothing is known about the runtime at build time: the seat is the
            // composition root's answer, and it arrives through
            // [`Self::with_runtime_kind`] before the first event can be seen.
            desktop_present: false,
            light_when_desktop,
            kind_switches,
            include_session_title,
            // Before the terminal has told us otherwise, the reader is assumed
            // to be at it: the client was started from a keypress, and a
            // terminal that never reports focus should not turn every
            // on-screen failure into a bell.
            focused: true,
            title_marked: false,
            seen: HashSet::new(),
            order: VecDeque::new(),
            pending: Vec::new(),
            pending_system: Vec::new(),
        }
    }

    /// Note what the runtime on the other end is.
    ///
    /// The seat resolves this once, before the interface starts, so it is a fact
    /// about the run rather than something that changes under the reader. A
    /// desktop shell is the one kind that announces the same events itself, and
    /// it is what [`Self::desktop_takes_over`] reads.
    pub fn with_runtime_kind(mut self, kind: vibex_core::RemoteServerKind) -> Self {
        self.set_desktop_present(kind.is_desktop());
        self
    }

    /// Note whether a Vibex desktop is on the other end.
    pub fn set_desktop_present(&mut self, present: bool) {
        self.desktop_present = present;
    }

    /// The transport this client will actually write.
    ///
    /// Not always the one the reader chose: while a desktop is on the other end
    /// and they asked this client to stay light, the title is the only signal
    /// left — the desktop draws the rest. A reader who turned the terminal
    /// channel off keeps it off, because "light" is not "louder than what I
    /// asked for".
    pub fn transport(&self) -> Transport {
        match self.transport {
            Transport::Off => Transport::Off,
            _ if self.desktop_takes_over() => Transport::Title,
            chosen => chosen,
        }
    }

    /// The transport the environment identified, which "automatic" resolves to.
    pub fn detected_transport(&self) -> Transport {
        self.detected
    }

    /// The transport the reader named, when they named one.
    pub fn transport_preference(&self) -> Option<Transport> {
        self.preferences.transport()
    }

    /// The terminal the environment identified, when it identified one.
    pub fn terminal(&self) -> Option<&str> {
        self.terminal.as_deref()
    }

    /// Whether the client runs on a host the reader reached over `ssh`.
    pub fn remote(&self) -> bool {
        self.remote
    }

    /// The popup program this host has, when it has one.
    pub fn delivery(&self) -> Option<SystemDelivery> {
        self.delivery
    }

    /// Whether the reader asked the host's desktop to draw a bubble.
    pub fn system_popup(&self) -> bool {
        self.system_popup
    }

    /// Whether a bubble would actually be drawn right now.
    ///
    /// Four facts, not one: the reader asked for it, the host has a program to
    /// draw it with, the reader is sitting at this host, and no desktop on the
    /// other end has taken the channel over.
    pub fn system_available(&self) -> bool {
        self.system_popup && !self.remote && self.delivery.is_some() && !self.desktop_takes_over()
    }

    /// Whether a Vibex desktop on the other end speaks for this client.
    ///
    /// The runtime is a desktop shell and the reader asked this client to stay
    /// light while it is: the popup is the desktop's, and the terminal channel is
    /// pinned to the title. Both halves are the reader's to change — the switch
    /// here, the seat at the command line.
    pub fn desktop_takes_over(&self) -> bool {
        self.light_when_desktop && self.desktop_present
    }

    /// Whether this client steps back for a desktop, as the reader set it.
    pub fn light_when_desktop(&self) -> bool {
        self.light_when_desktop
    }

    /// Whether notifications are on at all.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether one kind of news is one the reader asked about.
    pub fn kind_enabled(&self, kind: NotificationKind) -> bool {
        self.kind_switches[kind.index()]
    }

    /// Whether the session title may ride the payload.
    pub fn includes_session_title(&self) -> bool {
        self.include_session_title
    }

    /// Whether the terminal is currently the focused window.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Whether the popup is the channel this event should use.
    ///
    /// The popup is for a reader who is not at the terminal: it reaches across
    /// applications and carries words, where an escape only decorates a window
    /// they are not looking at. A reader who is at the terminal gets that
    /// terminal's own signal instead, so one event is one reminder rather than
    /// two. When no popup can be drawn the escape carries the news in both cases,
    /// which is what keeps a remote seat informed.
    fn popup_carries(&self) -> bool {
        self.system_available() && !self.focused
    }

    /// Whether any channel would carry news right now.
    ///
    /// The routing is what makes this more than "either channel is on": with the
    /// popup live and the reader at the terminal, an `Off` transport has nothing
    /// to write, and the event is not announced rather than announced into a
    /// channel that will not use it.
    fn any_channel(&self) -> bool {
        self.popup_carries() || self.transport() != Transport::Off
    }

    /// Turn the whole layer on or off.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.preferences.enabled = Some(enabled);
    }

    /// Turn one kind of news on or off.
    pub fn set_kind_enabled(&mut self, kind: NotificationKind, enabled: bool) {
        self.kind_switches[kind.index()] = enabled;
        match kind {
            NotificationKind::Completed => self.preferences.notify_agent_completed = Some(enabled),
            NotificationKind::NeedsInput => {
                self.preferences.notify_agent_needs_input = Some(enabled)
            }
            NotificationKind::Failed => self.preferences.notify_agent_failed = Some(enabled),
        }
    }

    /// Name the transport, or hand the choice back to detection with `None`.
    pub fn set_transport(&mut self, transport: Option<Transport>) {
        match transport {
            Some(transport) => {
                self.transport = transport;
                self.preferences.transport = Some(transport.id().to_string());
            }
            None => {
                self.transport = self.detected;
                self.preferences.transport = None;
            }
        }
    }

    /// Ask the host's desktop for a bubble, or stop asking.
    pub fn set_system_popup(&mut self, on: bool) {
        self.system_popup = on;
        self.preferences.system_popup = Some(on);
    }

    /// Step back for a desktop on the other end, or speak beside it.
    pub fn set_light_when_desktop(&mut self, on: bool) {
        self.light_when_desktop = on;
        self.preferences.light_when_desktop = Some(on);
    }

    /// Let the session title ride the payload, or keep it off.
    pub fn set_include_session_title(&mut self, on: bool) {
        self.include_session_title = on;
        self.preferences.include_session_title = Some(on);
    }

    /// Write the choices down, so the next run starts where this one left off.
    ///
    /// The same file the hand-written configuration uses, which is the point:
    /// there is one answer to what this client remembers, not two.
    pub fn save_preferences(&self) {
        self.preferences.save(self.preferences_path.as_deref());
    }

    /// Note that the terminal gained or lost focus.
    ///
    /// A title that was marked has done its job once the reader is back, so the
    /// tab goes back to naming the product. The write is queued like any other;
    /// this module never touches the terminal itself.
    pub fn set_focus(&mut self, focused: bool) {
        if focused && !self.focused && self.title_marked {
            let sequence = title_sequence(APP_TITLE);
            self.pending.extend_from_slice(sequence.as_bytes());
            self.title_marked = false;
        }
        self.focused = focused;
    }

    /// Whether one event should reach the reader out of band.
    ///
    /// The dedup key is consumed here rather than at [`Self::emit`], so a
    /// suppressed event does not stop a later duplicate from being announced
    /// once the reader has looked away.
    pub fn accept(
        &mut self,
        kind: NotificationKind,
        session_id: &str,
        source_event_id: &str,
        context: &NotifyContext<'_>,
    ) -> bool {
        if !self.enabled || !self.any_channel() || !self.kind_switches[kind.index()] {
            return false;
        }
        if self.focused && context.session_page && context.selected_session_id == Some(session_id) {
            return false;
        }
        self.remember(session_id, source_event_id)
    }

    /// Build one notification and queue it on the one channel that carries it.
    ///
    /// The channel is chosen, not stacked: a reader who is at the terminal is
    /// told by the terminal, a reader who has switched away is told by the popup,
    /// and one event is one reminder either way. [`Self::emit_test`] is the one
    /// caller that wants every live channel at once.
    pub fn emit(&mut self, session_id: &str, app: &str, message: &NotificationMessage) {
        if self.popup_carries() {
            self.queue_system(&message.summary(app), &message.body());
        } else {
            self.queue_terminal(session_id, &message.line());
        }
    }

    /// Raise the reader's own test notification.
    ///
    /// It goes through every channel a real one could use — that is the point of
    /// it: a reader who cannot see it knows the terminal, the multiplexer or the
    /// desktop is the reason, not the client. A real event is routed to one
    /// channel, but a test is the one moment the reader asked to see all of them,
    /// and the row that offers it names them. The policy is skipped (a test is
    /// asked for, not news) but the master switch is not: a notification layer
    /// that is off has nothing to test.
    pub fn emit_test(&mut self, app: &str, sentence: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let terminal = self.queue_terminal("test", sentence);
        let system = self.queue_system(app, sentence);
        terminal || system
    }

    /// Build the sequence for one notification and queue it.
    ///
    /// Reports whether a sequence was queued: an `Off` transport writes
    /// nothing, which is not the same as a payload that came out empty.
    fn queue_terminal(&mut self, session_id: &str, line: &str) -> bool {
        let sequence = self.sequence(session_id, line);
        if sequence.is_empty() {
            return false;
        }
        if self.transport() == Transport::Title {
            self.title_marked = true;
        }
        self.pending.extend_from_slice(sequence.as_bytes());
        true
    }

    /// Queue one popup for the host's own desktop.
    ///
    /// The body keeps its line breaks: a bubble is read by a person, and the
    /// second line is the session's name rather than more of the sentence.
    fn queue_system(&mut self, summary: &str, body: &str) -> bool {
        if !self.system_available() {
            return false;
        }
        self.pending_system.push(SystemNotification {
            title: sanitize(summary),
            body: sanitize_lines(body),
        });
        true
    }

    /// Take the bytes the run loop has to write, if any.
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    /// Take the popups the run loop has to hand to the host, if any.
    pub fn take_pending_system(&mut self) -> Vec<SystemNotification> {
        std::mem::take(&mut self.pending_system)
    }

    /// Record a dedup key, reporting whether it was new.
    fn remember(&mut self, session_id: &str, source_event_id: &str) -> bool {
        let key = (session_id.to_string(), source_event_id.to_string());
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > LEDGER_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    /// The control sequence one notification is carried by.
    fn sequence(&self, session_id: &str, line: &str) -> String {
        let app = APP_TITLE;
        let body = sanitize(line);
        let transport = self.transport();
        let inner = match transport {
            Transport::Off => return String::new(),
            Transport::Bell => "\x07".to_string(),
            // The marker is what makes the tab name readable at a glance, and
            // it is the body that says why: the title transport has no second
            // line to use.
            Transport::Title => title_sequence(&format!("● {body}")),
            Transport::Osc9 => format!("\x1b]9;{body}\x07"),
            Transport::Osc777 => {
                // The payload is `notify;title;body`, so a semicolon inside a
                // field would be read as the next one.
                format!(
                    "\x1b]777;notify;{};{}\x07",
                    app.replace(';', ","),
                    body.replace(';', ",")
                )
            }
            Transport::Osc99 => {
                format!("\x1b]99;i={};{body}\x07", identifier(session_id))
            }
            Transport::Iterm2Attention => "\x1b]1337;RequestAttention=yes\x07".to_string(),
        };
        match self.multiplexer {
            Some(Multiplexer::Tmux) if transport.is_osc() => tmux_passthrough(&inner),
            // screen has no passthrough envelope; an OSC it does not implement
            // is ignored, which the reader notices as a missing notification
            // rather than as a broken screen.
            _ => inner,
        }
    }
}

/// Wrap a sequence so tmux forwards it to the terminal behind it.
///
/// tmux's envelope is `DCS tmux; <payload> ST`, and the payload is the original
/// sequence with every `ESC` doubled. It is only delivered when the reader has
/// `allow-passthrough on`; without it tmux discards the whole envelope, so this
/// degrades to "no notification" rather than to stray text on the screen.
pub(crate) fn tmux_passthrough(inner: &str) -> String {
    format!("\x1bPtmux;{}\x1b\\", inner.replace('\x1b', "\x1b\x1b"))
}

/// `OSC 2` — the window and tab title.
fn title_sequence(title: &str) -> String {
    format!("\x1b]2;{title}\x07")
}

/// A session id inside OSC 99's metadata field.
///
/// The metadata is `key=value` pairs joined by `:` and `;`, so the id is
/// reduced to the characters that cannot invent a field of their own. A real id
/// is a UUID and loses nothing.
fn identifier(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect()
}

/// Make a value safe to put inside an OSC payload.
///
/// A control character inside a payload is not text: `BEL` and `ESC` end the
/// sequence, so an Agent — or a title derived from what the reader typed —
/// could otherwise close the sequence and write control bytes of its own. Runs
/// of whitespace collapse to one space so a title cannot be a paragraph, and the
/// result is bounded because a terminal truncates it anyway.
fn sanitize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(character);
        if out.chars().count() >= MAX_PAYLOAD_CHARS {
            break;
        }
    }
    out
}

/// Make a value safe to put in a notification body that is read as lines.
///
/// The popup's body is read by a person rather than parsed by a terminal, so a
/// line break is kept — it is the whole point of a two-part message — while
/// every other control character is dropped by [`sanitize`], which is applied
/// per line. Empty lines disappear, so a message with no session title is one
/// line rather than one line and a blank one.
fn sanitize_lines(value: &str) -> String {
    value
        .split('\n')
        .map(sanitize)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The notification one backend event carries, if it carries one.
///
/// A live event names the session twice, and the client only treats the pair as
/// a committed item when they agree — the same guard the rest of the event
/// path uses before it believes a sequence.
fn notification_candidate(event: &BackendEvent) -> Option<(NotificationKind, String, String)> {
    match event {
        BackendEvent::Timeline(event)
            if event.sequence == event.item.sequence
                && event.session_id == event.item.session_id =>
        {
            notification_kind(&event.item.payload).map(|kind| {
                (
                    kind,
                    event.session_id.as_str().to_string(),
                    event.item.id.as_str().to_string(),
                )
            })
        }
        // The remote seat also receives the runtime's own intent. It names the
        // timeline item it came from, so the ledger recognises the pair.
        BackendEvent::Notification(intent) => {
            if intent.expires_at_ms <= vibex_core::unix_timestamp_ms() {
                return None;
            }
            Some((
                kind_for_intent(&intent.kind),
                intent.session_id.as_str().to_string(),
                intent.source_event_id.as_str().to_string(),
            ))
        }
        _ => None,
    }
}

/// The news in a timeline payload, when there is any.
fn notification_kind(payload: &TimelinePayload) -> Option<NotificationKind> {
    match payload {
        TimelinePayload::Error(_) => Some(NotificationKind::Failed),
        TimelinePayload::PermissionRequest(_) | TimelinePayload::ElicitationRequest(_) => {
            Some(NotificationKind::NeedsInput)
        }
        TimelinePayload::AgentMessage(message) if message.is_final => {
            Some(NotificationKind::Completed)
        }
        _ => None,
    }
}

/// The news in a runtime notification intent.
fn kind_for_intent(kind: &AgentNotificationKind) -> NotificationKind {
    match kind {
        AgentNotificationKind::ApprovalRequired { .. }
        | AgentNotificationKind::InputRequired { .. } => NotificationKind::NeedsInput,
        AgentNotificationKind::TurnFailed => NotificationKind::Failed,
        AgentNotificationKind::TurnCompleted => NotificationKind::Completed,
    }
}

impl App {
    /// Where the notification choices are written.
    ///
    /// Beside the arrangement, the switcher's memory and the interface's own
    /// settings, for the same reason: one home holds everything this client
    /// remembers.
    pub fn notification_preferences_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_NOTIFY_FILE")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        Some(Self::interface_home()?.join("tui-notify.json"))
    }

    /// Note a backend event for out-of-band notification.
    ///
    /// Called before the event controller sees the event, which is deliberate:
    /// the controller drops a background session's events as stale, and a
    /// background session is exactly the one the reader has to be told about.
    pub fn observe_notification_event(&mut self, event: &BackendEvent) {
        let Some((kind, session_id, source_event_id)) = notification_candidate(event) else {
            return;
        };
        let selected = self.selected_session_id().map(|id| id.as_str().to_string());
        let context = NotifyContext {
            selected_session_id: selected.as_deref(),
            session_page: self.page_shows_session(),
        };
        if !self
            .notify
            .accept(kind, &session_id, &source_event_id, &context)
        {
            return;
        }
        let message = self.notification_message(kind, &session_id);
        let app = self.strings.app_name();
        self.notify.emit(&session_id, app, &message);
    }

    /// Note whether the terminal currently has focus.
    pub fn set_terminal_focused(&mut self, focused: bool) {
        self.notify.set_focus(focused);
    }

    /// Take the terminal escapes the notifier produced, if any.
    pub fn take_terminal_notifications(&mut self) -> Vec<u8> {
        self.notify.take_pending()
    }

    /// Take the popups the run loop has to hand to the host, if any.
    pub fn take_system_notifications(&mut self) -> Vec<SystemNotification> {
        self.notify.take_pending_system()
    }

    /// Raise the reader's own test notification, reporting whether any channel
    /// took it.
    pub fn test_notification(&mut self) -> bool {
        let app = self.strings.app_name();
        let body = self.strings.settings_notify_test_body();
        self.notify.emit_test(app, body)
    }

    /// What the desktop-popup row should say.
    ///
    /// The state is the one the reader can act on, not the stored preference: a
    /// desktop on the other end that draws its own, a remote seat, and a host
    /// with no program all leave the toggle inert, and saying so is better than
    /// letting the reader flip a switch nothing reads.
    pub fn notification_system_state(&self) -> NotifySystemState {
        if self.notify.desktop_takes_over() {
            return NotifySystemState::Desktop;
        }
        if self.notify.remote() {
            return NotifySystemState::Remote;
        }
        if self.notify.delivery().is_none() {
            return NotifySystemState::Unsupported;
        }
        if self.notify.system_popup() {
            NotifySystemState::On
        } else {
            NotifySystemState::Off
        }
    }

    /// Take every control sequence the last frame owes the terminal.
    ///
    /// One door for the run loop to write through, whatever produced the
    /// bytes: a notification raised by an event, or a picture a frame placed —
    /// the chip preview's, or the pairing page's QR code. All of them have to
    /// leave after the frame rather than inside it, and none is the frame's own
    /// business.
    pub fn take_terminal_control(&mut self) -> Vec<u8> {
        let mut control = self.take_terminal_notifications();
        control.extend_from_slice(&self.picture.take_escapes());
        control.extend_from_slice(&self.pairing_qr.take_escapes());
        control.extend_from_slice(&self.row_marks.take_escapes());
        control
    }

    /// Forget every picture the terminal was holding.
    ///
    /// The other half of [`App::take_terminal_control`]: that door carries what
    /// a frame wants *put* on the terminal, this one is for the moments the
    /// terminal has already taken everything down itself. A resize clears the
    /// screen, the clear takes the pictures with it — a terminal that draws
    /// images for itself drops them when the screen is cleared, which is what
    /// kitty's own protocol rules require — and leaving the alternate screen
    /// drops them the same way. This client's record of what is placed is what
    /// would stop the next frame from sending a mark again, so the reader sees
    /// a list whose logos are missing until each row happens to scroll out of
    /// the window and back in. Forgetting is cheap and sending is not: every
    /// picture goes out once, on the frame after this.
    ///
    /// One door for the same reason the escapes have one: three surfaces own a
    /// picture, and a caller that forgot only the one it was thinking about
    /// would leave the others blank.
    pub fn forget_terminal_pictures(&mut self) {
        self.picture.forget();
        self.pairing_qr.forget();
        self.row_marks.forget();
    }

    /// The message one notification says.
    ///
    /// The parts are handed over separately rather than joined here: the Agent's
    /// name is the reader's only way to tell one session from another, the
    /// sentence is the runtime's kind localized, and the session title rides only
    /// when the reader asked for it — and each channel decides how much of that
    /// fits on a line.
    fn notification_message(
        &self,
        kind: NotificationKind,
        session_id: &str,
    ) -> NotificationMessage {
        let sentence = match kind {
            NotificationKind::Failed => self.strings.notify_turn_failed(),
            NotificationKind::NeedsInput => self.strings.notify_needs_input(),
            NotificationKind::Completed => self.strings.notify_turn_completed(),
        };
        let session = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|session| session.id.as_str() == session_id);
        let Some(session) = session else {
            return NotificationMessage {
                agent: None,
                sentence: sentence.to_string(),
                session_title: None,
            };
        };
        let session_title = session.title.trim();
        NotificationMessage {
            agent: Some(self.session_agent_label(session)),
            sentence: sentence.to_string(),
            session_title: (self.notify.includes_session_title() && !session_title.is_empty())
                .then(|| session_title.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentNotificationKind, TimelineItemId};

    fn profile(transport: Transport, multiplexer: Option<Multiplexer>) -> TerminalProfile {
        TerminalProfile {
            transport,
            multiplexer,
            terminal: None,
            remote: false,
        }
    }

    /// A notifier with no popup program, so a test's environment cannot decide
    /// what it does. The popup has tests of its own below.
    fn built(preferences: NotifyPreferences, profile: TerminalProfile) -> Notifier {
        Notifier::build(preferences, profile, None, None)
    }

    fn notifier(transport: Transport) -> Notifier {
        built(
            NotifyPreferences {
                transport: Some(transport.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(transport, None),
        )
    }

    fn context<'a>(selected: Option<&'a str>, session_page: bool) -> NotifyContext<'a> {
        NotifyContext {
            selected_session_id: selected,
            session_page,
        }
    }

    /// A message as an Agent event produces one.
    fn message(agent: &str, sentence: &str) -> NotificationMessage {
        NotificationMessage {
            agent: Some(agent.to_string()),
            sentence: sentence.to_string(),
            session_title: None,
        }
    }

    /// A message that carries the session's own title as well.
    fn titled(agent: &str, sentence: &str, title: &str) -> NotificationMessage {
        NotificationMessage {
            session_title: Some(title.to_string()),
            ..message(agent, sentence)
        }
    }

    #[test]
    fn an_ssh_session_is_read_as_a_remote_seat() {
        for variable in ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"] {
            let remote = TerminalProfile::detect_from(|key| match key {
                _ if key == variable => Some("10.0.0.1 5000 10.0.0.2 22".to_string()),
                _ => None,
            });
            assert!(remote.remote, "{variable} did not mark the seat remote");
        }
        let local = TerminalProfile::detect_from(|key| match key {
            "TERM" => Some("xterm-256color".to_string()),
            _ => None,
        });
        assert!(!local.remote);
    }

    #[test]
    fn the_popup_program_follows_the_platform() {
        assert_eq!(
            SystemDelivery::detect_with(|program| program == "notify-send"),
            Some(SystemDelivery::NotifySend)
        );
        assert_eq!(SystemDelivery::detect_with(|_| false), None);
        // macOS prefers the program the system ships, even where libnotify was
        // installed beside it.
        let both = SystemDelivery::detect_with(|_| true);
        if cfg!(target_os = "macos") {
            assert_eq!(both, Some(SystemDelivery::OsaScript));
        } else {
            assert_eq!(both, Some(SystemDelivery::NotifySend));
        }
    }

    #[test]
    fn a_popup_is_queued_only_for_a_local_seat_that_asked_for_one() {
        let preferences = NotifyPreferences {
            transport: Some(Transport::Off.id().to_string()),
            system_popup: Some(true),
            ..NotifyPreferences::default()
        };
        let mut local = Notifier::build(
            preferences.clone(),
            profile(Transport::Off, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(local.system_available());
        // The terminal channel is off, so the popup alone is reason enough to
        // announce: the two channels are independent.
        local.set_focus(false);
        assert!(local.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        local.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert!(local.take_pending().is_empty());
        assert_eq!(
            local.take_pending_system(),
            vec![SystemNotification {
                title: "Vibex · Codex".to_string(),
                body: "Agent failed".to_string(),
            }]
        );

        // A seat reached over ssh: the bubble would be drawn on the cloud host.
        let mut remote_profile = profile(Transport::Off, None);
        remote_profile.remote = true;
        let mut remote = Notifier::build(
            preferences.clone(),
            remote_profile,
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(!remote.system_available());
        remote.set_focus(false);
        assert!(!remote.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        remote.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert!(remote.take_pending_system().is_empty());

        // A host with no program to draw it: the preference is not a promise.
        let mut bare = Notifier::build(
            preferences.clone(),
            profile(Transport::Off, None),
            None,
            None,
        );
        assert!(!bare.system_available());
        bare.set_focus(false);
        bare.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert!(bare.take_pending_system().is_empty());

        // And the reader's own switch, off.
        let declined = Notifier::build(
            NotifyPreferences {
                system_popup: Some(false),
                ..preferences
            },
            profile(Transport::Off, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(!declined.system_available());
    }

    #[test]
    fn a_bubble_is_laid_out_in_lines_and_keeps_no_control_bytes() {
        let mut notifier = Notifier::build(
            NotifyPreferences {
                transport: Some(Transport::Off.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Off, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        // The Agent names the summary, the event is the first body line, and the
        // session is the second: the run-on sentence this replaced was one line
        // the reader had to parse.
        notifier.set_focus(false);
        notifier.emit(
            "s1",
            "Vibex",
            &titled("Codex", "Agent failed", "Fix the flaky test"),
        );
        let pending = notifier.take_pending_system();
        assert_eq!(pending[0].title, "Vibex · Codex");
        assert_eq!(pending[0].body, "Agent failed\nFix the flaky test");

        // Everything a payload could use to invent a line of its own is still
        // dropped, and a blank title leaves one line rather than two.
        notifier.emit("s1", "Vibex", &titled("Codex", "line\nbreak\x07", "  "));
        let pending = notifier.take_pending_system();
        assert_eq!(pending[0].body, "line break");
    }

    #[test]
    fn the_osascript_payload_quotes_what_it_carries() {
        let command = SystemDelivery::OsaScript.command("Vibex", "he said \"hi\" \\ bye");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec![
                "-e".to_string(),
                "display notification \"he said \\\"hi\\\" \\\\ bye\" with title \"Vibex\""
                    .to_string(),
            ]
        );
    }

    #[test]
    fn osascript_joins_the_lines_of_a_multi_line_body() {
        // AppleScript has no escape for a newline inside a literal, so the body
        // arrives as two literals joined with the language's own `linefeed`.
        let command = SystemDelivery::OsaScript.command("Vibex · Codex", "Agent failed\nFix it");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args[1],
            "display notification \"Agent failed\" & linefeed & \"Fix it\" \
             with title \"Vibex · Codex\""
        );
    }

    #[test]
    fn the_settings_page_edits_the_same_choices_the_file_holds() {
        let mut notifier = built(NotifyPreferences::default(), profile(Transport::Bell, None));
        assert!(notifier.enabled());
        assert!(notifier.kind_enabled(NotificationKind::Failed));
        assert_eq!(notifier.transport_preference(), None);
        assert_eq!(notifier.detected_transport(), Transport::Bell);
        assert!(!notifier.includes_session_title());
        assert!(notifier.system_popup());

        notifier.set_enabled(false);
        notifier.set_kind_enabled(NotificationKind::Failed, false);
        notifier.set_kind_enabled(NotificationKind::NeedsInput, false);
        notifier.set_kind_enabled(NotificationKind::Completed, false);
        notifier.set_transport(Some(Transport::Osc9));
        notifier.set_include_session_title(true);
        notifier.set_system_popup(false);

        let written = serde_json::to_value(&notifier.preferences).unwrap();
        assert_eq!(written["enabled"], serde_json::json!(false));
        assert_eq!(written["notifyAgentFailed"], serde_json::json!(false));
        assert_eq!(written["notifyAgentNeedsInput"], serde_json::json!(false));
        assert_eq!(written["notifyAgentCompleted"], serde_json::json!(false));
        assert_eq!(written["transport"], serde_json::json!("osc9"));
        assert_eq!(written["includeSessionTitle"], serde_json::json!(true));
        assert_eq!(written["systemPopup"], serde_json::json!(false));

        // Handing the choice back to detection clears the stored spelling.
        notifier.set_transport(None);
        assert_eq!(notifier.transport(), Transport::Bell);
        assert_eq!(notifier.transport_preference(), None);
        assert!(notifier.preferences.transport.is_none());
    }

    #[test]
    fn a_test_notification_takes_every_live_channel() {
        let mut both = Notifier::build(
            NotifyPreferences {
                transport: Some(Transport::Bell.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Bell, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(both.emit_test("Vibex", "Test"));
        assert_eq!(both.take_pending(), b"\x07".to_vec());
        assert_eq!(both.take_pending_system().len(), 1);

        // With the layer off there is nothing to test, and the settings page
        // says so rather than writing a bell nobody asked for.
        let mut off = built(
            NotifyPreferences {
                enabled: Some(false),
                transport: Some(Transport::Bell.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Bell, None),
        );
        assert!(!off.emit_test("Vibex", "Test"));
        assert!(off.take_pending().is_empty());
        assert!(off.take_pending_system().is_empty());
    }

    #[test]
    fn the_system_state_reports_what_the_reader_can_act_on() {
        let local = |preference: bool| {
            Notifier::build(
                NotifyPreferences {
                    system_popup: Some(preference),
                    ..NotifyPreferences::default()
                },
                profile(Transport::Bell, None),
                Some(SystemDelivery::NotifySend),
                None,
            )
        };
        assert!(local(true).system_available());
        assert!(!local(false).system_available());

        let mut remote_profile = profile(Transport::Bell, None);
        remote_profile.remote = true;
        let remote = Notifier::build(
            NotifyPreferences::default(),
            remote_profile,
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(remote.remote());
        assert!(!remote.system_available());
    }

    #[test]
    fn one_event_is_one_reminder() {
        // Both channels are live, and the reader is at the terminal: the escape
        // is the one that speaks, because the terminal is what they are looking
        // at. The popup would be the same news beside it.
        let mut at_the_terminal = Notifier::build(
            NotifyPreferences::default(),
            profile(Transport::Bell, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        at_the_terminal.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(at_the_terminal.take_pending(), b"\x07".to_vec());
        assert!(at_the_terminal.take_pending_system().is_empty());

        // The reader has switched away: the bubble reaches them, and the bell
        // would only ring in a window they are not looking at.
        let mut away = Notifier::build(
            NotifyPreferences::default(),
            profile(Transport::Bell, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        away.set_focus(false);
        assert!(away.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        away.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert!(away.take_pending().is_empty());
        assert_eq!(away.take_pending_system().len(), 1);

        // No popup to draw — a remote seat, or a host with no program — so the
        // escape carries the news in both focus states, which is what keeps an
        // ssh reader informed.
        for focused in [true, false] {
            let mut remote_profile = profile(Transport::Bell, None);
            remote_profile.remote = true;
            let mut notifier = Notifier::build(
                NotifyPreferences::default(),
                remote_profile,
                Some(SystemDelivery::NotifySend),
                None,
            );
            notifier.set_focus(focused);
            notifier.emit("s1", "Vibex", &message("Codex", "Agent failed"));
            assert_eq!(notifier.take_pending(), b"\x07".to_vec());
            assert!(notifier.take_pending_system().is_empty());
        }
    }

    #[test]
    fn an_off_terminal_channel_is_not_announced_into_a_popup_that_will_not_use_it() {
        // The reader turned the terminal channel off and left the popup on. A
        // focused event has no channel at all — the popup only speaks to a
        // reader who is away — so it is not announced rather than announced
        // into nothing.
        let mut notifier = Notifier::build(
            NotifyPreferences {
                transport: Some(Transport::Off.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Off, None),
            Some(SystemDelivery::NotifySend),
            None,
        );
        assert!(!notifier.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        notifier.set_focus(false);
        assert!(notifier.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
    }

    #[test]
    fn a_desktop_on_the_other_end_gets_the_light_treatment() {
        let preferences = NotifyPreferences {
            transport: Some(Transport::Osc9.id().to_string()),
            ..NotifyPreferences::default()
        };
        let notifier = built(preferences.clone(), profile(Transport::Osc9, None))
            .with_runtime_kind(vibex_core::RemoteServerKind::Desktop);
        assert!(notifier.light_when_desktop());
        assert!(notifier.desktop_takes_over());
        // The channel the reader named is pinned to the lightest one there is,
        // and the popup is the desktop's.
        assert_eq!(notifier.transport(), Transport::Title);
        assert!(!notifier.system_available());

        // A headless runtime, or an authority seat that never named a desktop,
        // leaves both choices exactly as they were.
        let mut headless = built(preferences.clone(), profile(Transport::Osc9, None))
            .with_runtime_kind(vibex_core::RemoteServerKind::Headless);
        assert!(!headless.desktop_takes_over());
        assert_eq!(headless.transport(), Transport::Osc9);

        // The reader can hand the channels back to this client.
        headless.set_desktop_present(true);
        headless.set_light_when_desktop(false);
        assert!(!headless.desktop_takes_over());
        assert_eq!(headless.transport(), Transport::Osc9);
        assert_eq!(headless.preferences.light_when_desktop, Some(false));

        // "Light" is not "louder than what I asked for": a reader who turned the
        // terminal channel off keeps it off while the desktop speaks.
        let off = built(
            NotifyPreferences {
                transport: Some(Transport::Off.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Off, None),
        )
        .with_runtime_kind(vibex_core::RemoteServerKind::Desktop);
        assert_eq!(off.transport(), Transport::Off);
    }

    #[test]
    fn the_desktop_switch_is_written_down_with_the_others() {
        let mut notifier = built(NotifyPreferences::default(), profile(Transport::Bell, None));
        assert!(notifier.light_when_desktop());
        notifier.set_light_when_desktop(false);
        let written = serde_json::to_value(&notifier.preferences).unwrap();
        assert_eq!(written["lightWhenDesktop"], serde_json::json!(false));
    }

    #[test]
    fn detection_picks_the_sequence_the_terminal_implements() {
        let kitty = TerminalProfile::detect_from(|key| match key {
            "KITTY_WINDOW_ID" => Some("1".to_string()),
            "TERM" => Some("xterm-kitty".to_string()),
            _ => None,
        });
        assert_eq!(kitty.transport, Transport::Osc99);
        assert_eq!(kitty.terminal.as_deref(), Some("kitty"));
        assert_eq!(kitty.multiplexer, None);

        let iterm = TerminalProfile::detect_from(|key| match key {
            "TERM_PROGRAM" => Some("iTerm.app".to_string()),
            _ => None,
        });
        assert_eq!(iterm.transport, Transport::Osc9);

        let windows = TerminalProfile::detect_from(|key| match key {
            "WT_SESSION" => Some("abc".to_string()),
            "TERM" => Some("xterm-256color".to_string()),
            _ => None,
        });
        assert_eq!(windows.transport, Transport::Osc9);
        assert_eq!(windows.terminal.as_deref(), Some("Windows Terminal"));

        let unknown = TerminalProfile::detect_from(|key| match key {
            "TERM" => Some("xterm-256color".to_string()),
            _ => None,
        });
        assert_eq!(unknown.transport, Transport::Title);
        assert_eq!(unknown.terminal, None);
    }

    #[test]
    fn a_multiplexer_defaults_to_the_bell_it_can_always_forward() {
        let tmux = TerminalProfile::detect_from(|key| match key {
            "TMUX" => Some("/tmp/tmux-1000/default,1,0".to_string()),
            "KITTY_WINDOW_ID" => Some("1".to_string()),
            _ => None,
        });
        assert_eq!(tmux.multiplexer, Some(Multiplexer::Tmux));
        assert_eq!(tmux.transport, Transport::Bell);
        // The outer terminal is still named, because the reader may override
        // the transport and then wants to know what the passthrough is for.
        assert_eq!(tmux.terminal.as_deref(), Some("kitty"));

        let screen = TerminalProfile::detect_from(|key| match key {
            "STY" => Some("1234.pts-0".to_string()),
            _ => None,
        });
        assert_eq!(screen.multiplexer, Some(Multiplexer::Screen));
        assert_eq!(screen.transport, Transport::Bell);
    }

    #[test]
    fn an_unknown_transport_id_falls_back_to_detection() {
        let preferences = NotifyPreferences {
            transport: Some("carrier-pigeon".to_string()),
            ..NotifyPreferences::default()
        };
        let notifier = built(preferences, profile(Transport::Osc9, None));
        assert_eq!(notifier.transport(), Transport::Osc9);
    }

    #[test]
    fn a_reader_at_the_session_is_not_interrupted() {
        let mut notifier = notifier(Transport::Bell);
        notifier.set_focus(true);
        assert!(!notifier.accept(
            NotificationKind::Failed,
            "s1",
            "e1",
            &context(Some("s1"), true)
        ));
        // The same failure is news once the reader is on another page.
        assert!(notifier.accept(
            NotificationKind::Failed,
            "s1",
            "e1",
            &context(Some("s1"), false)
        ));
        // And for a session that is not the one on screen.
        assert!(notifier.accept(
            NotificationKind::Failed,
            "s2",
            "e2",
            &context(Some("s1"), true)
        ));
    }

    #[test]
    fn a_focus_lost_reader_is_told_about_the_session_in_front() {
        let mut notifier = notifier(Transport::Bell);
        notifier.set_focus(false);
        assert!(notifier.accept(
            NotificationKind::NeedsInput,
            "s1",
            "e1",
            &context(Some("s1"), true)
        ));
    }

    #[test]
    fn the_same_event_is_announced_once() {
        let mut notifier = notifier(Transport::Bell);
        notifier.set_focus(false);
        assert!(notifier.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        // The live stream and the polling fallback (or the intent beside the
        // timeline item) both spell the same source id.
        assert!(!notifier.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        assert!(notifier.accept(NotificationKind::Failed, "s1", "e2", &context(None, false)));
    }

    #[test]
    fn switched_off_kinds_and_the_off_transport_stay_silent() {
        let mut completed = built(
            NotifyPreferences {
                notify_agent_completed: Some(false),
                ..NotifyPreferences::default()
            },
            profile(Transport::Bell, None),
        );
        assert!(!completed.accept(
            NotificationKind::Completed,
            "s1",
            "e1",
            &context(None, false)
        ));
        assert!(completed.accept(NotificationKind::Failed, "s1", "e2", &context(None, false)));

        let mut off = notifier(Transport::Off);
        assert!(!off.accept(NotificationKind::Failed, "s1", "e1", &context(None, false)));
        off.emit("s1", "Vibex", &message("Codex", "nope"));
        assert!(off.take_pending().is_empty());
    }

    #[test]
    fn each_transport_writes_its_own_sequence() {
        let mut bell = notifier(Transport::Bell);
        bell.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(bell.take_pending(), b"\x07".to_vec());

        let mut title = notifier(Transport::Title);
        title.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(
            String::from_utf8(title.take_pending()).unwrap(),
            "\x1b]2;● Codex · Agent failed\x07"
        );

        let mut osc9 = notifier(Transport::Osc9);
        osc9.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(
            String::from_utf8(osc9.take_pending()).unwrap(),
            "\x1b]9;Codex · Agent failed\x07"
        );

        let mut osc777 = notifier(Transport::Osc777);
        osc777.emit("s1", "Vibex", &message("Codex", "Agent; failed"));
        assert_eq!(
            String::from_utf8(osc777.take_pending()).unwrap(),
            "\x1b]777;notify;Vibex;Codex · Agent, failed\x07"
        );

        let mut osc99 = notifier(Transport::Osc99);
        osc99.emit("s 1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(
            String::from_utf8(osc99.take_pending()).unwrap(),
            "\x1b]99;i=s1;Codex · Agent failed\x07"
        );

        let mut iterm = notifier(Transport::Iterm2Attention);
        iterm.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(
            String::from_utf8(iterm.take_pending()).unwrap(),
            "\x1b]1337;RequestAttention=yes\x07"
        );
    }

    #[test]
    fn tmux_gets_the_passthrough_envelope_with_doubled_escapes() {
        let mut notifier = built(
            NotifyPreferences {
                transport: Some(Transport::Osc9.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Osc9, Some(Multiplexer::Tmux)),
        );
        notifier.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(
            String::from_utf8(notifier.take_pending()).unwrap(),
            "\x1bPtmux;\x1b\x1b]9;Codex · Agent failed\x07\x1b\\"
        );
    }

    #[test]
    fn the_bell_is_not_wrapped_for_tmux() {
        let mut notifier = built(
            NotifyPreferences::default(),
            profile(Transport::Bell, Some(Multiplexer::Tmux)),
        );
        notifier.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert_eq!(notifier.take_pending(), b"\x07".to_vec());
    }

    #[test]
    fn a_marked_title_is_cleared_when_the_terminal_comes_back() {
        let mut notifier = notifier(Transport::Title);
        notifier.set_focus(false);
        notifier.emit("s1", "Vibex", &message("Codex", "Agent failed"));
        assert!(!notifier.take_pending().is_empty());
        notifier.set_focus(true);
        assert_eq!(
            String::from_utf8(notifier.take_pending()).unwrap(),
            "\x1b]2;Vibex\x07"
        );
    }

    #[test]
    fn a_payload_cannot_carry_control_bytes_of_its_own() {
        assert_eq!(sanitize("a\nb\tc"), "a b c");
        assert_eq!(sanitize("\x1b]2;owned\x07"), "]2;owned");
        assert_eq!(sanitize("  spaced   out  "), "spaced out");
        assert!(sanitize(&"x".repeat(MAX_PAYLOAD_CHARS * 2)).chars().count() <= MAX_PAYLOAD_CHARS);
    }

    #[test]
    fn a_timeline_error_names_the_session_and_item_it_came_from() {
        let session_id = vibex_core::VibexSessionId::new();
        let item = timeline_item(&session_id, TimelinePayload::Error(error_payload("boom")));
        let expected_item = item.id.as_str().to_string();
        let event = vibex_backend::BackendEvent::Timeline(vibex_core::TimelineLiveEvent {
            session_id: session_id.clone(),
            sequence: item.sequence,
            item,
        });
        let (kind, session, source) = notification_candidate(&event).unwrap();
        assert_eq!(kind, NotificationKind::Failed);
        assert_eq!(session, session_id.as_str());
        assert_eq!(source, expected_item);
    }

    #[test]
    fn a_waiting_request_is_news_and_a_delta_is_not() {
        let final_message = TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: "done".to_string(),
            is_final: true,
        });
        assert_eq!(
            notification_kind(&final_message),
            Some(NotificationKind::Completed)
        );
        let delta = TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: "still going".to_string(),
            is_final: false,
        });
        assert_eq!(notification_kind(&delta), None);
    }

    #[test]
    fn an_uncommitted_timeline_pair_is_not_believed() {
        let session_id = vibex_core::VibexSessionId::new();
        let item = timeline_item(&session_id, TimelinePayload::Error(error_payload("boom")));
        let event = vibex_backend::BackendEvent::Timeline(vibex_core::TimelineLiveEvent {
            session_id: session_id.clone(),
            sequence: item.sequence + 1,
            item,
        });
        assert!(notification_candidate(&event).is_none());
    }

    #[test]
    fn an_expired_intent_is_not_announced() {
        let session_id = vibex_core::VibexSessionId::new();
        let stale = vibex_core::AgentNotificationIntent {
            notification_id: "n1".to_string(),
            source_event_id: TimelineItemId::new(),
            session_id,
            kind: AgentNotificationKind::TurnFailed,
            created_at_ms: 0,
            expires_at_ms: vibex_core::unix_timestamp_ms() - 1,
            opaque_locator: String::new(),
        };
        assert!(
            notification_candidate(&vibex_backend::BackendEvent::Notification(stale)).is_none()
        );
    }

    #[test]
    fn an_intent_maps_to_the_same_kind_as_its_timeline_item() {
        assert_eq!(
            kind_for_intent(&AgentNotificationKind::TurnFailed),
            NotificationKind::Failed
        );
        assert_eq!(
            kind_for_intent(&AgentNotificationKind::TurnCompleted),
            NotificationKind::Completed
        );
        assert_eq!(
            kind_for_intent(&AgentNotificationKind::InputRequired {
                request_id: vibex_core::RequestId::new(),
            }),
            NotificationKind::NeedsInput
        );
    }

    fn error_payload(message: &str) -> vibex_core::TimelineErrorPayload {
        vibex_core::TimelineErrorPayload {
            code: "agent_failed".to_string(),
            message: message.to_string(),
            recoverable: false,
        }
    }

    fn timeline_item(
        session_id: &vibex_core::VibexSessionId,
        payload: TimelinePayload,
    ) -> vibex_core::TimelineItem {
        vibex_core::TimelineItem {
            id: TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: 7,
            timestamp_ms: 7,
            source: vibex_core::TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload,
        }
    }
}
