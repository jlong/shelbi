//! The Machines view's toolkit-free state: the shared [`MachinesModel`] (from
//! `shelbi_orchestrator::machines_model`, read off the UI thread), the live
//! reachability folded over it, and the workspace selection. `machines.rs`
//! draws it; tests drive it directly.
//!
//! Keys match the TUI's machines view: up / down (and `j` / `k`) move between
//! workspaces across machines, Enter focuses the selected one, `r` refreshes.

use std::collections::HashMap;

use shelbi_app::view::{MachineWorkspace, MachinesModel};
use shelbi_core::Reachability;
use shelbi_state::keymap::{Key, KeyChord};

/// What the view should do after a key or a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Focus this workspace (`CommandKind::FocusWorkspace`).
    Focus(String),
    Refresh,
}

#[derive(Default)]
pub struct MachinesState {
    pub project: String,
    model: MachinesModel,
    /// Whether a read has landed yet.
    loaded: bool,
    /// Why the last read failed (the project config didn't load).
    error: Option<String>,
    /// The latest probe results, re-applied over every read.
    reachability: HashMap<String, Reachability>,
    /// `(machine, workspace)` for every workspace, in render order.
    positions: Vec<(usize, usize)>,
    /// Index into `positions`.
    pub selected: Option<usize>,
}

impl MachinesState {
    pub fn new(project: impl Into<String>) -> Self {
        MachinesState {
            project: project.into(),
            ..MachinesState::default()
        }
    }

    /// Fold in a fresh read, keeping the selection on the same workspace.
    pub fn apply(&mut self, read: Result<MachinesModel, String>) {
        let selected = self.selected_workspace().map(|w| w.name.clone());
        self.loaded = true;
        match read {
            Ok(model) => {
                self.model = model;
                self.error = None;
            }
            Err(e) => {
                self.model = MachinesModel::default();
                self.error = Some(e);
            }
        }
        self.model.fold_reachability(&self.reachability);
        self.positions = self.model.workspace_positions();
        self.selected = selected.and_then(|name| {
            self.positions
                .iter()
                .position(|&(mi, wi)| self.model.machines[mi].workspaces[wi].name == name)
        });
    }

    /// Fold the prober's latest results in. Returns whether anything changed.
    pub fn set_reachability(&mut self, results: HashMap<String, Reachability>) -> bool {
        if results == self.reachability {
            return false;
        }
        self.reachability = results;
        self.model.fold_reachability(&self.reachability);
        true
    }

    /// The `(machine, ssh host)` pairs the prober should probe.
    pub fn probe_targets(&self) -> Vec<(String, String)> {
        self.model.probe_targets()
    }

