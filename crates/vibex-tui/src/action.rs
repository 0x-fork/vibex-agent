//! Every user-triggered action the TUI can perform.
//!
//! The enum is the vocabulary shared by three producers — the keymap, the
//! command palette, and in-page widgets such as the approval card — and one
//! consumer, [`crate::app::App::perform`]. Keeping one vocabulary is what makes
//! "the key bar cannot drift from the handler" true by construction.
//!
//! Variants are grouped by domain. Each one declares the scope that owns it and
//! the help text shown for it, so a new action is a single edit rather than
//! four.

use crate::keymap::Scope;

macro_rules! intents {
    ($( $variant:ident => { scope: $scope:ident, id: $id:expr, label: $label:expr, help: $help:expr } ),* $(,)?) => {
        /// A user-triggered action.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Intent {
            $($variant),*
        }

        impl Intent {
            /// Every intent, in declaration order.
            pub const ALL: &'static [Intent] = &[ $( Intent::$variant ),* ];

            /// Stable identifier used by the key-remap file and by tests.
            pub const fn id(self) -> &'static str {
                match self { $( Intent::$variant => $id ),* }
            }

            /// Scope that owns the intent by default.
            pub const fn default_scope(self) -> Scope {
                match self { $( Intent::$variant => Scope::$scope ),* }
            }

            /// Short label shown in the key bar and the help panel.
            pub const fn default_label(self) -> &'static str {
                match self { $( Intent::$variant => $label ),* }
            }

            /// One-line explanation shown in `?` help.
            pub const fn help(self) -> &'static str {
                match self { $( Intent::$variant => $help ),* }
            }
        }
    };
}

