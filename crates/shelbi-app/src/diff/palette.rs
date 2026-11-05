//! The diff view's palette: files (`Ctrl-p`, `>` switches to commands),
//! commands (`:`, deleting the `>` goes back to files), and the commits to
//! review one at a time. The state and its keys live here; a client draws
//! [`DiffPalette::items`].

use shelbi_palette::Scorer;
use shelbi_state::keymap::{DiffAction, Key, KeyChord, Mods};

use super::backend::TargetKind;
use super::help::{action_label, applies};
use super::model::{Changeset, CommitScope};

/// What the palette lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Files,
    Commands,
    Commits,
}

/// What choosing an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteTarget {
    File(usize),
    Action(DiffAction),
    Scope(CommitScope),
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub label: String,
    /// What a row shows beside the label: a file's counts, a commit's author.
    pub detail: String,
    pub target: PaletteTarget,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteStep {
    Stay,
    Close,
    Choose(PaletteTarget),
}

/// The commands `:` lists, in order: what's useful without leaving the palette
/// (the moves are all faster on their keys).
const COMMANDS: &[DiffAction] = &[
    DiffAction::ToggleSplit,
    DiffAction::NextHunkFilter,
    DiffAction::ToggleBlame,
    DiffAction::ReplyOrRefresh,
    DiffAction::PickCommit,
    DiffAction::Search,
    DiffAction::ToggleViewed,
    DiffAction::ToggleFileList,
    DiffAction::CloseAllFolds,
    DiffAction::OpenAllFolds,
    DiffAction::YankAll,
    DiffAction::SendToAgent,
    DiffAction::SendToReviewer,
    DiffAction::RevertHunk,
    DiffAction::RevertFile,
    DiffAction::ClearComments,
    DiffAction::OpenInEditor,
    DiffAction::Help,
];

/// A command's name in the palette.
fn command_label(action: DiffAction, kind: TargetKind) -> String {
    match action {
        DiffAction::ReplyOrRefresh => "Refresh".to_string(),
        other => action_label(other, kind).to_string(),
    }
}

/// The palette's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPalette {
    pub mode: PaletteMode,
    /// The text typed. In files mode a leading `>` switches to commands.
    pub query: String,
    /// The selected row of [`Self::items`].
    pub selected: usize,
    /// What the commands are for: a review has no revert, a workspace no
    /// Reviewer.
    pub kind: TargetKind,
}

impl DiffPalette {
    pub fn new(mode: PaletteMode) -> Self {
        DiffPalette { mode, query: String::new(), selected: 0, kind: TargetKind::Workspace }
    }

    /// With the commands a `kind` target has.
    pub fn for_kind(mut self, kind: TargetKind) -> Self {
        self.kind = kind;
        self
    }

