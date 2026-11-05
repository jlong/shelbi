//! The keymap's contexts (its modes) as a person reads them: a title, when
//! the bindings apply, and the actions in the order a list of them shows
//! (the desktop's Settings ▸ Keybindings page, Figma frame 2252:28172).
//!
//! [`Action::all`] stays the stable, declaration-ordered iteration the loader
//! and emitters use; [`CONTEXTS`] is the reading order. Every action appears
//! in exactly one context (tested below).

use super::actions::{
    Action, ActivityAction, DiffAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    SidebarAction,
};

/// One keymap mode, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeymapContext {
    /// The mode's name, as `keys.yaml` and [`Action::mode`] spell it.
    pub mode: &'static str,
    /// The group's title (`Issues board`).
    pub title: &'static str,
    /// When its bindings apply (`While the issues board is focused`).
    pub hint: &'static str,
    /// Its actions, in reading order.
    pub actions: &'static [Action],
}

/// Every context, in reading order: global first, then the views, then the
/// modal ones.
pub const CONTEXTS: &[KeymapContext] = &[
    KeymapContext {
        mode: "global",
        title: "Global",
        hint: "Available throughout Shelbi",
        actions: &[
            Action::Global(GlobalAction::OpenPalette),
            Action::Global(GlobalAction::FocusSidebar),
            Action::Global(GlobalAction::FocusMain),
            Action::Global(GlobalAction::ZenToggle),
            Action::Global(GlobalAction::Quit),
        ],
    },
    KeymapContext {
        mode: "sidebar",
        title: "Sidebar",
        hint: "While the sidebar is focused",
        actions: &[
            Action::Sidebar(SidebarAction::NavUp),
            Action::Sidebar(SidebarAction::NavDown),
            Action::Sidebar(SidebarAction::Activate),
            Action::Sidebar(SidebarAction::Refresh),
            Action::Sidebar(SidebarAction::OpenSettings),
            Action::Sidebar(SidebarAction::Quit),
        ],
    },
    KeymapContext {
        mode: "kanban",
        title: "Issues board",
        hint: "While the issues board is focused",
        actions: &[
            Action::Kanban(KanbanAction::NavLeft),
            Action::Kanban(KanbanAction::NavRight),
            Action::Kanban(KanbanAction::NavUp),
            Action::Kanban(KanbanAction::NavDown),
            Action::Kanban(KanbanAction::MoveCardLeft),
            Action::Kanban(KanbanAction::MoveCardRight),
            Action::Kanban(KanbanAction::ReorderUp),
            Action::Kanban(KanbanAction::ReorderDown),
            Action::Kanban(KanbanAction::OpenPopover),
            Action::Kanban(KanbanAction::OpenWorkspace),
            Action::Kanban(KanbanAction::CycleWorkflowFilter),
            Action::Kanban(KanbanAction::Refresh),
        ],
    },
    KeymapContext {
        mode: "popover",
        title: "Issue details",
        hint: "While issue details are open",
        actions: &[
            Action::Popover(PopoverAction::Close),
            Action::Popover(PopoverAction::ScrollUp),
            Action::Popover(PopoverAction::ScrollDown),
            Action::Popover(PopoverAction::PageUp),
            Action::Popover(PopoverAction::PageDown),
            Action::Popover(PopoverAction::ScrollHome),
            Action::Popover(PopoverAction::MoveLeft),
            Action::Popover(PopoverAction::MoveRight),
            Action::Popover(PopoverAction::OpenWorkspace),
            Action::Popover(PopoverAction::OpenPr),
        ],
    },
    KeymapContext {
        mode: "activity",
        title: "Activity",
        hint: "While the activity feed is focused",
        actions: &[
            Action::Activity(ActivityAction::ScrollUp),
            Action::Activity(ActivityAction::ScrollDown),
            Action::Activity(ActivityAction::PageUp),
            Action::Activity(ActivityAction::PageDown),
            Action::Activity(ActivityAction::ScrollHome),
            Action::Activity(ActivityAction::ScrollEnd),
            Action::Activity(ActivityAction::Refresh),
            Action::Activity(ActivityAction::ResetFilter),
            Action::Activity(ActivityAction::ToggleZenFilter),
            Action::Activity(ActivityAction::ToggleWorkspacesFilter),
        ],
    },
    KeymapContext {
        mode: "diff",
        title: "Diff view",
        hint: "While the diff view is focused",
        actions: &[
            Action::Diff(DiffAction::CursorDown),
            Action::Diff(DiffAction::CursorUp),
            Action::Diff(DiffAction::PrevFile),
            Action::Diff(DiffAction::NextFile),
            Action::Diff(DiffAction::PageDown),
            Action::Diff(DiffAction::PageUp),
            Action::Diff(DiffAction::HalfPageDown),
            Action::Diff(DiffAction::HalfPageUp),
            Action::Diff(DiffAction::Top),
            Action::Diff(DiffAction::Bottom),
            Action::Diff(DiffAction::CenterViewport),
            Action::Diff(DiffAction::CenterCursor),
            Action::Diff(DiffAction::NextChange),
            Action::Diff(DiffAction::PrevChange),
            Action::Diff(DiffAction::NextComment),
            Action::Diff(DiffAction::PrevComment),
            Action::Diff(DiffAction::NextEmptyLine),
            Action::Diff(DiffAction::PrevEmptyLine),
            Action::Diff(DiffAction::FindForward),
            Action::Diff(DiffAction::FindBackward),
            Action::Diff(DiffAction::TillForward),
            Action::Diff(DiffAction::TillBackward),
            Action::Diff(DiffAction::RepeatFind),
            Action::Diff(DiffAction::Search),
            Action::Diff(DiffAction::NextMatch),
            Action::Diff(DiffAction::PrevMatch),
            Action::Diff(DiffAction::FilePalette),
            Action::Diff(DiffAction::CommandPalette),
            Action::Diff(DiffAction::Help),
            Action::Diff(DiffAction::ToggleSplit),
            Action::Diff(DiffAction::NextHunkFilter),
            Action::Diff(DiffAction::PrevHunkFilter),
            Action::Diff(DiffAction::ToggleBlame),
            Action::Diff(DiffAction::ToggleHunkFold),
            Action::Diff(DiffAction::CloseHunkFold),
            Action::Diff(DiffAction::OpenHunkFold),
            Action::Diff(DiffAction::CloseFileFold),
            Action::Diff(DiffAction::OpenFileFold),
            Action::Diff(DiffAction::CloseAllFolds),
            Action::Diff(DiffAction::OpenAllFolds),
            Action::Diff(DiffAction::VisualSelect),
            Action::Diff(DiffAction::Comment),
            Action::Diff(DiffAction::ReplyOrRefresh),
            Action::Diff(DiffAction::ToggleThread),
            Action::Diff(DiffAction::DeleteComment),
            Action::Diff(DiffAction::ClearComments),
            Action::Diff(DiffAction::Yank),
            Action::Diff(DiffAction::YankAll),
            Action::Diff(DiffAction::SendToAgent),
            Action::Diff(DiffAction::SendToReviewer),
            Action::Diff(DiffAction::RevertHunk),
            Action::Diff(DiffAction::RevertFile),
            Action::Diff(DiffAction::OpenInEditor),
            Action::Diff(DiffAction::ToggleViewed),
            Action::Diff(DiffAction::PickCommit),
            Action::Diff(DiffAction::ToggleFileList),
        ],
    },
    KeymapContext {
        mode: "palette",
        title: "Command palette",
        hint: "While the command palette is open",
        actions: &[
            Action::Palette(PaletteAction::Close),
            Action::Palette(PaletteAction::Activate),
            Action::Palette(PaletteAction::NavUp),
            Action::Palette(PaletteAction::NavDown),
            Action::Palette(PaletteAction::Backspace),
        ],
    },
];

/// The context for `mode`, if it is one.
pub fn context(mode: &str) -> Option<&'static KeymapContext> {
    CONTEXTS.iter().find(|c| c.mode == mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::MODE_NAMES;

    #[test]
    fn every_mode_has_one_context_in_mode_order() {
        let modes: Vec<&str> = CONTEXTS.iter().map(|c| c.mode).collect();
        assert_eq!(modes, MODE_NAMES);
    }

    #[test]
    fn every_action_is_listed_once_in_its_own_context() {
        for action in Action::all() {
            let homes: Vec<&str> = CONTEXTS
                .iter()
                .filter(|c| c.actions.contains(&action))
                .map(|c| c.mode)
                .collect();
            assert_eq!(homes, [action.mode()], "{action:?}");
        }
        let listed: usize = CONTEXTS.iter().map(|c| c.actions.len()).sum();
        assert_eq!(listed, Action::all().count());
    }

    #[test]
    fn labels_are_unique_within_a_context() {
        for c in CONTEXTS {
            let mut labels: Vec<&str> = c.actions.iter().map(|a| a.label()).collect();
            labels.sort_unstable();
            labels.dedup();
            assert_eq!(labels.len(), c.actions.len(), "{}", c.mode);
        }
    }
}