intents! {
    // ---- global ---------------------------------------------------------
    OpenCommandPalette => { scope: Global, id: "command_palette", label: "Commands", help: "Fuzzy-search every page and action." },
    ToggleHelp => { scope: Global, id: "help", label: "Help", help: "Show the keys and concepts for whatever has focus right now." },
    OpenSettings => { scope: Global, id: "open_settings", label: "Settings", help: "Theme, language, icons, colour and key bindings." },
    RequestQuit => { scope: Global, id: "quit", label: "Quit", help: "Leave the TUI and restore the terminal. The palette is the only way to ask for it by name." },
    Back => { scope: Global, id: "back", label: "Back", help: "Close the overlay, then the panel, then the page. Never quits." },
    FocusNext => { scope: Global, id: "focus_next", label: "Next pane", help: "Move focus between sidebar, main area, details and composer." },
    FocusPrevious => { scope: Global, id: "focus_previous", label: "Previous pane", help: "Move focus backwards." },
    Refresh => { scope: Global, id: "refresh", label: "Refresh", help: "Re-read the current page from the runtime." },
    ContextualCancel => { scope: Global, id: "cancel", label: "Cancel", help: "Clear the input, else interrupt the running turn, else offer to quit." },
    GotoSessions => { scope: Global, id: "goto_sessions", label: "Sessions", help: "Jump to the session list." },
    GotoManagement => { scope: Global, id: "goto_management", label: "Management", help: "Jump to the management index." },
    ToggleSidebar => { scope: Global, id: "toggle_sidebar", label: "Sidebar", help: "Collapse or show the sidebar." },
    ReloadKeymap => { scope: Global, id: "reload_keymap", label: "Reload keys", help: "Re-read ~/.vibex/tui-keys.toml." },

    // ---- list motion shared by every page --------------------------------
    SelectPrevious => { scope: Global, id: "select_previous", label: "Previous", help: "Move the selection up." },
    SelectNext => { scope: Global, id: "select_next", label: "Next", help: "Move the selection down." },
    ScrollPageUp => { scope: Global, id: "scroll_page_up", label: "Page up", help: "Scroll one page up." },
    ScrollPageDown => { scope: Global, id: "scroll_page_down", label: "Page down", help: "Scroll one page down." },
    ScrollHalfPageUp => { scope: Global, id: "scroll_half_up", label: "Half up", help: "Scroll half a page up." },
    ScrollHalfPageDown => { scope: Global, id: "scroll_half_down", label: "Half down", help: "Scroll half a page down." },
    ScrollToTop => { scope: Global, id: "scroll_top", label: "Top", help: "Jump to the first item." },
    ScrollToBottom => { scope: Global, id: "scroll_bottom", label: "Bottom", help: "Jump to the last item and resume following." },
    ShowDetails => { scope: Global, id: "show_details", label: "Details", help: "Open the details panel for the selection." },

    // ---- sessions -------------------------------------------------------
    OpenSelectedSession => { scope: Sessions, id: "session_open", label: "Open", help: "Open the selected session." },
    EnterSession => { scope: Sessions, id: "session_enter", label: "Enter", help: "Move into the session's workbench." },
    NewSession => { scope: Sessions, id: "session_new", label: "New", help: "Create a session in a workspace you pick." },
    BeginRenameSession => { scope: Sessions, id: "session_rename", label: "Rename", help: "Rename the selected session." },
    ForkSession => { scope: Sessions, id: "session_fork", label: "Fork", help: "Copy the session up to a chosen point." },
    ArchiveSession => { scope: Sessions, id: "session_archive", label: "Archive", help: "Archive the selected session, or restore it when it is already archived." },
    UnarchiveSession => { scope: Sessions, id: "session_unarchive", label: "Restore", help: "Restore the selected archived session to the list." },
    DeleteSession => { scope: Sessions, id: "session_delete", label: "Delete", help: "Permanently delete the selected session." },
    ToggleShowArchived => { scope: Sessions, id: "session_show_archived", label: "Archived", help: "Include archived sessions in the list." },
    ToggleSessionCard => { scope: Sessions, id: "session_card_toggle", label: "Details", help: "Open or close the selected session's detail card." },
    CollapseSessionCards => { scope: Sessions, id: "session_card_collapse", label: "Close cards", help: "Close every open session detail card." },
    ToggleChildBranch => { scope: Sessions, id: "session_children_toggle", label: "Sub-sessions", help: "Fold or unfold the selected session's delegated sub-sessions." },
    CopySessionRow => { scope: Sessions, id: "session_copy", label: "Copy", help: "Copy the selected session's details to the clipboard." },
    PinSession => { scope: Sessions, id: "session_pin", label: "Pin", help: "Keep the selected session above the rest of the list." },
    ToggleAutoContinue => { scope: Sessions, id: "session_auto_continue", label: "Auto-continue", help: "Let the selected session continue itself when a turn stops without an answer." },
    MoveSessionUp => { scope: Sessions, id: "session_move_up", label: "Move up", help: "Move the selected session one place earlier in the list." },
    MoveSessionDown => { scope: Sessions, id: "session_move_down", label: "Move down", help: "Move the selected session one place later in the list." },
    SwitchWorkspace => { scope: Sessions, id: "workspace_switch", label: "Workspace", help: "Change the workspace used for new sessions." },
    OpenWorkspaceBrowser => { scope: Sessions, id: "workspace_browse", label: "Browse", help: "List directories on the authority host." },
    WorkspaceBrowseUp => { scope: Sessions, id: "workspace_up", label: "Parent", help: "Go to the parent directory." },
    WorkspaceBrowseSelect => { scope: Sessions, id: "workspace_select", label: "Use directory", help: "Work in the highlighted directory, else the one the picker is showing." },

    // ---- agent transcript -----------------------------------------------
    FocusComposer => { scope: Agent, id: "composer_focus", label: "Write", help: "Put the cursor in the message composer." },
    ContinueTurn => { scope: Agent, id: "turn_continue", label: "Continue", help: "Ask the Agent to continue an unfinished turn." },
    ToggleBlockExpanded => { scope: Agent, id: "block_toggle", label: "Expand", help: "Expand or collapse the selected transcript block." },
    ToggleAllBlocksExpanded => { scope: Agent, id: "block_toggle_all", label: "Expand all", help: "Expand or collapse every collapsible block." },
    ToggleReasoningExpanded => { scope: Agent, id: "reasoning_toggle", label: "Reasoning", help: "Show or hide the Agent's reasoning." },
    CopyBlockBody => { scope: Agent, id: "block_copy", label: "Copy", help: "Copy the selected block's body to the clipboard." },
    CopyBlockMetadata => { scope: Agent, id: "block_copy_meta", label: "Copy meta", help: "Copy the command, path or id attached to the block." },
    OpenBlockDetails => { scope: Agent, id: "block_details", label: "Details", help: "Open the diff, terminal output or full text for the block." },
    SwitchAgentRuntime => { scope: Agent, id: "runtime_switch", label: "Runtime", help: "Choose the Agent, its model and the run options it publishes." },
    ProbeAgentRuntime => { scope: Agent, id: "runtime_probe", label: "Probe", help: "Ask the runtime to re-discover available Agents." },
    BeginTranscriptSearch => { scope: Agent, id: "transcript_search", label: "Find", help: "Search the transcript with a regular expression." },
    ToggleDock => { scope: Agent, id: "dock_toggle", label: "Dock", help: "Show or hide the panel of running agents, the plan and held messages." },
    DockActivate => { scope: Agent, id: "dock_activate", label: "Dock open", help: "Open the highlighted dock row: jump to it, or fold its section." },
    OpenChildSessions => { scope: Agent, id: "session_children_open", label: "Sub-sessions", help: "Open the sub-session the open one delegated, or pick from the dock when it delegated several." },
    GotoParentSession => { scope: Agent, id: "session_parent", label: "Main session", help: "Go back to the session that delegated this one." },
    DockHideDone => { scope: Agent, id: "dock_hide_done", label: "Hide done", help: "Hide finished agents and completed plan steps in the dock." },
    LoadOlderHistory => { scope: Agent, id: "history_older", label: "Older history", help: "Fetch the page of history above the oldest loaded one." },
    QueueSelectPrevious => { scope: Agent, id: "queue_previous", label: "Queue up", help: "Move the queue cursor to the message above." },
    QueueSelectNext => { scope: Agent, id: "queue_next", label: "Queue down", help: "Move the queue cursor to the message below." },
    QueueEditSelected => { scope: Agent, id: "queue_edit", label: "Queue edit", help: "Pull the queued message back into the composer." },
    QueueDeleteSelected => { scope: Agent, id: "queue_delete", label: "Queue drop", help: "Remove the queued message." },
    QueueMoveUp => { scope: Agent, id: "queue_move_up", label: "Queue raise", help: "Send the queued message one turn earlier." },
    QueueMoveDown => { scope: Agent, id: "queue_move_down", label: "Queue lower", help: "Send the queued message one turn later." },
    QueueSendNow => { scope: Agent, id: "queue_send_now", label: "Send now", help: "Interrupt the turn and send the queued message immediately." },
    ScheduleMessage => { scope: Agent, id: "message_schedule", label: "Schedule", help: "Give the draft a send time; until the clock reaches it the message waits in the queue." },
    SearchNext => { scope: Agent, id: "search_next", label: "Next match", help: "Jump to the next match, wrapping at the end." },
    SearchPrevious => { scope: Agent, id: "search_previous", label: "Prev match", help: "Jump to the previous match, wrapping at the start." },

    // ---- composer -------------------------------------------------------
    SubmitComposer => { scope: Composer, id: "composer_submit", label: "Send", help: "Send the draft as a new turn." },
    InsertNewline => { scope: Composer, id: "composer_newline", label: "Newline", help: "Insert a line break without sending." },
    ToggleMultiline => { scope: Composer, id: "composer_multiline", label: "Multiline", help: "Toggle multi-line editing." },
    ComposerHistoryPrevious => { scope: Composer, id: "composer_history_prev", label: "Prev sent", help: "Recall the previous message you sent." },
    ComposerHistoryNext => { scope: Composer, id: "composer_history_next", label: "Next sent", help: "Move forward through sent messages." },
    EditComposerExternally => { scope: Composer, id: "composer_editor", label: "$EDITOR", help: "Edit the draft in your editor, then return." },
    BackgroundRunningCommand => { scope: Composer, id: "composer_background", label: "Background", help: "Move the running foreground command to the background." },
    SteerRunningTurn => { scope: Composer, id: "composer_steer", label: "Steer", help: "Inject the draft into the running turn without interrupting it." },
    CompletionNext => { scope: Composer, id: "completion_next", label: "Next match", help: "Highlight the next completion." },
    CompletionPrevious => { scope: Composer, id: "completion_previous", label: "Prev match", help: "Highlight the previous completion." },
    CompletionAccept => { scope: Composer, id: "completion_accept", label: "Accept", help: "Insert the highlighted completion." },
    CompletionCancel => { scope: Composer, id: "completion_cancel", label: "Dismiss", help: "Close the completion menu." },
    DeleteWordBefore => { scope: Composer, id: "composer_delete_word", label: "Delete word", help: "Delete the word before the cursor." },
    DeleteWordAfter => { scope: Composer, id: "composer_delete_word_after", label: "Kill word", help: "Delete the word after the cursor into the kill buffer." },
    DeleteWordBackward => { scope: Composer, id: "composer_delete_word_backward", label: "Kill word back", help: "Delete the word before the cursor into the kill buffer." },
    KillToLineEnd => { scope: Composer, id: "composer_kill_to_end", label: "Kill to end", help: "Cut from the cursor to the end of the line." },
    KillToLineStart => { scope: Composer, id: "composer_kill_to_start", label: "Kill to start", help: "Cut from the start of the line to the cursor." },
    YankKill => { scope: Composer, id: "composer_yank", label: "Yank", help: "Put the last cut text back at the cursor." },
    ComposerUndo => { scope: Composer, id: "composer_undo", label: "Undo", help: "Undo the last edit to the draft." },
    ComposerRedo => { scope: Composer, id: "composer_redo", label: "Redo", help: "Redo an undone edit." },
    ComposerWordLeft => { scope: Composer, id: "composer_word_left", label: "Word left", help: "Move the cursor to the start of the previous word." },
    ComposerWordRight => { scope: Composer, id: "composer_word_right", label: "Word right", help: "Move the cursor past the end of the next word." },
    ComposerLineStart => { scope: Composer, id: "composer_line_start", label: "Line start", help: "Move the cursor to the start of the line." },
    ComposerLineEnd => { scope: Composer, id: "composer_line_end", label: "Line end", help: "Move the cursor to the end of the line." },
    AttachImage => { scope: Composer, id: "composer_image", label: "Image", help: "Attach an image from the clipboard, or name a file to attach." },
    PasteClipboard => { scope: Composer, id: "composer_paste", label: "Paste", help: "Paste the clipboard: an image becomes an attachment, text goes into the draft." },
    SelectAllDraft => { scope: Composer, id: "composer_select_all", label: "Select all", help: "Select the whole draft so typing replaces it." },
    CopyDraftSelection => { scope: Composer, id: "composer_copy_draft", label: "Copy draft", help: "Copy the selected part of the draft to the clipboard." },

    // ---- overlays -------------------------------------------------------
    CloseOverlay => { scope: Overlay, id: "overlay_close", label: "Close", help: "Dismiss this overlay without applying anything." },
    ConfirmOverlay => { scope: Overlay, id: "overlay_confirm", label: "Confirm", help: "Apply this overlay's value." },
    OverlayNextField => { scope: Overlay, id: "overlay_next_field", label: "Next field", help: "Move to the next form field." },
    OverlayPreviousField => { scope: Overlay, id: "overlay_prev_field", label: "Prev field", help: "Move to the previous form field." },
    OverlayToggleValue => { scope: Overlay, id: "overlay_toggle", label: "Toggle", help: "Flip a boolean field or a multi-select option." },
    ScheduleClear => { scope: Overlay, id: "schedule_clear", label: "Clear", help: "Void the draft's send time and keep the message." },
    PaletteRun => { scope: Overlay, id: "palette_run", label: "Run", help: "Run the highlighted command." },

    // ---- the runtime switcher -------------------------------------------
    // These live in the overlay scope rather than a switcher-only one because
    // every other overlay ignores what it does not use, and one shared scope is
    // what keeps the switcher's keys remappable from the same file.
    OverlayFoldOpen => { scope: Overlay, id: "overlay_fold_open", label: "Expand", help: "Expand the highlighted group, or step into it." },
    OverlayFoldClosed => { scope: Overlay, id: "overlay_fold_close", label: "Fold", help: "Fold the highlighted group, or step out to its heading." },
    StarRuntimeModel => { scope: Overlay, id: "overlay_star_model", label: "Star", help: "Star or unstar the highlighted model in the runtime switcher." },
    ManageRuntimeAccount => { scope: Overlay, id: "overlay_manage_account", label: "Account", help: "Open the highlighted runtime's Agent account in the management pages." },
    ResetRunOption => { scope: Overlay, id: "overlay_reset_run_option", label: "Reset", help: "Stage the highlighted run option back to the Agent's own default." },

    // ---- approvals ------------------------------------------------------
    ApprovalApprove => { scope: Overlay, id: "approval_approve", label: "Allow", help: "Approve the pending request." },
    ApprovalDeny => { scope: Overlay, id: "approval_deny", label: "Deny", help: "Refuse the pending request." },
    ApprovalAlways => { scope: Overlay, id: "approval_always", label: "Always", help: "Approve and remember for the rest of this session." },
    ApprovalFocusNext => { scope: Overlay, id: "approval_next", label: "Next option", help: "Move between the request's allowed responses." },
    ElicitationSubmit => { scope: Overlay, id: "elicitation_submit", label: "Submit", help: "Send the answers back to the Agent." },
    ElicitationFieldNext => { scope: Overlay, id: "elicitation_next", label: "Next field", help: "Move to the next question." },
    ElicitationFieldPrevious => { scope: Overlay, id: "elicitation_prev", label: "Prev field", help: "Move to the previous question." },




    // ---- management -----------------------------------------------------
    ToggleSelectedEntry => { scope: Management, id: "management_toggle", label: "Toggle", help: "Enable or disable the highlighted entry." },
    EditSelectedEntry => { scope: Management, id: "management_edit", label: "Edit", help: "Edit the highlighted entry." },
    ReloadManagement => { scope: Management, id: "management_reload", label: "Reload", help: "Re-read this management section." },

    // ---- configuration centre -------------------------------------------
    ConfigModulePrevious => { scope: Global, id: "config_module_previous", label: "Module", help: "Move to the previous configuration module (Agent, MCP, Skills, Prompts)." },
    ConfigModuleNext => { scope: Global, id: "config_module_next", label: "Module", help: "Move to the next configuration module." },
    ConfigSectionPrevious => { scope: Management, id: "config_section_previous", label: "Area", help: "Move to the previous function area of this module." },
    ConfigSectionNext => { scope: Management, id: "config_section_next", label: "Area", help: "Move to the next function area of this module." },
    ConfigPaneNext => { scope: Management, id: "config_pane_next", label: "Pane", help: "Move the keyboard between the modules, the areas, the records and the details." },
    ConfigPanePrevious => { scope: Management, id: "config_pane_previous", label: "Pane", help: "Move the keyboard backwards between the panes." },
    ConfigEntryActions => { scope: Management, id: "config_entry_actions", label: "Actions", help: "Open everything the highlighted record can do." },
    ConfigEntryCreate => { scope: Management, id: "config_entry_create", label: "New", help: "Create a record in this area." },
    ConfigEntryDelete => { scope: Management, id: "config_entry_delete", label: "Delete", help: "Delete the highlighted record." },
    ConfigFormSave => { scope: Overlay, id: "config_form_save", label: "Save", help: "Write the form to the runtime." },
    GotoDevices => { scope: Global, id: "goto_devices", label: "Pairing and devices", help: "Paired devices, pairing codes and the audit log." },

    // ---- providers ------------------------------------------------------

    // ---- devices --------------------------------------------------------
    ActivateDevicePairing => { scope: Devices, id: "device_pair_activate", label: "Open", help: "Open the highlighted way to pair a device." },
    DevicesTabNext => { scope: Devices, id: "device_tab_next", label: "Tab", help: "Move to the other half of the devices page." },
    DevicesTabPrevious => { scope: Devices, id: "device_tab_previous", label: "Tab", help: "Move to the other half of the devices page." },
    AllowLanPairing => { scope: Devices, id: "device_lan_allow", label: "Allow", help: "Approve the device that asked to pair on this network." },
    RefuseLanPairing => { scope: Devices, id: "device_lan_refuse", label: "Refuse", help: "Refuse the device that asked to pair on this network." },
    RevokeSelectedDevice => { scope: Devices, id: "device_revoke", label: "Revoke", help: "Revoke the highlighted device's access." },
    OpenDeviceAudit => { scope: Devices, id: "device_audit", label: "Audit", help: "Show the remote-protocol audit trail." },

    // ---- settings -------------------------------------------------------
    ActivateSetting => { scope: Settings, id: "setting_activate", label: "Change", help: "Change the highlighted setting." },
    SettingPrevious => { scope: Settings, id: "setting_previous", label: "Previous value", help: "Step the setting backwards." },
    SettingNext => { scope: Settings, id: "setting_next", label: "Next value", help: "Step the setting forwards." },
    ResetSetting => { scope: Settings, id: "setting_reset", label: "Reset", help: "Reset the highlighted setting to its default after a confirmation." },

    // ---- filters (shared) ------------------------------------------------
    BeginFilter => { scope: Global, id: "filter_begin", label: "Filter", help: "Type to filter the current list." },
    ClearFilter => { scope: Global, id: "filter_clear", label: "Clear filter", help: "Remove the active filter." },
}

