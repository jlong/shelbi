//! The workspace panel's navigation, shared by both clients: its rows, the
//! selection over them, what activating one asks the host to do, and its
//! keys.
//!
//! The panel stands in the sidebar's column while a workspace is open (the
//! TUI's `rt-workspace-sidebar`, the desktop's `dt-workspace-sidebar`), as the
//! review panel does ([`crate::review`]). The TUI's panel
//! (`shelbi-tui/src/workspace_panel.rs`) builds its rows with [`panel_rows`]
//! and activates them with [`activate_row`]; the desktop's Workspace view
//! holds a [`WorkspacePanelState`] over the same functions. The clients
//! differ only in the rows they ask for: the desktop adds the user-shell
//! toggle and a reduced panel for an idle workspace, which the TUI doesn't
//! have (it opens the panel only for a workspace with a task).
//!
//! Nothing here does IO: activating a row returns a [`WsEffect`] the host
//! carries out. Edit and View Diff open a pane, or with an app chosen for
//! them (`editor:` / `diff_tool:`, [`AppTools`]) launch the app
//! ([`WsEffect::OpenApp`]) and leave the main area as it was.

use shelbi_state::keymap::{Key, KeyChord, Mods};
use shelbi_state::AppTools;

use crate::review::step_selection;
use crate::ReviewRole;

/// One entry of the view switch: what the main area shows for the
/// workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsSwitch {
    /// The workspace's agent session (the default).
    Agent,
    /// The daemon's diff session for the worktree.
    Diff,
    /// The daemon's editor session for the worktree.
    Editor,
}

impl WsSwitch {
    /// The daemon-spawned content role it shows (`None` for the agent).
    pub fn role(self) -> Option<ReviewRole> {
        match self {
            WsSwitch::Agent => None,
            WsSwitch::Diff => Some(ReviewRole::Diff),
            WsSwitch::Editor => Some(ReviewRole::Editor),
        }
    }

    /// Its label: the agent's display name (`Developer`), `View Diff`, or
    /// `Edit in <editor>`.
    pub fn label(self, agent_name: &str, editor_name: &str) -> String {
        match self {
            WsSwitch::Agent => agent_name.to_string(),
            WsSwitch::Diff => "View Diff".to_string(),
            WsSwitch::Editor => format!("Edit in {editor_name}"),
        }
    }
}

/// One selectable panel row, top to bottom. The status, the task's title and
/// description preview, and the branch and machine are not rows; the `More`
/// link and the folder are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsRow {
    /// The back button at the top.
    Back,
    /// The `More` link: the full task description. Only with a body.
    More,
    /// The worktree folder: reveal it in the file manager.
    Folder,
    Switch(WsSwitch),
    /// The user shell beside the agent, on or off (desktop).
    Shell,
}

/// What a panel interaction asks the host to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsEffect {
    None,
    /// Go back to the navigation sidebar, leaving the content sessions
    /// loaded.
    Back,
    /// End the diff and editor sessions, then go back (the TUI's `q`).
    Close,
    RevealFolder,
    ShowAgent,
    ShowDiff,
    ShowEditor,
    /// Open the worktree (the editor) or its diff in the app chosen for
    /// that role. The main area keeps what it shows.
    OpenApp(ReviewRole),
    ShowDescription,
    /// Open or close the user shell beside the agent.
    ToggleShell,
}

/// Which rows a panel has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsPanelShape {
    /// The task has a description: `More` is offered.
    pub has_body: bool,
    /// The worktree can be revealed (a local workspace).
    pub folder: bool,
    /// The view switch's entries, in order (none for an idle workspace).
    pub switches: Vec<WsSwitch>,
    /// The user-shell toggle is offered.
    pub shell: bool,
    /// Which of Edit and View Diff open an app.
    pub apps: AppTools,
}

impl WsPanelShape {
    /// A workspace's panel: with a task, `More` (when it has a body), the
    /// view switch (the agent, plus View Diff and Edit in <editor> when
    /// local) and, when `shell`, the shell toggle; idle, only the back button
    /// and the folder. The folder only when local.
    pub fn for_workspace(has_task: bool, has_body: bool, local: bool, shell: bool) -> Self {
        let switches = match (has_task, local) {
            (false, _) => Vec::new(),
            (true, true) => vec![WsSwitch::Agent, WsSwitch::Diff, WsSwitch::Editor],
            (true, false) => vec![WsSwitch::Agent],
        };
        WsPanelShape {
            has_body: has_task && has_body,
            folder: local,
            switches,
            shell: has_task && shell,
            apps: AppTools::default(),
        }
    }