    /// The rows, best match first.
    pub fn items(&self, changeset: &Changeset) -> Vec<PaletteItem> {
        let mut all = match self.mode {
            PaletteMode::Files => changeset
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| PaletteItem {
                    label: f.path.clone(),
                    detail: format!("{} +{} −{}", f.status.letter(), f.additions, f.deletions),
                    target: PaletteTarget::File(i),
                })
                .collect::<Vec<_>>(),
            PaletteMode::Commands => COMMANDS
                .iter()
                .filter(|a| applies(**a, self.kind))
                .map(|a| PaletteItem {
                    label: command_label(*a, self.kind),
                    detail: String::new(),
                    target: PaletteTarget::Action(*a),
                })
                .collect(),
            PaletteMode::Commits => commit_items(changeset),
        };
        let query = self.query.trim();
        if query.is_empty() {
            return all;
        }
        let mut scorer = Scorer::new(query);
        let mut scored: Vec<(u16, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, item)| Some((scorer.score(&item.label)?, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let order: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
        let mut items: Vec<Option<PaletteItem>> = all.drain(..).map(Some).collect();
        order.into_iter().filter_map(|i| items[i].take()).collect()
    }

    /// A key.
    pub fn key(&mut self, chord: KeyChord, changeset: &Changeset) -> PaletteStep {
        let ctrl = chord.mods.contains(Mods::CONTROL);
        match chord.code {
            Key::Esc => return PaletteStep::Close,
            Key::Enter => {
                let items = self.items(changeset);
                return match items.into_iter().nth(self.selected) {
                    Some(item) => PaletteStep::Choose(item.target),
                    None => PaletteStep::Stay,
                };
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Char('p') if ctrl => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.step_down(changeset),
            Key::Char('n') if ctrl => self.step_down(changeset),
            Key::Backspace => {
                if self.query.is_empty() && self.mode == PaletteMode::Commands {
                    // Deleting the `>` goes back to files.
                    self.mode = PaletteMode::Files;
                } else {
                    self.query.pop();
                }
                self.selected = 0;
            }
            Key::Char('>') if self.query.is_empty() && self.mode == PaletteMode::Files => {
                self.mode = PaletteMode::Commands;
                self.selected = 0;
            }
            Key::Char(c) if !ctrl && !chord.mods.contains(Mods::SUPER) && !chord.mods.contains(Mods::ALT) => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PaletteStep::Stay
    }

    fn step_down(&mut self, changeset: &Changeset) {
        let count = self.items(changeset).len();
        if self.selected + 1 < count {
            self.selected += 1;
        }
    }
}

/// `All`, then uncommitted work where it can exist, then each commit.
fn commit_items(changeset: &Changeset) -> Vec<PaletteItem> {
    let mut out = vec![PaletteItem {
        label: "All changes".to_string(),
        detail: String::new(),
        target: PaletteTarget::Scope(CommitScope::All),
    }];
    out.push(PaletteItem {
        label: "Uncommitted changes".to_string(),
        detail: String::new(),
        target: PaletteTarget::Scope(CommitScope::Uncommitted),
    });
    out.extend(changeset.commits.iter().map(|c| PaletteItem {
        label: format!("{} {}", c.short, c.subject),
        detail: c.author.clone(),
        target: PaletteTarget::Scope(CommitScope::Commit(c.sha.clone())),
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::model::CommitInfo;
    use crate::diff::parse::parse_patch;

    fn changeset() -> Changeset {
        Changeset {
            files: parse_patch(
                "diff --git a/src/review.rs b/src/review.rs\n--- a/src/review.rs\n+++ b/src/review.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1 +1 @@\n-a\n+b\n",
            ),
            commits: vec![CommitInfo {
                sha: "abc123".into(),
                short: "abc".into(),
                author: "Ada".into(),
                time: 0,
                subject: "Fix it".into(),
            }],
            ..Changeset::default()
        }
    }

    fn ch(c: char) -> KeyChord {
        KeyChord::new(Key::Char(c), Mods::NONE)
    }

    #[test]
    fn typing_filters_files_and_enter_jumps() {
        let cs = changeset();
        let mut p = DiffPalette::new(PaletteMode::Files);
        assert_eq!(p.items(&cs).len(), 2);
        for c in "app".chars() {
            p.key(ch(c), &cs);
        }
        let items = p.items(&cs);
        assert_eq!(items[0].label, "src/app.rs");
        assert_eq!(items[0].detail, "M +1 −1");
        assert_eq!(p.key(KeyChord::new(Key::Enter, Mods::NONE), &cs), PaletteStep::Choose(PaletteTarget::File(1)));
    }

    #[test]
    fn a_leading_angle_switches_to_commands_and_backspace_returns() {
        let cs = changeset();
        let mut p = DiffPalette::new(PaletteMode::Files);
        p.key(ch('>'), &cs);
        assert_eq!(p.mode, PaletteMode::Commands);
        assert!(p.items(&cs).iter().any(|i| i.label == "Refresh"));
        p.key(KeyChord::new(Key::Backspace, Mods::NONE), &cs);
        assert_eq!(p.mode, PaletteMode::Files);
    }

    #[test]
    fn the_selection_stays_on_the_list() {
        let cs = changeset();
        let mut p = DiffPalette::new(PaletteMode::Files);
        let down = KeyChord::new(Key::Down, Mods::NONE);
        p.key(down, &cs);
        p.key(down, &cs);
        p.key(down, &cs);
        assert_eq!(p.selected, 1);
        p.key(KeyChord::new(Key::Char('p'), Mods::CONTROL), &cs);
        assert_eq!(p.selected, 0);
        assert_eq!(p.key(KeyChord::new(Key::Esc, Mods::NONE), &cs), PaletteStep::Close);
    }

    #[test]
    fn commits_list_all_uncommitted_and_each_commit() {
        let cs = changeset();
        let p = DiffPalette::new(PaletteMode::Commits);
        let items = p.items(&cs);
        assert_eq!(items[0].target, PaletteTarget::Scope(CommitScope::All));
        assert_eq!(items[1].target, PaletteTarget::Scope(CommitScope::Uncommitted));
        assert_eq!(items[2].label, "abc Fix it");
        assert_eq!(items[2].target, PaletteTarget::Scope(CommitScope::Commit("abc123".into())));
    }
}
