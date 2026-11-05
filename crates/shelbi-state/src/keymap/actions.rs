//! Customizable keybindings — the action hierarchy.
//!
//! Every key in shelbi's TUI maps to an [`Action`]. Actions are split per
//! mode (global, sidebar, kanban, popover, activity, diff, palette) so
//! that the same chord can mean different things in different views
//! without ambiguity.
//!
//! The enum here is the source of truth for built-in defaults: each
//! variant carries its display description, its default chord list, and
//! the mode it belongs to. The `keys.yaml` loader iterates [`Action::all`]
//! to seed the embedded layer of the three-layer merge.
//!
//! Match arms intentionally avoid macro magic — `grep` for an action name
//! should land directly on the description / default chord list / mode.

/// Mode-tagged action — the dispatched value when a chord matches in one
/// of the TUI modes. Re-exports [`GlobalAction`] etc. through the
/// inner enums for direct construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Global(GlobalAction),
    Sidebar(SidebarAction),
    Kanban(KanbanAction),
    Popover(PopoverAction),
    Activity(ActivityAction),
    Diff(DiffAction),
    Palette(PaletteAction),
}

/// Actions that fire regardless of which view is focused (Ctrl+C, the
/// palette opener, the Zen toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalAction {
    Quit,
    ZenToggle,
    OpenPalette,
    /// Move keyboard focus left: to the nav sidebar (or, while a review is
    /// open, the review panel). Vim-style `Ctrl+H`.
    FocusSidebar,
    /// Move keyboard focus right: to the main pane (or, while a review is
    /// open, the review content view). Vim-style `Ctrl+L`, so it also does
    /// what Ctrl+L conventionally does: a full clear-and-redraw of the screen,
    /// which recovers the TUI from anything written over it.
    FocusMain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarAction {
    Quit,
    NavUp,
    NavDown,
    Activate,
    Refresh,
    /// Open Settings in place of the sidebar (the desktop app's ⌘, as a
    /// terminal-safe key: the sidebar has focus, so it never reaches an
    /// agent).
    OpenSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KanbanAction {
    NavLeft,
    NavRight,
    NavUp,
    NavDown,
    MoveCardLeft,
    MoveCardRight,
    ReorderUp,
    ReorderDown,
    OpenPopover,
    Refresh,
    CycleWorkflowFilter,
    OpenWorkspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PopoverAction {
    Close,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ScrollHome,
    MoveLeft,
    MoveRight,
    OpenWorkspace,
    OpenPr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityAction {
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ScrollHome,
    ScrollEnd,
    Refresh,
    ResetFilter,
    ToggleZenFilter,
    ToggleWorkspacesFilter,
}

/// What a key does in the built-in diff view (Skim's normal-mode keys). Both
/// clients drive `shelbi_app::diff` with these. The two-key defaults (`g g`,
/// `z a`, `] h`) are the only two-key bindings the keymap takes: see
/// [`KeyChord`](super::KeyChord)'s leader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffAction {
    CursorDown,
    CursorUp,
    PrevFile,
    NextFile,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    Top,
    Bottom,
    CenterViewport,
    CenterCursor,
    NextChange,
    PrevChange,
    NextComment,
    PrevComment,
    NextEmptyLine,
    PrevEmptyLine,
    FindForward,
    FindBackward,
    TillForward,
    TillBackward,
    RepeatFind,
    Search,
    NextMatch,
    PrevMatch,
    FilePalette,
    CommandPalette,
    Help,
    ToggleSplit,
    NextHunkFilter,
    PrevHunkFilter,
    ToggleBlame,
    ToggleHunkFold,
    CloseHunkFold,
    OpenHunkFold,
    CloseFileFold,
    OpenFileFold,
    CloseAllFolds,
    OpenAllFolds,
    VisualSelect,
    Comment,
    ReplyOrRefresh,
    ToggleThread,
    DeleteComment,
    ClearComments,
    Yank,
    YankAll,
    SendToAgent,
    SendToReviewer,
    RevertHunk,
    RevertFile,
    OpenInEditor,
    ToggleViewed,
    PickCommit,
    ToggleFileList,
    ToggleResolved,
    ToggleCommentTarget,
    SubmitReview,
    PrInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteAction {
    Close,
    Activate,
    NavUp,
    NavDown,
    Backspace,
}

/// Lowercase mode names — also the top-level YAML keys under
/// `keys.yaml::defaults` and `keys.yaml::projects.<name>`.
pub const MODE_NAMES: &[&str] = &[
    "global", "sidebar", "kanban", "popover", "activity", "diff", "palette",
];

impl Action {
    /// Human-readable label for the help screen and palette hints.
    pub const fn description(&self) -> &'static str {
        match self {
            Action::Global(a) => match a {
                GlobalAction::Quit => "Quit shelbi",
                GlobalAction::ZenToggle => "Toggle Zen Mode",
                GlobalAction::OpenPalette => "Open command palette",
                GlobalAction::FocusSidebar => "Focus the nav sidebar (left)",
                GlobalAction::FocusMain => "Focus the main pane (right) and redraw the screen",
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => "Quit sidebar",
                SidebarAction::NavUp => "Sidebar: move selection up",
                SidebarAction::NavDown => "Sidebar: move selection down",
                SidebarAction::Activate => "Sidebar: activate selection",
                SidebarAction::Refresh => "Sidebar: refresh",
                SidebarAction::OpenSettings => "Sidebar: open Settings",
            },
            Action::Kanban(a) => match a {
                KanbanAction::NavLeft => "Kanban: focus column to the left",
                KanbanAction::NavRight => "Kanban: focus column to the right",
                KanbanAction::NavUp => "Kanban: move selection up",
                KanbanAction::NavDown => "Kanban: move selection down",
                KanbanAction::MoveCardLeft => "Kanban: move card to the left column",
                KanbanAction::MoveCardRight => "Kanban: move card to the right column",
                KanbanAction::ReorderUp => "Kanban: reorder card up within column",
                KanbanAction::ReorderDown => "Kanban: reorder card down within column",
                KanbanAction::OpenPopover => "Kanban: open card popover",
                KanbanAction::Refresh => "Kanban: refresh",
                KanbanAction::CycleWorkflowFilter => {
                    "Kanban: cycle workflow filter (All → wf1 → wf2 → All)"
                }
                KanbanAction::OpenWorkspace => "Kanban: open selected card's assigned workspace",
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => "Popover: close",
                PopoverAction::ScrollUp => "Popover: scroll up",
                PopoverAction::ScrollDown => "Popover: scroll down",
                PopoverAction::PageUp => "Popover: page up",
                PopoverAction::PageDown => "Popover: page down",
                PopoverAction::ScrollHome => "Popover: scroll to top",
                PopoverAction::MoveLeft => "Popover: move task to the left column",
                PopoverAction::MoveRight => "Popover: move task to the right column",
                PopoverAction::OpenWorkspace => "Popover: open card's assigned workspace",
                PopoverAction::OpenPr => "Popover: open card's pull request in the browser",
            },
            Action::Activity(a) => match a {
                ActivityAction::ScrollUp => "Activity: scroll up",
                ActivityAction::ScrollDown => "Activity: scroll down",
                ActivityAction::PageUp => "Activity: page up",
                ActivityAction::PageDown => "Activity: page down",
                ActivityAction::ScrollHome => "Activity: scroll to top",
                ActivityAction::ScrollEnd => "Activity: scroll to bottom",
                ActivityAction::Refresh => "Activity: refresh",
                ActivityAction::ResetFilter => "Activity: reset filter",
                ActivityAction::ToggleZenFilter => "Activity: toggle Zen filter",
                ActivityAction::ToggleWorkspacesFilter => "Activity: toggle workspaces filter",
            },
            Action::Diff(a) => match a {
                DiffAction::CursorDown => "Diff: line down",
                DiffAction::CursorUp => "Diff: line up",
                DiffAction::PrevFile => "Diff: previous file",
                DiffAction::NextFile => "Diff: next file",
                DiffAction::PageDown => "Diff: page down",
                DiffAction::PageUp => "Diff: page up",
                DiffAction::HalfPageDown => "Diff: half page down",
                DiffAction::HalfPageUp => "Diff: half page up",
                DiffAction::Top => "Diff: go to the top",
                DiffAction::Bottom => "Diff: go to the bottom",
                DiffAction::CenterViewport => "Diff: center the viewport on the cursor",
                DiffAction::CenterCursor => "Diff: center the cursor in the viewport",
                DiffAction::NextChange => "Diff: next change",
                DiffAction::PrevChange => "Diff: previous change",
                DiffAction::NextComment => "Diff: next comment",
                DiffAction::PrevComment => "Diff: previous comment",
                DiffAction::NextEmptyLine => "Diff: next empty line",
                DiffAction::PrevEmptyLine => "Diff: previous empty line",
                DiffAction::FindForward => "Diff: find a character forward",
                DiffAction::FindBackward => "Diff: find a character backward",
                DiffAction::TillForward => "Diff: move to just before a character",
                DiffAction::TillBackward => "Diff: move back to just after a character",
                DiffAction::RepeatFind => "Diff: repeat the character find",
                DiffAction::Search => "Diff: search all files",
                DiffAction::NextMatch => "Diff: next search match",
                DiffAction::PrevMatch => "Diff: previous search match",
                DiffAction::FilePalette => "Diff: file palette",
                DiffAction::CommandPalette => "Diff: command palette",
                DiffAction::Help => "Diff: show the keys",
                DiffAction::ToggleSplit => "Diff: unified or split view",
                DiffAction::NextHunkFilter => "Diff: next hunk filter (all, additions, deletions)",
                DiffAction::PrevHunkFilter => "Diff: previous hunk filter",
                DiffAction::ToggleBlame => "Diff: blame gutter",
                DiffAction::ToggleHunkFold => "Diff: fold or unfold the hunk",
                DiffAction::CloseHunkFold => "Diff: fold the hunk",
                DiffAction::OpenHunkFold => "Diff: unfold the hunk",
                DiffAction::CloseFileFold => "Diff: fold the file",
                DiffAction::OpenFileFold => "Diff: unfold the file",
                DiffAction::CloseAllFolds => "Diff: fold everything",
                DiffAction::OpenAllFolds => "Diff: unfold everything",
                DiffAction::VisualSelect => "Diff: select lines",
                DiffAction::Comment => "Diff: comment on the line, or edit its comment",
                DiffAction::ReplyOrRefresh => "Diff: reply to the comment, or refresh",
                DiffAction::ToggleThread => "Diff: expand or collapse the thread",
                DiffAction::DeleteComment => "Diff: delete the comment",
                DiffAction::ClearComments => "Diff: clear all comments",
                DiffAction::Yank => "Diff: yank the comment or selection",
                DiffAction::YankAll => "Diff: yank all comments",
                DiffAction::SendToAgent => "Diff: send all comments to the agent",
                DiffAction::SendToReviewer => "Diff: send all comments to the Reviewer agent",
                DiffAction::RevertHunk => "Diff: revert the hunk",
                DiffAction::RevertFile => "Diff: revert the file",
                DiffAction::OpenInEditor => "Diff: open the file at the line in the editor",
                DiffAction::ToggleViewed => "Diff: mark the file viewed",
                DiffAction::PickCommit => "Diff: review one commit",
                DiffAction::ToggleFileList => "Diff: show or hide the file list",
                DiffAction::ToggleResolved => "Diff: resolve or reopen the GitHub thread",
                DiffAction::ToggleCommentTarget => "Diff: new comments go to GitHub or stay here",
                DiffAction::SubmitReview => "Diff: submit a review of the pull request",
                DiffAction::PrInfo => "Diff: every check and review on the pull request",
            },
            Action::Palette(a) => match a {
                PaletteAction::Close => "Palette: close",
                PaletteAction::Activate => "Palette: activate selection",
                PaletteAction::NavUp => "Palette: move selection up",
                PaletteAction::NavDown => "Palette: move selection down",
                PaletteAction::Backspace => "Palette: delete last query char",
            },
        }
    }

    /// Short label, without the mode prefix [`Self::description`] carries:
    /// what a list grouped by mode (the desktop's Settings ▸ Keybindings)
    /// shows beside the action's chords. The group's title
    /// ([`KeymapContext::title`]) supplies the mode.
    ///
    /// [`KeymapContext::title`]: crate::keymap::KeymapContext
    pub const fn label(&self) -> &'static str {
        match self {
            Action::Global(a) => match a {
                GlobalAction::Quit => "Quit Shelbi",
                GlobalAction::ZenToggle => "Toggle Zen mode",
                GlobalAction::OpenPalette => "Open command palette",
                GlobalAction::FocusSidebar => "Focus sidebar",
                GlobalAction::FocusMain => "Focus main pane and redraw",
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => "Quit",
                SidebarAction::NavUp => "Move selection up",
                SidebarAction::NavDown => "Move selection down",
                SidebarAction::Activate => "Open selection",
                SidebarAction::Refresh => "Refresh",
                SidebarAction::OpenSettings => "Open Settings",
            },
            Action::Kanban(a) => match a {
                KanbanAction::NavLeft => "Focus previous column",
                KanbanAction::NavRight => "Focus next column",
                KanbanAction::NavUp => "Move selection up",
                KanbanAction::NavDown => "Move selection down",
                KanbanAction::MoveCardLeft => "Move card to previous column",
                KanbanAction::MoveCardRight => "Move card to next column",
                KanbanAction::ReorderUp => "Move card up",
                KanbanAction::ReorderDown => "Move card down",
                KanbanAction::OpenPopover => "Open issue details",
                KanbanAction::Refresh => "Refresh",
                KanbanAction::CycleWorkflowFilter => "Cycle workflow filter",
                KanbanAction::OpenWorkspace => "Open assigned workspace",
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => "Close issue details",
                PopoverAction::ScrollUp => "Scroll up",
                PopoverAction::ScrollDown => "Scroll down",
                PopoverAction::PageUp => "Page up",
                PopoverAction::PageDown => "Page down",
                PopoverAction::ScrollHome => "Scroll to top",
                PopoverAction::MoveLeft => "Move task to previous column",
                PopoverAction::MoveRight => "Move task to next column",
                PopoverAction::OpenWorkspace => "Open assigned workspace",
                PopoverAction::OpenPr => "Open pull request",
            },
            Action::Activity(a) => match a {
                ActivityAction::ScrollUp => "Scroll up",
                ActivityAction::ScrollDown => "Scroll down",
                ActivityAction::PageUp => "Page up",
                ActivityAction::PageDown => "Page down",
                ActivityAction::ScrollHome => "Scroll to top",
                ActivityAction::ScrollEnd => "Scroll to bottom",
                ActivityAction::Refresh => "Refresh",
                ActivityAction::ResetFilter => "Reset filters",
                ActivityAction::ToggleZenFilter => "Toggle Zen filter",
                ActivityAction::ToggleWorkspacesFilter => "Toggle workspaces filter",
            },
            Action::Diff(a) => match a {
                DiffAction::CursorDown => "Line down",
                DiffAction::CursorUp => "Line up",
                DiffAction::PrevFile => "Previous file",
                DiffAction::NextFile => "Next file",
                DiffAction::PageDown => "Page down",
                DiffAction::PageUp => "Page up",
                DiffAction::HalfPageDown => "Half page down",
                DiffAction::HalfPageUp => "Half page up",
                DiffAction::Top => "Go to the top",
                DiffAction::Bottom => "Go to the bottom",
                DiffAction::CenterViewport => "Center the viewport on the cursor",
                DiffAction::CenterCursor => "Center the cursor in the viewport",
                DiffAction::NextChange => "Next change",
                DiffAction::PrevChange => "Previous change",
                DiffAction::NextComment => "Next comment",
                DiffAction::PrevComment => "Previous comment",
                DiffAction::NextEmptyLine => "Next empty line",
                DiffAction::PrevEmptyLine => "Previous empty line",
                DiffAction::FindForward => "Find a character forward",
                DiffAction::FindBackward => "Find a character backward",
                DiffAction::TillForward => "Move to just before a character",
                DiffAction::TillBackward => "Move back to just after a character",
                DiffAction::RepeatFind => "Repeat the character find",
                DiffAction::Search => "Search all files",
                DiffAction::NextMatch => "Next search match",
                DiffAction::PrevMatch => "Previous search match",
                DiffAction::FilePalette => "File palette",
                DiffAction::CommandPalette => "Command palette",
                DiffAction::Help => "Show the keys",
                DiffAction::ToggleSplit => "Unified or split view",
                DiffAction::NextHunkFilter => "Next hunk filter (all, additions, deletions)",
                DiffAction::PrevHunkFilter => "Previous hunk filter",
                DiffAction::ToggleBlame => "Blame gutter",
                DiffAction::ToggleHunkFold => "Fold or unfold the hunk",
                DiffAction::CloseHunkFold => "Fold the hunk",
                DiffAction::OpenHunkFold => "Unfold the hunk",
                DiffAction::CloseFileFold => "Fold the file",
                DiffAction::OpenFileFold => "Unfold the file",
                DiffAction::CloseAllFolds => "Fold everything",
                DiffAction::OpenAllFolds => "Unfold everything",
                DiffAction::VisualSelect => "Select lines",
                DiffAction::Comment => "Comment on the line, or edit its comment",
                DiffAction::ReplyOrRefresh => "Reply to the comment, or refresh",
                DiffAction::ToggleThread => "Expand or collapse the thread",
                DiffAction::DeleteComment => "Delete the comment",
                DiffAction::ClearComments => "Clear all comments",
                DiffAction::Yank => "Yank the comment or selection",
                DiffAction::YankAll => "Yank all comments",
                DiffAction::SendToAgent => "Send all comments to the agent",
                DiffAction::SendToReviewer => "Send all comments to the Reviewer agent",
                DiffAction::RevertHunk => "Revert the hunk",
                DiffAction::RevertFile => "Revert the file",
                DiffAction::OpenInEditor => "Open the file at the line in the editor",
                DiffAction::ToggleViewed => "Mark the file viewed",
                DiffAction::PickCommit => "Review one commit",
                DiffAction::ToggleFileList => "Show or hide the file list",
                DiffAction::ToggleResolved => "Resolve or reopen the GitHub thread",
                DiffAction::ToggleCommentTarget => "New comments go to GitHub or stay here",
                DiffAction::SubmitReview => "Submit a review of the pull request",
                DiffAction::PrInfo => "Every check and review on the pull request",
            },
            Action::Palette(a) => match a {
                PaletteAction::Close => "Close command palette",
                PaletteAction::Activate => "Run selected command",
                PaletteAction::NavUp => "Previous command",
                PaletteAction::NavDown => "Next command",
                PaletteAction::Backspace => "Delete previous character",
            },
        }
    }

    /// Default chord list — what `load_keymaps` installs when the user
    /// has no `keys.yaml`. Each string must parse with [`KeyChord::parse`].
    ///
    /// [`KeyChord::parse`]: crate::keymap::KeyChord::parse
    pub fn default_chords(&self) -> &'static [&'static str] {
        match self {
            Action::Global(a) => match a {
                GlobalAction::Quit => &["ctrl-c"],
                GlobalAction::ZenToggle => &["alt-z"],
                GlobalAction::OpenPalette => &["ctrl-p"],
                GlobalAction::FocusSidebar => &["ctrl-h"],
                GlobalAction::FocusMain => &["ctrl-l"],
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => &["q", "ctrl-c"],
                SidebarAction::NavUp => &["k", "up"],
                SidebarAction::NavDown => &["j", "down"],
                // Space joins Enter so a user can collapse / expand a
                // focused machine row without leaving the keyboard. The
                // same chord activates a focused workspace row — matches
                // the kanban's "enter or space" affordance.
                SidebarAction::Activate => &["enter", "space"],
                SidebarAction::Refresh => &["r"],
                // The comma of the desktop's ⌘, on its own: free in the
                // sidebar, and no terminal mangles it.
                SidebarAction::OpenSettings => &[","],
            },
            Action::Kanban(a) => match a {
                KanbanAction::NavLeft => &["h", "left"],
                KanbanAction::NavRight => &["l", "right"],
                KanbanAction::NavUp => &["k", "up"],
                KanbanAction::NavDown => &["j", "down"],
                KanbanAction::MoveCardLeft => &["H"],
                KanbanAction::MoveCardRight => &["L"],
                KanbanAction::ReorderUp => &["K", "shift-up"],
                KanbanAction::ReorderDown => &["J", "shift-down"],
                KanbanAction::OpenPopover => &["enter", "space"],
                KanbanAction::Refresh => &["r"],
                KanbanAction::CycleWorkflowFilter => &["tab"],
                // `o` for "open workspace" — jump to the tmux chat of the
                // selected card's assigned workspace. Free across the board
                // keymap; mirrored on the popover so it works open or closed.
                KanbanAction::OpenWorkspace => &["o"],
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => &["esc", "enter", "space", "q"],
                PopoverAction::ScrollUp => &["k", "up"],
                PopoverAction::ScrollDown => &["j", "down"],
                PopoverAction::PageUp => &["page-up", "u"],
                PopoverAction::PageDown => &["page-down", "d"],
                PopoverAction::ScrollHome => &["g", "home"],
                // Same capital H / L the board uses for `move_card_*`,
                // so muscle memory carries into the modal. Shift+arrows
                // join them the way `reorder_*` pairs K / shift-up.
                PopoverAction::MoveLeft => &["H", "shift-left"],
                PopoverAction::MoveRight => &["L", "shift-right"],
                // Mirrors the board's `o` so jumping to the card's workspace
                // works whether the card is open or closed.
                PopoverAction::OpenWorkspace => &["o"],
                PopoverAction::OpenPr => &["p"],
            },
            Action::Activity(a) => match a {
                ActivityAction::ScrollUp => &["k", "up"],
                ActivityAction::ScrollDown => &["j", "down"],
                ActivityAction::PageUp => &["page-up", "u"],
                ActivityAction::PageDown => &["page-down", "d"],
                ActivityAction::ScrollHome => &["g", "home"],
                ActivityAction::ScrollEnd => &["G", "end"],
                ActivityAction::Refresh => &["r"],
                ActivityAction::ResetFilter => &["a"],
                ActivityAction::ToggleZenFilter => &["z"],
                ActivityAction::ToggleWorkspacesFilter => &["w"],
            },
            // Skim's defaults, plus the arrows, and `m` (viewed), `c` (one
            // commit) and backslash (file list), which have no key in Skim.
            Action::Diff(a) => match a {
                DiffAction::CursorDown => &["j", "down"],
                DiffAction::CursorUp => &["k", "up"],
                DiffAction::PrevFile => &["h", "left"],
                DiffAction::NextFile => &["l", "ctrl-n", "right"],
                DiffAction::PageDown => &["space", "ctrl-f", "page-down"],
                DiffAction::PageUp => &["b", "page-up"],
                DiffAction::HalfPageDown => &["ctrl-d"],
                DiffAction::HalfPageUp => &["ctrl-u"],
                DiffAction::Top => &["g g"],
                DiffAction::Bottom => &["G"],
                DiffAction::CenterViewport => &["z z"],
                DiffAction::CenterCursor => &["M"],
                DiffAction::NextChange => &["] h"],
                DiffAction::PrevChange => &["[ h"],
                DiffAction::NextComment => &["] c"],
                DiffAction::PrevComment => &["[ c"],
                DiffAction::NextEmptyLine => &["}"],
                DiffAction::PrevEmptyLine => &["{"],
                DiffAction::FindForward => &["f"],
                DiffAction::FindBackward => &["F"],
                DiffAction::TillForward => &["t"],
                DiffAction::TillBackward => &["T"],
                DiffAction::RepeatFind => &[";"],
                DiffAction::Search => &["/"],
                DiffAction::NextMatch => &["n"],
                DiffAction::PrevMatch => &["N"],
                DiffAction::FilePalette => &["ctrl-p"],
                DiffAction::CommandPalette => &[":"],
                DiffAction::Help => &["?"],
                DiffAction::ToggleSplit => &["s"],
                DiffAction::NextHunkFilter => &["tab"],
                DiffAction::PrevHunkFilter => &["back-tab"],
                DiffAction::ToggleBlame => &["B"],
                DiffAction::ToggleHunkFold => &["z a"],
                DiffAction::CloseHunkFold => &["z c"],
                DiffAction::OpenHunkFold => &["z o"],
                DiffAction::CloseFileFold => &["z C"],
                DiffAction::OpenFileFold => &["z O"],
                DiffAction::CloseAllFolds => &["z M"],
                DiffAction::OpenAllFolds => &["z R"],
                DiffAction::VisualSelect => &["v", "V"],
                DiffAction::Comment => &["enter"],
                DiffAction::ReplyOrRefresh => &["r"],
                DiffAction::ToggleThread => &["o"],
                DiffAction::DeleteComment => &["d"],
                DiffAction::ClearComments => &["D"],
                DiffAction::Yank => &["y"],
                DiffAction::YankAll => &["Y"],
                DiffAction::SendToAgent => &["g Y"],
                DiffAction::SendToReviewer => &["g A"],
                DiffAction::RevertHunk => &["g r"],
                DiffAction::RevertFile => &["g R"],
                DiffAction::OpenInEditor => &["ctrl-g"],
                DiffAction::ToggleViewed => &["m"],
                DiffAction::PickCommit => &["c"],
                DiffAction::ToggleFileList => &["ctrl-b", "\\"],
                DiffAction::ToggleResolved => &["x"],
                DiffAction::ToggleCommentTarget => &["C"],
                DiffAction::SubmitReview => &["R"],
                DiffAction::PrInfo => &["i"],
            },
            Action::Palette(a) => match a {
                PaletteAction::Close => &["esc", "ctrl-c", "ctrl-p"],
                PaletteAction::Activate => &["enter"],
                PaletteAction::NavUp => &["up"],
                PaletteAction::NavDown => &["down"],
                PaletteAction::Backspace => &["backspace"],
            },
        }
    }

    /// The mode this action belongs to — one of [`MODE_NAMES`].
    pub const fn mode(&self) -> &'static str {
        match self {
            Action::Global(_) => "global",
            Action::Sidebar(_) => "sidebar",
            Action::Kanban(_) => "kanban",
            Action::Popover(_) => "popover",
            Action::Activity(_) => "activity",
            Action::Diff(_) => "diff",
            Action::Palette(_) => "palette",
        }
    }

    /// Whether this action's mode takes two-key bindings (`g g`). Only the
    /// diff view's key layer waits for a second key, so a two-key binding
    /// anywhere else could never fire and the loader refuses it.
    pub const fn takes_two_keys(&self) -> bool {
        matches!(self, Action::Diff(_))
    }

    /// snake_case key name as it appears in `keys.yaml`. The inverse of
    /// the parser's lookup table — used by emit-side code (round-trip
    /// tests, future `shelbi keys export`).
    pub const fn key_name(&self) -> &'static str {
        match self {
            Action::Global(a) => match a {
                GlobalAction::Quit => "quit",
                GlobalAction::ZenToggle => "zen_toggle",
                GlobalAction::OpenPalette => "open_palette",
                GlobalAction::FocusSidebar => "focus_sidebar",
                GlobalAction::FocusMain => "focus_main",
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => "quit",
                SidebarAction::NavUp => "nav_up",
                SidebarAction::NavDown => "nav_down",
                SidebarAction::Activate => "activate",
                SidebarAction::Refresh => "refresh",
                SidebarAction::OpenSettings => "open_settings",
            },
            Action::Kanban(a) => match a {
                KanbanAction::NavLeft => "nav_left",
                KanbanAction::NavRight => "nav_right",
                KanbanAction::NavUp => "nav_up",
                KanbanAction::NavDown => "nav_down",
                KanbanAction::MoveCardLeft => "move_card_left",
                KanbanAction::MoveCardRight => "move_card_right",
                KanbanAction::ReorderUp => "reorder_up",
                KanbanAction::ReorderDown => "reorder_down",
                KanbanAction::OpenPopover => "open_popover",
                KanbanAction::Refresh => "refresh",
                KanbanAction::CycleWorkflowFilter => "cycle_workflow_filter",
                KanbanAction::OpenWorkspace => "open_workspace",
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => "close",
                PopoverAction::ScrollUp => "scroll_up",
                PopoverAction::ScrollDown => "scroll_down",
                PopoverAction::PageUp => "page_up",
                PopoverAction::PageDown => "page_down",
                PopoverAction::ScrollHome => "scroll_home",
                PopoverAction::MoveLeft => "move_left",
                PopoverAction::MoveRight => "move_right",
                PopoverAction::OpenWorkspace => "open_workspace",
                PopoverAction::OpenPr => "open_pr",
            },
            Action::Activity(a) => match a {
                ActivityAction::ScrollUp => "scroll_up",
                ActivityAction::ScrollDown => "scroll_down",
                ActivityAction::PageUp => "page_up",
                ActivityAction::PageDown => "page_down",
                ActivityAction::ScrollHome => "scroll_home",
                ActivityAction::ScrollEnd => "scroll_end",
                ActivityAction::Refresh => "refresh",
                ActivityAction::ResetFilter => "reset_filter",
                ActivityAction::ToggleZenFilter => "toggle_zen_filter",
                ActivityAction::ToggleWorkspacesFilter => "toggle_workspaces_filter",
            },
            Action::Diff(a) => match a {
                DiffAction::CursorDown => "cursor_down",
                DiffAction::CursorUp => "cursor_up",
                DiffAction::PrevFile => "prev_file",
                DiffAction::NextFile => "next_file",
                DiffAction::PageDown => "page_down",
                DiffAction::PageUp => "page_up",
                DiffAction::HalfPageDown => "half_page_down",
                DiffAction::HalfPageUp => "half_page_up",
                DiffAction::Top => "top",
                DiffAction::Bottom => "bottom",
                DiffAction::CenterViewport => "center_viewport",
                DiffAction::CenterCursor => "center_cursor",
                DiffAction::NextChange => "next_change",
                DiffAction::PrevChange => "prev_change",
                DiffAction::NextComment => "next_comment",
                DiffAction::PrevComment => "prev_comment",
                DiffAction::NextEmptyLine => "next_empty_line",
                DiffAction::PrevEmptyLine => "prev_empty_line",
                DiffAction::FindForward => "find_forward",
                DiffAction::FindBackward => "find_backward",
                DiffAction::TillForward => "till_forward",
                DiffAction::TillBackward => "till_backward",
                DiffAction::RepeatFind => "repeat_find",
                DiffAction::Search => "search",
                DiffAction::NextMatch => "next_match",
                DiffAction::PrevMatch => "prev_match",
                DiffAction::FilePalette => "file_palette",
                DiffAction::CommandPalette => "command_palette",
                DiffAction::Help => "help",
                DiffAction::ToggleSplit => "toggle_split",
                DiffAction::NextHunkFilter => "next_hunk_filter",
                DiffAction::PrevHunkFilter => "prev_hunk_filter",
                DiffAction::ToggleBlame => "toggle_blame",
                DiffAction::ToggleHunkFold => "toggle_hunk_fold",
                DiffAction::CloseHunkFold => "close_hunk_fold",
                DiffAction::OpenHunkFold => "open_hunk_fold",
                DiffAction::CloseFileFold => "close_file_fold",
                DiffAction::OpenFileFold => "open_file_fold",
                DiffAction::CloseAllFolds => "close_all_folds",
                DiffAction::OpenAllFolds => "open_all_folds",
                DiffAction::VisualSelect => "visual_select",
                DiffAction::Comment => "comment",
                DiffAction::ReplyOrRefresh => "reply_or_refresh",
                DiffAction::ToggleThread => "toggle_thread",
                DiffAction::DeleteComment => "delete_comment",
                DiffAction::ClearComments => "clear_comments",
                DiffAction::Yank => "yank",
                DiffAction::YankAll => "yank_all",
                DiffAction::SendToAgent => "send_to_agent",
                DiffAction::SendToReviewer => "send_to_reviewer",
                DiffAction::RevertHunk => "revert_hunk",
                DiffAction::RevertFile => "revert_file",
                DiffAction::OpenInEditor => "open_in_editor",
                DiffAction::ToggleViewed => "toggle_viewed",
                DiffAction::PickCommit => "pick_commit",
                DiffAction::ToggleFileList => "toggle_file_list",
                DiffAction::ToggleResolved => "toggle_resolved",
                DiffAction::ToggleCommentTarget => "toggle_comment_target",
                DiffAction::SubmitReview => "submit_review",
                DiffAction::PrInfo => "pr_info",
            },
            Action::Palette(a) => match a {
                PaletteAction::Close => "close",
                PaletteAction::Activate => "activate",
                PaletteAction::NavUp => "nav_up",
                PaletteAction::NavDown => "nav_down",
                PaletteAction::Backspace => "backspace",
            },
        }
    }

    /// Iterate every action across every mode. Order is stable
    /// (global → sidebar → … → palette, in declaration order within
    /// each mode) so emit-side callers can produce deterministic output.
    pub fn all() -> impl Iterator<Item = Action> {
        const GLOBAL: &[GlobalAction] = &[
            GlobalAction::Quit,
            GlobalAction::ZenToggle,
            GlobalAction::OpenPalette,
            GlobalAction::FocusSidebar,
            GlobalAction::FocusMain,
        ];
        const SIDEBAR: &[SidebarAction] = &[
            SidebarAction::Quit,
            SidebarAction::NavUp,
            SidebarAction::NavDown,
            SidebarAction::Activate,
            SidebarAction::Refresh,
            SidebarAction::OpenSettings,
        ];
        const KANBAN: &[KanbanAction] = &[
            KanbanAction::NavLeft,
            KanbanAction::NavRight,
            KanbanAction::NavUp,
            KanbanAction::NavDown,
            KanbanAction::MoveCardLeft,
            KanbanAction::MoveCardRight,
            KanbanAction::ReorderUp,
            KanbanAction::ReorderDown,
            KanbanAction::OpenPopover,
            KanbanAction::Refresh,
            KanbanAction::CycleWorkflowFilter,
            KanbanAction::OpenWorkspace,
        ];
        const POPOVER: &[PopoverAction] = &[
            PopoverAction::Close,
            PopoverAction::ScrollUp,
            PopoverAction::ScrollDown,
            PopoverAction::PageUp,
            PopoverAction::PageDown,
            PopoverAction::ScrollHome,
            PopoverAction::MoveLeft,
            PopoverAction::MoveRight,
            PopoverAction::OpenWorkspace,
            PopoverAction::OpenPr,
        ];
        const ACTIVITY: &[ActivityAction] = &[
            ActivityAction::ScrollUp,
            ActivityAction::ScrollDown,
            ActivityAction::PageUp,
            ActivityAction::PageDown,
            ActivityAction::ScrollHome,
            ActivityAction::ScrollEnd,
            ActivityAction::Refresh,
            ActivityAction::ResetFilter,
            ActivityAction::ToggleZenFilter,
            ActivityAction::ToggleWorkspacesFilter,
        ];
        const DIFF: &[DiffAction] = &[
            DiffAction::CursorDown,
            DiffAction::CursorUp,
            DiffAction::PrevFile,
            DiffAction::NextFile,
            DiffAction::PageDown,
            DiffAction::PageUp,
            DiffAction::HalfPageDown,
            DiffAction::HalfPageUp,
            DiffAction::Top,
            DiffAction::Bottom,
            DiffAction::CenterViewport,
            DiffAction::CenterCursor,
            DiffAction::NextChange,
            DiffAction::PrevChange,
            DiffAction::NextComment,
            DiffAction::PrevComment,
            DiffAction::NextEmptyLine,
            DiffAction::PrevEmptyLine,
            DiffAction::FindForward,
            DiffAction::FindBackward,
            DiffAction::TillForward,
            DiffAction::TillBackward,
            DiffAction::RepeatFind,
            DiffAction::Search,
            DiffAction::NextMatch,
            DiffAction::PrevMatch,
            DiffAction::FilePalette,
            DiffAction::CommandPalette,
            DiffAction::Help,
            DiffAction::ToggleSplit,
            DiffAction::NextHunkFilter,
            DiffAction::PrevHunkFilter,
            DiffAction::ToggleBlame,
            DiffAction::ToggleHunkFold,
            DiffAction::CloseHunkFold,
            DiffAction::OpenHunkFold,
            DiffAction::CloseFileFold,
            DiffAction::OpenFileFold,
            DiffAction::CloseAllFolds,
            DiffAction::OpenAllFolds,
            DiffAction::VisualSelect,
            DiffAction::Comment,
            DiffAction::ReplyOrRefresh,
            DiffAction::ToggleThread,
            DiffAction::DeleteComment,
            DiffAction::ClearComments,
            DiffAction::Yank,
            DiffAction::YankAll,
            DiffAction::SendToAgent,
            DiffAction::SendToReviewer,
            DiffAction::RevertHunk,
            DiffAction::RevertFile,
            DiffAction::OpenInEditor,
            DiffAction::ToggleViewed,
            DiffAction::PickCommit,
            DiffAction::ToggleFileList,
            DiffAction::ToggleResolved,
            DiffAction::ToggleCommentTarget,
            DiffAction::SubmitReview,
            DiffAction::PrInfo,
        ];
        const PALETTE: &[PaletteAction] = &[
            PaletteAction::Close,
            PaletteAction::Activate,
            PaletteAction::NavUp,
            PaletteAction::NavDown,
            PaletteAction::Backspace,
        ];
        GLOBAL
            .iter()
            .copied()
            .map(Action::Global)
            .chain(SIDEBAR.iter().copied().map(Action::Sidebar))
            .chain(KANBAN.iter().copied().map(Action::Kanban))
            .chain(POPOVER.iter().copied().map(Action::Popover))
            .chain(ACTIVITY.iter().copied().map(Action::Activity))
            .chain(DIFF.iter().copied().map(Action::Diff))
            .chain(PALETTE.iter().copied().map(Action::Palette))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_actions_have_at_least_one_default_chord() {
        for a in Action::all() {
            assert!(
                !a.default_chords().is_empty(),
                "action {a:?} has no default chord"
            );
        }
    }

    #[test]
    fn all_actions_have_a_known_mode() {
        for a in Action::all() {
            assert!(
                MODE_NAMES.contains(&a.mode()),
                "action {a:?} has unknown mode {}",
                a.mode()
            );
        }
    }

    #[test]
    fn all_actions_have_nonempty_description() {
        for a in Action::all() {
            assert!(!a.description().is_empty(), "{a:?} has empty description");
        }
    }

    #[test]
    fn all_actions_have_snake_case_key_name() {
        for a in Action::all() {
            let n = a.key_name();
            assert!(!n.is_empty(), "{a:?} has empty key_name");
            assert!(
                n.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{a:?} key_name {n} not snake_case"
            );
        }
    }
}
