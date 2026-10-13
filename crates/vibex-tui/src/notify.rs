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
//! the machine the reader is actually sitting at. Nothing here starts a
//! process, opens a socket or talks to a display server, so the crate keeps its
//! "linkable without a window server" boundary.
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
//! # What is in the message
//!
//! Only what the runtime already publishes plus the Agent's name: a kind and a
//! session id. The Agent's prose never rides a notification, because a terminal
//! may log an OSC payload and a title is read by anyone glancing at the screen.
//! The session title is therefore opt-in (`includeSessionTitle`).

use std::collections::{HashSet, VecDeque};
use std::path::Path;

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
/// It owns no I/O: a decision and the bytes it produces are collected in
/// `pending`, and the run loop writes them after the frame. Keeping the writes
/// out of the reducer is what lets every rule here be a unit test.
#[derive(Debug)]
pub struct Notifier {
    enabled: bool,
    transport: Transport,
    multiplexer: Option<Multiplexer>,
    terminal: Option<String>,
    kind_switches: [bool; 3],
    include_session_title: bool,
    focused: bool,
    title_marked: bool,
    seen: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
    pending: Vec<u8>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    /// Read the saved choices and detect the terminal.
    pub fn new() -> Self {
        let preferences = NotifyPreferences::load(App::notification_preferences_path().as_deref());
        Self::from_preferences(preferences)
    }

    /// Build from already-read choices, detecting the terminal.
    pub fn from_preferences(preferences: NotifyPreferences) -> Self {
        let profile = TerminalProfile::detect();
        Self::from_parts(preferences, profile)
    }

    fn from_parts(preferences: NotifyPreferences, profile: TerminalProfile) -> Self {
        let transport = preferences.transport().unwrap_or(profile.transport);
        Self {
            enabled: preferences.is_enabled(),
            transport,
            multiplexer: profile.multiplexer,
            terminal: profile.terminal,
            kind_switches: [
                preferences.kind_enabled(NotificationKind::Completed),
                preferences.kind_enabled(NotificationKind::NeedsInput),
                preferences.kind_enabled(NotificationKind::Failed),
            ],
            include_session_title: preferences.includes_session_title(),
            // Before the terminal has told us otherwise, the reader is assumed
            // to be at it: the client was started from a keypress, and a
            // terminal that never reports focus should not turn every
            // on-screen failure into a bell.
            focused: true,
            title_marked: false,
            seen: HashSet::new(),
            order: VecDeque::new(),
            pending: Vec::new(),
        }
    }

    /// The transport this client will use.
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// The terminal the environment identified, when it identified one.
    pub fn terminal(&self) -> Option<&str> {
        self.terminal.as_deref()
    }

    /// Whether the session title may ride the payload.
    pub fn includes_session_title(&self) -> bool {
        self.include_session_title
    }

    /// Whether the terminal is currently the focused window.
    pub fn focused(&self) -> bool {
        self.focused
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
        if !self.enabled || self.transport == Transport::Off || !self.kind_switches[kind.index()] {
            return false;
        }
        if self.focused && context.session_page && context.selected_session_id == Some(session_id) {
            return false;
        }
        self.remember(session_id, source_event_id)
    }

    /// Build the sequence for one notification and queue it.
    pub fn emit(&mut self, session_id: &str, title: &str, body: &str) {
        let sequence = self.sequence(session_id, title, body);
        if sequence.is_empty() {
            return;
        }
        if self.transport == Transport::Title {
            self.title_marked = true;
        }
        self.pending.extend_from_slice(sequence.as_bytes());
    }