    pub fn model(&self) -> &MachinesModel {
        &self.model
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The view's title: the project's display name, else its slug.
    pub fn title(&self) -> &str {
        self.model.display_name.as_deref().unwrap_or(&self.project)
    }

    /// The selection's position in render order, for the row painter.
    pub fn is_selected(&self, mi: usize, wi: usize) -> bool {
        self.selected
            .and_then(|s| self.positions.get(s))
            .is_some_and(|&p| p == (mi, wi))
    }

    pub fn selected_workspace(&self) -> Option<&MachineWorkspace> {
        let &(mi, wi) = self.positions.get(self.selected?)?;
        self.model.machines.get(mi)?.workspaces.get(wi)
    }

    fn step(&mut self, delta: isize) {
        if self.positions.is_empty() {
            return;
        }
        let last = self.positions.len() as isize - 1;
        self.selected = Some(match self.selected {
            Some(s) => (s as isize + delta).clamp(0, last) as usize,
            None => 0,
        });
    }

    /// A click on machine `mi`'s workspace `wi`: select it and focus it.
    pub fn click(&mut self, mi: usize, wi: usize) -> Vec<Intent> {
        let Some(pos) = self.positions.iter().position(|&p| p == (mi, wi)) else {
            return Vec::new();
        };
        self.selected = Some(pos);
        self.focus_selected()
    }

    fn focus_selected(&self) -> Vec<Intent> {
        self.selected_workspace()
            .map(|w| Intent::Focus(w.name.clone()))
            .into_iter()
            .collect()
    }

    /// A key on the focused view. `Some` when handled (the view stops
    /// propagation), `None` to let the window's global keys run.
    pub fn handle_key(&mut self, chord: KeyChord) -> Option<Vec<Intent>> {
        // Modified keys (Ctrl+H / Ctrl+L, ...) are the window's.
        if !chord.mods.is_empty() {
            return None;
        }
        match chord.code {
            Key::Up | Key::Char('k') => self.step(-1),
            Key::Down | Key::Char('j') => self.step(1),
            Key::Enter => return Some(self.focus_selected()),
            Key::Char('r') => return Some(vec![Intent::Refresh]),
            _ => return None,
        }
        Some(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{MachineRow, SlotAssignment};
    use shelbi_core::MachineKind;

    fn ws(name: &str) -> MachineWorkspace {
        MachineWorkspace {
            name: name.into(),
            is_review_slot: false,
            state: None,
            current_task: None,
            assignment: SlotAssignment::Idle,
            agent: None,
            task_title: None,
            branch: None,
            review: None,
        }
    }

    fn machine(name: &str, local: bool, workspaces: &[&str]) -> MachineRow {
        MachineRow {
            name: name.into(),
            kind: if local {
                MachineKind::Local
            } else {
                MachineKind::Ssh
            },
            host: (!local).then(|| format!("{name}.lan")),
            is_local: local,
            tags: vec![],
            remote: None,
            reachability: if local {
                Reachability::Reachable
            } else {
                Reachability::Unknown
            },
            workspaces: workspaces.iter().map(|w| ws(w)).collect(),
        }
    }

    fn model(machines: Vec<MachineRow>) -> MachinesModel {
        MachinesModel {
            display_name: Some("Demo".into()),
            machines,
        }
    }

    fn state() -> MachinesState {
        let mut s = MachinesState::new("demo");
        s.apply(Ok(model(vec![
            machine("local", true, &["alpha", "bravo"]),
            machine("gpu", false, &["gpu-1"]),
        ])));
        s
    }

    fn key(spec: &str) -> KeyChord {
        KeyChord::parse(spec).unwrap()
    }

    #[test]
    fn keys_walk_every_workspace_across_machines_and_enter_focuses() {
        let mut s = state();
        assert_eq!(s.title(), "Demo");
        s.handle_key(key("j"));
        assert_eq!(
            s.selected_workspace().map(|w| w.name.as_str()),
            Some("alpha")
        );
        s.handle_key(key("down"));
        s.handle_key(key("j"));
        assert_eq!(
            s.selected_workspace().map(|w| w.name.as_str()),
            Some("gpu-1")
        );
        s.handle_key(key("j"));
        assert_eq!(
            s.selected_workspace().map(|w| w.name.as_str()),
            Some("gpu-1"),
            "clamps"
        );
        assert_eq!(
            s.handle_key(key("enter")),
            Some(vec![Intent::Focus("gpu-1".into())])
        );
        assert_eq!(s.handle_key(key("r")), Some(vec![Intent::Refresh]));
        assert_eq!(
            s.handle_key(key("x")),
            None,
            "unbound keys reach the window"
        );
    }

    #[test]
    fn clicking_a_workspace_focuses_it() {
        let mut s = state();
        assert_eq!(s.click(0, 1), vec![Intent::Focus("bravo".into())]);
        assert!(s.is_selected(0, 1));
        assert!(s.click(5, 0).is_empty());
    }

    #[test]
    fn reachability_survives_reads_and_only_touches_remotes() {
        let mut s = state();
        assert_eq!(
            s.probe_targets(),
            vec![("gpu".to_string(), "gpu.lan".to_string())]
        );
        assert_eq!(
            s.model().machines[1].reachability,
            Reachability::Unknown,
            "checking…"
        );
        let mut results = HashMap::new();
        results.insert(
            "gpu".to_string(),
            Reachability::Unreachable {
                error: "timed out".into(),
            },
        );
        assert!(s.set_reachability(results.clone()));
        assert!(
            !s.set_reachability(results),
            "unchanged results don't repaint"
        );
        // A later read (which starts remotes at Unknown) keeps the probe result.
        s.apply(Ok(model(vec![
            machine("local", true, &["alpha", "bravo"]),
            machine("gpu", false, &["gpu-1"]),
        ])));
        assert!(matches!(
            s.model().machines[1].reachability,
            Reachability::Unreachable { .. }
        ));
        assert_eq!(s.model().machines[0].reachability, Reachability::Reachable);
    }

    #[test]
    fn the_selection_follows_its_workspace_and_a_failed_read_shows_why() {
        let mut s = state();
        s.click(1, 0);
        s.apply(Ok(model(vec![
            machine("local", true, &["alpha", "bravo", "charlie"]),
            machine("gpu", false, &["gpu-1"]),
        ])));
        assert_eq!(
            s.selected_workspace().map(|w| w.name.as_str()),
            Some("gpu-1")
        );
        s.apply(Err("project `demo` not found".into()));
        assert_eq!(s.error(), Some("project `demo` not found"));
        assert!(s.model().machines.is_empty());
        assert!(s.selected_workspace().is_none());
    }
}
