//! Customizable keybindings — the action hierarchy.
//!
//! Every key in shelbi's TUI maps to an [`Action`]. Actions are split per
//! mode (global, sidebar, kanban, popover, review, activity, palette) so
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
/// of the seven TUI modes. Re-exports [`GlobalAction`] etc. through the
/// inner enums for direct construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Global(GlobalAction),
    Sidebar(SidebarAction),
    Kanban(KanbanAction),
    Popover(PopoverAction),
    Review(ReviewAction),
    Activity(ActivityAction),
    Palette(PaletteAction),
}

/// Actions that fire regardless of which view is focused (Ctrl+C, the
/// palette opener, the Zen toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalAction {
    Quit,
    ZenToggle,
    OpenPalette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarAction {
    Quit,
    NavUp,
    NavDown,
    Activate,
    Refresh,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PopoverAction {
    Close,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ScrollHome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewAction {
    NavUp,
    NavDown,
    ScrollBodyUp,
    ScrollBodyDown,
    PageBodyUp,
    PageBodyDown,
    ScrollBodyHome,
    Activate,
    Refresh,
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
    "global", "sidebar", "kanban", "popover", "review", "activity", "palette",
];

impl Action {
    /// Human-readable label for the help screen and palette hints.
    pub const fn description(&self) -> &'static str {
        match self {
            Action::Global(a) => match a {
                GlobalAction::Quit => "Quit shelbi",
                GlobalAction::ZenToggle => "Toggle Zen Mode",
                GlobalAction::OpenPalette => "Open command palette",
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => "Quit sidebar",
                SidebarAction::NavUp => "Sidebar: move selection up",
                SidebarAction::NavDown => "Sidebar: move selection down",
                SidebarAction::Activate => "Sidebar: activate selection",
                SidebarAction::Refresh => "Sidebar: refresh",
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
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => "Popover: close",
                PopoverAction::ScrollUp => "Popover: scroll up",
                PopoverAction::ScrollDown => "Popover: scroll down",
                PopoverAction::PageUp => "Popover: page up",
                PopoverAction::PageDown => "Popover: page down",
                PopoverAction::ScrollHome => "Popover: scroll to top",
            },
            Action::Review(a) => match a {
                ReviewAction::NavUp => "Review: move selection up",
                ReviewAction::NavDown => "Review: move selection down",
                ReviewAction::ScrollBodyUp => "Review: scroll body up",
                ReviewAction::ScrollBodyDown => "Review: scroll body down",
                ReviewAction::PageBodyUp => "Review: page body up",
                ReviewAction::PageBodyDown => "Review: page body down",
                ReviewAction::ScrollBodyHome => "Review: scroll body to top",
                ReviewAction::Activate => "Review: activate selection",
                ReviewAction::Refresh => "Review: refresh",
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
            Action::Palette(a) => match a {
                PaletteAction::Close => "Palette: close",
                PaletteAction::Activate => "Palette: activate selection",
                PaletteAction::NavUp => "Palette: move selection up",
                PaletteAction::NavDown => "Palette: move selection down",
                PaletteAction::Backspace => "Palette: delete last query char",
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
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => &["esc", "enter", "space", "q"],
                PopoverAction::ScrollUp => &["k", "up"],
                PopoverAction::ScrollDown => &["j", "down"],
                PopoverAction::PageUp => &["page-up", "u"],
                PopoverAction::PageDown => &["page-down", "d"],
                PopoverAction::ScrollHome => &["g", "home"],
            },
            Action::Review(a) => match a {
                ReviewAction::NavUp => &["k", "up"],
                ReviewAction::NavDown => &["j", "down"],
                ReviewAction::ScrollBodyUp => &["K"],
                ReviewAction::ScrollBodyDown => &["J"],
                ReviewAction::PageBodyUp => &["page-up", "u"],
                ReviewAction::PageBodyDown => &["page-down", "d"],
                ReviewAction::ScrollBodyHome => &["g", "home"],
                ReviewAction::Activate => &["enter", "space"],
                ReviewAction::Refresh => &["r"],
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
            Action::Review(_) => "review",
            Action::Activity(_) => "activity",
            Action::Palette(_) => "palette",
        }
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
            },
            Action::Sidebar(a) => match a {
                SidebarAction::Quit => "quit",
                SidebarAction::NavUp => "nav_up",
                SidebarAction::NavDown => "nav_down",
                SidebarAction::Activate => "activate",
                SidebarAction::Refresh => "refresh",
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
            },
            Action::Popover(a) => match a {
                PopoverAction::Close => "close",
                PopoverAction::ScrollUp => "scroll_up",
                PopoverAction::ScrollDown => "scroll_down",
                PopoverAction::PageUp => "page_up",
                PopoverAction::PageDown => "page_down",
                PopoverAction::ScrollHome => "scroll_home",
            },
            Action::Review(a) => match a {
                ReviewAction::NavUp => "nav_up",
                ReviewAction::NavDown => "nav_down",
                ReviewAction::ScrollBodyUp => "scroll_body_up",
                ReviewAction::ScrollBodyDown => "scroll_body_down",
                ReviewAction::PageBodyUp => "page_body_up",
                ReviewAction::PageBodyDown => "page_body_down",
                ReviewAction::ScrollBodyHome => "scroll_body_home",
                ReviewAction::Activate => "activate",
                ReviewAction::Refresh => "refresh",
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
        ];
        const SIDEBAR: &[SidebarAction] = &[
            SidebarAction::Quit,
            SidebarAction::NavUp,
            SidebarAction::NavDown,
            SidebarAction::Activate,
            SidebarAction::Refresh,
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
        ];
        const POPOVER: &[PopoverAction] = &[
            PopoverAction::Close,
            PopoverAction::ScrollUp,
            PopoverAction::ScrollDown,
            PopoverAction::PageUp,
            PopoverAction::PageDown,
            PopoverAction::ScrollHome,
        ];
        const REVIEW: &[ReviewAction] = &[
            ReviewAction::NavUp,
            ReviewAction::NavDown,
            ReviewAction::ScrollBodyUp,
            ReviewAction::ScrollBodyDown,
            ReviewAction::PageBodyUp,
            ReviewAction::PageBodyDown,
            ReviewAction::ScrollBodyHome,
            ReviewAction::Activate,
            ReviewAction::Refresh,
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
            .chain(REVIEW.iter().copied().map(Action::Review))
            .chain(ACTIVITY.iter().copied().map(Action::Activity))
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
