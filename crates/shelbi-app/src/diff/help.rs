//! The `?` overlay's contents: every action in the `diff` context, grouped,
//! with the keys it has now. Built from the live keymap, so a rebound key
//! shows as rebound. How a chord is spelled is the client's.

use shelbi_state::keymap::{Action, DiffAction, KeyChord, ModeKeymap};

use super::backend::TargetKind;

/// One action's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRow {
    pub action: DiffAction,
    pub label: &'static str,
    /// Its keys, spelled by the client. Empty when it's unbound.
    pub keys: Vec<String>,
}

/// A group of rows under a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: &'static str,
    pub rows: Vec<HelpRow>,
}

/// The groups, in the order the overlay shows them.
pub const SECTIONS: &[(&str, &[DiffAction])] = &[
    (
        "Move",
        &[
            DiffAction::CursorDown,
            DiffAction::CursorUp,
            DiffAction::NextFile,
            DiffAction::PrevFile,
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
        ],
    ),
    (
        "Find",
        &[
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
        ],
    ),
    (
        "View",
        &[
            DiffAction::ToggleSplit,
            DiffAction::NextHunkFilter,
            DiffAction::PrevHunkFilter,
            DiffAction::ToggleBlame,
            DiffAction::ToggleFileList,
            DiffAction::PickCommit,
            DiffAction::ToggleViewed,
        ],
    ),
    (
        "Fold",
        &[
            DiffAction::ToggleHunkFold,
            DiffAction::CloseHunkFold,
            DiffAction::OpenHunkFold,
            DiffAction::CloseFileFold,
            DiffAction::OpenFileFold,
            DiffAction::CloseAllFolds,
            DiffAction::OpenAllFolds,
        ],
    ),
    (
        "Comment",
        &[
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
        ],
    ),
    ("Revert", &[DiffAction::RevertHunk, DiffAction::RevertFile]),
    ("Other", &[DiffAction::OpenInEditor, DiffAction::Help]),
];

/// The overlay's rows for `keymap`, each chord spelled by `spell`.
pub fn help_sections(
    keymap: &ModeKeymap<DiffAction>,
    kind: TargetKind,
    spell: impl Fn(&KeyChord) -> String,
) -> Vec<HelpSection> {
    SECTIONS
        .iter()
        .map(|(title, actions)| HelpSection {
            title,
            rows: actions
                .iter()
                .filter(|action| applies(**action, kind))
                .map(|action| HelpRow {
                    action: *action,
                    label: action_label(*action, kind),
                    keys: keymap.chords_for(*action).into_iter().map(&spell).collect(),
                })
                .collect(),
        })
        .filter(|section: &HelpSection| !section.rows.is_empty())
        .collect()
}

/// Whether `action` means anything on a `kind` target. A review has no worktree
/// of its own to revert, and a workspace has no Reviewer.
pub fn applies(action: DiffAction, kind: TargetKind) -> bool {
    match action {
        DiffAction::RevertHunk | DiffAction::RevertFile => kind == TargetKind::Workspace,
        DiffAction::SendToReviewer => kind == TargetKind::Review,
        _ => true,
    }
}

/// `action`'s name on a `kind` target: in a review, `gY` sends the comments back
/// through Reject.
pub fn action_label(action: DiffAction, kind: TargetKind) -> &'static str {
    match (action, kind) {
        (DiffAction::SendToAgent, TargetKind::Review) => "Reject with the comments as its reason",
        _ => Action::Diff(action).label(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::validate_keymaps_yaml;

    #[test]
    fn a_review_lists_no_revert_and_a_workspace_no_reviewer() {
        let km = validate_keymaps_yaml("", None).0.diff;
        let rows = |kind| -> Vec<DiffAction> {
            help_sections(&km, kind, |c| c.canonical()).iter().flat_map(|s| s.rows.iter().map(|r| r.action)).collect()
        };
        let (review, workspace) = (rows(TargetKind::Review), rows(TargetKind::Workspace));
        assert!(review.contains(&DiffAction::SendToReviewer) && !review.contains(&DiffAction::RevertFile));
        assert!(workspace.contains(&DiffAction::RevertHunk) && !workspace.contains(&DiffAction::SendToReviewer));
        assert_eq!(action_label(DiffAction::SendToAgent, TargetKind::Review), "Reject with the comments as its reason");
    }

    #[test]
    fn every_action_is_listed_once() {
        let listed: Vec<Action> = SECTIONS
            .iter()
            .flat_map(|(_, actions)| actions.iter().map(|a| Action::Diff(*a)))
            .collect();
        for action in Action::all().filter(|a| a.mode() == "diff") {
            assert_eq!(listed.iter().filter(|a| **a == action).count(), 1, "{action:?}");
        }
        assert_eq!(listed.len(), Action::all().filter(|a| a.mode() == "diff").count());
    }

    #[test]
    fn it_shows_the_keys_in_effect() {
        let km = validate_keymaps_yaml("defaults:\n  diff:\n    cursor_down: e\n    top: 'x y'\n    help: []\n", None).0.diff;
        let sections = help_sections(&km, TargetKind::Workspace, |c| c.canonical());
        let row = |action| {
            sections.iter().flat_map(|s| &s.rows).find(|r| r.action == action).unwrap().clone()
        };
        assert_eq!(row(DiffAction::CursorDown).keys, ["e"]);
        assert_eq!(row(DiffAction::Top).keys, ["x y"]);
        assert_eq!(row(DiffAction::Help).keys, Vec::<String>::new());
        assert_eq!(row(DiffAction::SendToAgent).keys, ["g shift-y"]);
    }
}