    /// The shape with Edit and View Diff opening `apps`.
    pub fn with_apps(mut self, apps: AppTools) -> Self {
        self.apps = apps;
        self
    }
}

/// The panel's rows: Back, `More`, the folder, the view switch, the shell.
pub fn panel_rows(shape: &WsPanelShape) -> Vec<WsRow> {
    let mut rows = vec![WsRow::Back];
    if shape.has_body {
        rows.push(WsRow::More);
    }
    if shape.folder {
        rows.push(WsRow::Folder);
    }
    rows.extend(shape.switches.iter().map(|s| WsRow::Switch(*s)));
    if shape.shell {
        rows.push(WsRow::Shell);
    }
    rows
}

/// What activating `row` does, and the switch entry it makes active. Edit
/// and View Diff with an app chosen (`apps`) launch it and make nothing
/// active: the main area keeps its view.
pub fn activate_row(row: WsRow, has_body: bool, apps: AppTools) -> (WsEffect, Option<WsSwitch>) {
    match row {
        WsRow::Back => (WsEffect::Back, None),
        WsRow::More => (description_effect(has_body), None),
        WsRow::Folder => (WsEffect::RevealFolder, None),
        WsRow::Switch(WsSwitch::Agent) => (WsEffect::ShowAgent, Some(WsSwitch::Agent)),
        WsRow::Switch(WsSwitch::Diff) if apps.diff => (WsEffect::OpenApp(ReviewRole::Diff), None),
        WsRow::Switch(WsSwitch::Diff) => (WsEffect::ShowDiff, Some(WsSwitch::Diff)),
        WsRow::Switch(WsSwitch::Editor) if apps.editor => (WsEffect::OpenApp(ReviewRole::Editor), None),
        WsRow::Switch(WsSwitch::Editor) => (WsEffect::ShowEditor, Some(WsSwitch::Editor)),
        WsRow::Shell => (WsEffect::ToggleShell, None),
    }
}

/// The description popover opens only when there is a body to show.
pub fn description_effect(has_body: bool) -> WsEffect {
    if has_body {
        WsEffect::ShowDescription
    } else {
        WsEffect::None
    }
}

/// The panel's interactive state: its rows, the selection, and the active
/// switch entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePanelState {
    rows: Vec<WsRow>,
    selected: usize,
    active: WsSwitch,
    has_body: bool,
    apps: AppTools,
}

impl WorkspacePanelState {
    /// The panel for `shape`, with the agent selected and active (the
    /// default content), or the back button when there is no switch.
    pub fn new(shape: &WsPanelShape) -> Self {
        let rows = panel_rows(shape);
        let selected = rows
            .iter()
            .position(|r| *r == WsRow::Switch(WsSwitch::Agent))
            .unwrap_or(0);
        WorkspacePanelState {
            rows,
            selected,
            active: WsSwitch::Agent,
            has_body: shape.has_body,
            apps: shape.apps,
        }
    }