    /// Take the bytes the run loop has to write, if any.
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
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
    fn sequence(&self, session_id: &str, title: &str, body: &str) -> String {
        let title = sanitize(title);
        let body = sanitize(body);
        let inner = match self.transport {
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
                    title.replace(';', ","),
                    body.replace(';', ",")
                )
            }
            Transport::Osc99 => {
                format!("\x1b]99;i={};{title} · {body}\x07", identifier(session_id))
            }
            Transport::Iterm2Attention => "\x1b]1337;RequestAttention=yes\x07".to_string(),
        };
        match self.multiplexer {
            Some(Multiplexer::Tmux) if self.transport.is_osc() => tmux_passthrough(&inner),
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
        let body = self.notification_body(kind, &session_id);
        let title = self.strings.app_name();
        self.notify.emit(&session_id, title, &body);
    }

    /// Note whether the terminal currently has focus.
    pub fn set_terminal_focused(&mut self, focused: bool) {
        self.notify.set_focus(focused);
    }

    /// Take the terminal escapes the notifier produced, if any.
    pub fn take_terminal_notifications(&mut self) -> Vec<u8> {
        self.notify.take_pending()
    }

    /// Take every control sequence the last frame owes the terminal.
    ///
    /// One door for the run loop to write through, whatever produced the
    /// bytes: a notification raised by an event, or the picture the chip
    /// preview placed. Both have to leave after the frame rather than inside
    /// it, and neither is the frame's own business.
    pub fn take_terminal_control(&mut self) -> Vec<u8> {
        let mut control = self.take_terminal_notifications();
        control.extend_from_slice(&self.picture.take_escapes());
        control
    }

    /// The message one notification says.
    ///
    /// The Agent's name is the reader's only way to tell one session from
    /// another, so it leads; the sentence is the runtime's kind, localized. The
    /// session title rides only when the reader asked for it.
    fn notification_body(&self, kind: NotificationKind, session_id: &str) -> String {
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
            return sentence.to_string();
        };
        let mut body = format!("{} · {sentence}", self.session_agent_label(session));
        if self.notify.includes_session_title() && !session.title.trim().is_empty() {
            body.push_str(" · ");
            body.push_str(session.title.trim());
        }
        body
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
        }
    }

    fn notifier(transport: Transport) -> Notifier {
        Notifier::from_parts(
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
        let notifier = Notifier::from_parts(preferences, profile(Transport::Osc9, None));
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
        let mut completed = Notifier::from_parts(
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
        off.emit("s1", "Vibex", "nope");
        assert!(off.take_pending().is_empty());
    }

    #[test]
    fn each_transport_writes_its_own_sequence() {
        let mut bell = notifier(Transport::Bell);
        bell.emit("s1", "Vibex", "Agent failed");
        assert_eq!(bell.take_pending(), b"\x07".to_vec());

        let mut title = notifier(Transport::Title);
        title.emit("s1", "Vibex", "Agent failed");
        assert_eq!(
            String::from_utf8(title.take_pending()).unwrap(),
            "\x1b]2;● Agent failed\x07"
        );

        let mut osc9 = notifier(Transport::Osc9);
        osc9.emit("s1", "Vibex", "Agent failed");
        assert_eq!(
            String::from_utf8(osc9.take_pending()).unwrap(),
            "\x1b]9;Agent failed\x07"
        );

        let mut osc777 = notifier(Transport::Osc777);
        osc777.emit("s1", "Vibex", "Agent; failed");
        assert_eq!(
            String::from_utf8(osc777.take_pending()).unwrap(),
            "\x1b]777;notify;Vibex;Agent, failed\x07"
        );

        let mut osc99 = notifier(Transport::Osc99);
        osc99.emit("s 1", "Vibex", "Agent failed");
        assert_eq!(
            String::from_utf8(osc99.take_pending()).unwrap(),
            "\x1b]99;i=s1;Vibex · Agent failed\x07"
        );

        let mut iterm = notifier(Transport::Iterm2Attention);
        iterm.emit("s1", "Vibex", "Agent failed");
        assert_eq!(
            String::from_utf8(iterm.take_pending()).unwrap(),
            "\x1b]1337;RequestAttention=yes\x07"
        );
    }

    #[test]
    fn tmux_gets_the_passthrough_envelope_with_doubled_escapes() {
        let mut notifier = Notifier::from_parts(
            NotifyPreferences {
                transport: Some(Transport::Osc9.id().to_string()),
                ..NotifyPreferences::default()
            },
            profile(Transport::Osc9, Some(Multiplexer::Tmux)),
        );
        notifier.emit("s1", "Vibex", "Agent failed");
        assert_eq!(
            String::from_utf8(notifier.take_pending()).unwrap(),
            "\x1bPtmux;\x1b\x1b]9;Agent failed\x07\x1b\\"
        );
    }

    #[test]
    fn the_bell_is_not_wrapped_for_tmux() {
        let mut notifier = Notifier::from_parts(
            NotifyPreferences::default(),
            profile(Transport::Bell, Some(Multiplexer::Tmux)),
        );
        notifier.emit("s1", "Vibex", "Agent failed");
        assert_eq!(notifier.take_pending(), b"\x07".to_vec());
    }

    #[test]
    fn a_marked_title_is_cleared_when_the_terminal_comes_back() {
        let mut notifier = notifier(Transport::Title);
        notifier.set_focus(false);
        notifier.emit("s1", "Vibex", "Agent failed");
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