impl Intent {
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|intent| intent.id().eq_ignore_ascii_case(id.trim()))
    }

    /// Whether the intent changes state and therefore needs a live connection.
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Intent::SubmitComposer
                | Intent::ContinueTurn
                | Intent::ApprovalApprove
                | Intent::ApprovalDeny
                | Intent::ApprovalAlways
                | Intent::ElicitationSubmit
                | Intent::BeginRenameSession
                | Intent::ForkSession
                | Intent::ArchiveSession
                | Intent::UnarchiveSession
                | Intent::DeleteSession
                | Intent::ActivateDevicePairing
                | Intent::AllowLanPairing
                | Intent::RefuseLanPairing
                | Intent::RevokeSelectedDevice
                | Intent::ToggleSelectedEntry
                | Intent::EditSelectedEntry
                | Intent::SteerRunningTurn
                | Intent::BackgroundRunningCommand
                | Intent::SwitchAgentRuntime
        )
    }

    /// Whether the intent names somewhere else to be.
    ///
    /// These are the destinations the key bar and the cheatsheet keep
    /// advertising while a modal or a filter owns the keyboard, so a surface
    /// that swallowed them would be promising keys it does not answer: a modal
    /// puts itself down and runs one, exactly as `Esc` would have put it down.
    pub const fn leaves_the_surface(self) -> bool {
        matches!(
            self,
            Intent::OpenCommandPalette
                | Intent::ToggleHelp
                | Intent::OpenSettings
                | Intent::GotoSessions
                | Intent::GotoManagement
        )
    }

    /// Whether the intent is meaningful while the connection is down.
    ///
    /// `NewSession` is here rather than in [`Self::mutates`] because it only
    /// moves to the composing page and asks for reads: the page is the one the
    /// client opens on, so it has to be reachable in the first moments of a run
    /// when the seat is still connecting. Sending from it is what needs the
    /// runtime, and `SubmitComposer` says so.
    pub const fn works_offline(self) -> bool {
        matches!(
            self,
            Intent::ToggleHelp
                | Intent::OpenSettings
                | Intent::OpenCommandPalette
                | Intent::Back
                | Intent::FocusNext
                | Intent::FocusPrevious
                | Intent::NewSession
                | Intent::GotoSessions
                | Intent::GotoManagement
                | Intent::SelectPrevious
                | Intent::SelectNext
                | Intent::ScrollPageUp
                | Intent::ScrollPageDown
                | Intent::ScrollHalfPageUp
                | Intent::ScrollHalfPageDown
                | Intent::ScrollToTop
                | Intent::ScrollToBottom
                | Intent::ToggleBlockExpanded
                | Intent::ToggleAllBlocksExpanded
                | Intent::ToggleReasoningExpanded
                | Intent::ToggleSessionCard
                | Intent::CollapseSessionCards
                | Intent::CopySessionRow
                | Intent::BeginTranscriptSearch
                | Intent::SearchNext
                | Intent::SearchPrevious
                | Intent::QueueSelectPrevious
                | Intent::QueueSelectNext
                | Intent::QueueEditSelected
                | Intent::QueueDeleteSelected
                | Intent::QueueMoveUp
                | Intent::QueueMoveDown
                | Intent::ScheduleMessage
                | Intent::CopyBlockBody
                | Intent::CopyBlockMetadata
                | Intent::OpenBlockDetails
                | Intent::ToggleSidebar
                | Intent::ReloadKeymap
                | Intent::CloseOverlay
                | Intent::RequestQuit
                | Intent::ContextualCancel
                | Intent::FocusComposer
                | Intent::InsertNewline
                | Intent::ToggleMultiline
                | Intent::ComposerHistoryPrevious
                | Intent::ComposerHistoryNext
                | Intent::CompletionNext
                | Intent::CompletionPrevious
                | Intent::CompletionAccept
                | Intent::CompletionCancel
                | Intent::DeleteWordBefore
                | Intent::DeleteWordAfter
                | Intent::DeleteWordBackward
                | Intent::KillToLineEnd
                | Intent::KillToLineStart
                | Intent::YankKill
                | Intent::ComposerUndo
                | Intent::ComposerRedo
                | Intent::ComposerWordLeft
                | Intent::ComposerWordRight
                | Intent::ComposerLineStart
                | Intent::ComposerLineEnd
                | Intent::SelectAllDraft
                | Intent::CopyDraftSelection
                | Intent::BeginFilter
                | Intent::ClearFilter
                | Intent::ActivateSetting
                | Intent::SettingPrevious
                | Intent::SettingNext
                | Intent::ResetSetting
                // Folding the catalogue, starring a model and resetting a run
                // option are all local: the switcher can be read and arranged
                // while the runtime is unreachable. Sending what a row was set
                // to does need the runtime, which is why `ManageRuntimeAccount`
                // is deliberately absent.
                | Intent::OverlayFoldOpen
                | Intent::OverlayFoldClosed
                | Intent::StarRuntimeModel
                | Intent::ResetRunOption
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_unique() {
        let mut seen = BTreeSet::new();
        for intent in Intent::ALL {
            assert!(seen.insert(intent.id()), "duplicate id {}", intent.id());
        }
    }

    #[test]
    fn labels_and_help_are_present() {
        for intent in Intent::ALL {
            assert!(!intent.default_label().is_empty(), "{}", intent.id());
            assert!(!intent.help().is_empty(), "{}", intent.id());
            assert!(
                intent.help().ends_with('.'),
                "{} help should be a sentence",
                intent.id()
            );
        }
    }

    #[test]
    fn every_intent_round_trips_through_its_id() {
        for intent in Intent::ALL {
            assert_eq!(Intent::from_id(intent.id()), Some(*intent));
        }
        assert_eq!(Intent::from_id("  QUIT "), Some(Intent::RequestQuit));
        assert_eq!(Intent::from_id("nope"), None);
    }

    #[test]
    fn mutating_intents_never_claim_to_work_offline() {
        for intent in Intent::ALL {
            if intent.mutates() {
                assert!(
                    !intent.works_offline(),
                    "{} mutates but claims to work offline",
                    intent.id()
                );
            }
        }
    }
}