    /// Follow a new shape (a task started or ended, its body resolved): the
    /// selection stays on the same row when it still exists. A view switch
    /// that appears (a task started) takes the selection, as in a new panel.
    pub fn sync(&mut self, shape: &WsPanelShape) {
        let current = self.selected_row();
        self.apps = shape.apps;
        let rows = panel_rows(shape);
        if rows == self.rows && shape.has_body == self.has_body {
            return;
        }
        let agent = WsRow::Switch(WsSwitch::Agent);
        let gained_agent = !self.rows.contains(&agent) && rows.contains(&agent);
        self.rows = rows;
        self.has_body = shape.has_body;
        let keep = if gained_agent { Some(agent) } else { current };
        self.selected = keep
            .and_then(|row| self.rows.iter().position(|r| *r == row))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    pub fn rows(&self) -> &[WsRow] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_row(&self) -> Option<WsRow> {
        self.rows.get(self.selected).copied()
    }

    /// The switch entry the main area shows.
    pub fn active(&self) -> WsSwitch {
        self.active
    }

    /// The host fell back to another entry (a content view that failed).
    pub fn set_active(&mut self, switch: WsSwitch) {
        self.active = switch;
    }

    /// Move the selection up (`-1`) or down (`+1`), wrapping.
    pub fn nav(&mut self, delta: i32) {
        self.selected = step_selection(self.selected, self.rows.len(), delta);
    }

    /// Activate the selected row (Enter / Space).
    pub fn activate(&mut self) -> WsEffect {
        match self.selected_row() {
            Some(row) => self.activate_index(self.selected, row),
            None => WsEffect::None,
        }
    }

    /// Select and activate `row` (a click). A row the panel doesn't have
    /// does nothing.
    pub fn click(&mut self, row: WsRow) -> WsEffect {
        match self.rows.iter().position(|r| *r == row) {
            Some(idx) => self.activate_index(idx, row),
            None => WsEffect::None,
        }
    }

    fn activate_index(&mut self, idx: usize, row: WsRow) -> WsEffect {
        self.selected = idx;
        let (effect, switch) = activate_row(row, self.has_body, self.apps);
        if let Some(switch) = switch {
            self.active = switch;
        }
        effect
    }

    /// One key while the panel has focus: `↑`/`k` and `↓`/`j` move, Enter and
    /// Space activate, `m` opens the description, Esc goes back (leaving the
    /// content loaded) and `q` closes the content and goes back, as in the
    /// TUI. Chords with Ctrl, Alt or Cmd are not the panel's.
    pub fn handle_key(&mut self, chord: KeyChord) -> WsEffect {
        if chord.mods.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER) {
            return WsEffect::None;
        }
        match chord.code {
            Key::Up | Key::Char('k') => {
                self.nav(-1);
                WsEffect::None
            }
            Key::Down | Key::Char('j') => {
                self.nav(1);
                WsEffect::None
            }
            Key::Enter | Key::Char(' ') => self.activate(),
            Key::Char('m') => description_effect(self.has_body),
            Key::Esc => WsEffect::Back,
            Key::Char('q') => WsEffect::Close,
            _ => WsEffect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(code: Key) -> KeyChord {
        KeyChord {
            code,
            mods: Mods::NONE,
        }
    }

    #[test]
    fn a_local_workspace_with_a_task_has_every_row() {
        let shape = WsPanelShape::for_workspace(true, true, true, true);
        assert_eq!(
            panel_rows(&shape),
            [
                WsRow::Back,
                WsRow::More,
                WsRow::Folder,
                WsRow::Switch(WsSwitch::Agent),
                WsRow::Switch(WsSwitch::Diff),
                WsRow::Switch(WsSwitch::Editor),
                WsRow::Shell,
            ]
        );
        let panel = WorkspacePanelState::new(&shape);
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Agent)), "the agent is selected first");
        assert_eq!(panel.active(), WsSwitch::Agent);
    }

    #[test]
    fn a_remote_workspace_has_no_folder_diff_or_editor() {
        let shape = WsPanelShape::for_workspace(true, false, false, true);
        assert_eq!(
            panel_rows(&shape),
            [WsRow::Back, WsRow::Switch(WsSwitch::Agent), WsRow::Shell]
        );
    }

    #[test]
    fn an_idle_workspace_has_only_back_and_the_folder() {
        let shape = WsPanelShape::for_workspace(false, true, true, true);
        assert_eq!(panel_rows(&shape), [WsRow::Back, WsRow::Folder]);
        let mut panel = WorkspacePanelState::new(&shape);
        assert_eq!(panel.selected_row(), Some(WsRow::Back));
        assert_eq!(panel.handle_key(chord(Key::Char('m'))), WsEffect::None, "no description");
    }

    #[test]
    fn keys_move_wrap_and_activate() {
        let shape = WsPanelShape::for_workspace(true, true, true, true);
        let mut panel = WorkspacePanelState::new(&shape);
        assert_eq!(panel.handle_key(chord(Key::Down)), WsEffect::None);
        assert_eq!(panel.handle_key(chord(Key::Enter)), WsEffect::ShowDiff);
        assert_eq!(panel.active(), WsSwitch::Diff);
        assert_eq!(panel.handle_key(chord(Key::Char('j'))), WsEffect::None);
        assert_eq!(panel.handle_key(chord(Key::Char('j'))), WsEffect::None);
        assert_eq!(panel.selected_row(), Some(WsRow::Shell));
        assert_eq!(panel.handle_key(chord(Key::Char(' '))), WsEffect::ToggleShell);
        assert_eq!(panel.active(), WsSwitch::Diff, "the shell is not a switch entry");
        panel.nav(1);
        assert_eq!(panel.selected_row(), Some(WsRow::Back), "wraps to the top");
        assert_eq!(panel.handle_key(chord(Key::Char('k'))), WsEffect::None);
        assert_eq!(panel.selected_row(), Some(WsRow::Shell), "and to the bottom");
        assert_eq!(panel.handle_key(chord(Key::Char('m'))), WsEffect::ShowDescription);
        assert_eq!(panel.handle_key(chord(Key::Esc)), WsEffect::Back);
        assert_eq!(panel.handle_key(chord(Key::Char('q'))), WsEffect::Close);
        let cmd = KeyChord {
            code: Key::Char('k'),
            mods: Mods::SUPER,
        };
        assert_eq!(panel.handle_key(cmd), WsEffect::None);
        assert_eq!(panel.selected_row(), Some(WsRow::Shell), "a Cmd chord is not the panel's");
    }

    #[test]
    fn a_click_selects_and_activates_its_row() {
        let shape = WsPanelShape::for_workspace(true, true, true, true);
        let mut panel = WorkspacePanelState::new(&shape);
        assert_eq!(panel.click(WsRow::Switch(WsSwitch::Editor)), WsEffect::ShowEditor);
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Editor)));
        assert_eq!(panel.active(), WsSwitch::Editor);
        assert_eq!(panel.click(WsRow::Folder), WsEffect::RevealFolder);
        assert_eq!(panel.click(WsRow::More), WsEffect::ShowDescription);
        // A row the panel doesn't have.
        let mut idle = WorkspacePanelState::new(&WsPanelShape::for_workspace(false, false, true, true));
        assert_eq!(idle.click(WsRow::Shell), WsEffect::None);
    }

    #[test]
    fn a_new_shape_keeps_the_selection_on_its_row() {
        let mut panel = WorkspacePanelState::new(&WsPanelShape::for_workspace(true, false, true, true));
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Agent)));
        // The body resolved: More appears above, the selection follows the agent.
        panel.sync(&WsPanelShape::for_workspace(true, true, true, true));
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Agent)));
        // The task ended: the agent row is gone, the selection clamps.
        panel.sync(&WsPanelShape::for_workspace(false, false, true, true));
        assert_eq!(panel.rows(), [WsRow::Back, WsRow::Folder]);
        assert_eq!(panel.selected_row(), Some(WsRow::Folder));
        // A new task: the agent takes the selection again.
        panel.sync(&WsPanelShape::for_workspace(true, false, true, true));
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Agent)));
    }

    #[test]
    fn an_app_editor_or_diff_launches_and_keeps_the_view() {
        let apps = AppTools { editor: true, diff: false };
        let shape = WsPanelShape::for_workspace(true, true, true, false).with_apps(apps);
        let mut panel = WorkspacePanelState::new(&shape);
        assert_eq!(panel.click(WsRow::Switch(WsSwitch::Editor)), WsEffect::OpenApp(ReviewRole::Editor));
        assert_eq!(panel.active(), WsSwitch::Agent, "the agent stays shown");
        assert_eq!(panel.selected_row(), Some(WsRow::Switch(WsSwitch::Editor)));
        assert_eq!(panel.click(WsRow::Switch(WsSwitch::Diff)), WsEffect::ShowDiff, "the diff is still a pane");
        assert_eq!(panel.active(), WsSwitch::Diff);

        // The setting changed: a synced shape takes it.
        panel.sync(&shape.clone().with_apps(AppTools { editor: false, diff: true }));
        assert_eq!(panel.click(WsRow::Switch(WsSwitch::Diff)), WsEffect::OpenApp(ReviewRole::Diff));
        assert_eq!(panel.active(), WsSwitch::Diff, "still the pane it showed");
        assert_eq!(panel.click(WsRow::Switch(WsSwitch::Editor)), WsEffect::ShowEditor);
    }

    #[test]
    fn the_switch_labels_name_the_agent_and_the_editor() {
        assert_eq!(WsSwitch::Agent.label("Developer", "Vim"), "Developer");
        assert_eq!(WsSwitch::Diff.label("Developer", "Vim"), "View Diff");
        assert_eq!(WsSwitch::Editor.label("Developer", "Vim"), "Edit in Vim");
        assert_eq!(WsSwitch::Diff.role(), Some(ReviewRole::Diff));
        assert_eq!(WsSwitch::Agent.role(), None);
    }
}
