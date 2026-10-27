//! Settings ▸ Workspaces' state, toolkit-free (`dt-workspace-settings-index`):
//! the project's workspace pool and the machines that host it, as the
//! project config declares them, with each remote's live SSH reachability
//! folded over.
//!
//! [`PoolIndex::build`] maps a loaded [`Project`] to the page's rows: the
//! workspaces in declaration order with their derived worktree path
//! (`workspace::workspace_worktree`), machine, effective tags (their own
//! plus the machine's, [`Project::effective_tags`]) and effective slot
//! ([`Project::workspace_slot`]: explicit, else the zero-based place among
//! the machine's workspaces); then the machines, each with its SSH host, its
//! root project directory and how many workspaces it hosts.
//!
//! The read ([`read`]) and the probes run off the UI thread. A read that
//! lands for another project than the page's is dropped
//! ([`WorkspacesState::loaded`]). Reachability is keyed by machine *and*
//! host ([`probe_key`]), so a result probed for a host the machine no longer
//! names never shows against it. Reachability says only that SSH answered:
//! it is not the Shelbi CLI, Git or the project being ready there, and the
//! labels say so.
//!
//! The page's buttons don't edit the config themselves: each asks for a
//! form through an [`Intent`], which the window routes back to whoever
//! handles it (Add machine and Configure open the machine form,
//! `super::machine_form`; the workspace forms are their own task).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use shelbi_core::{Machine, MachineKind, Project, Reachability};

/// A form the page asks for: what its buttons, rows and menus do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// New workspace.
    NewWorkspace,
    /// A workspace's edit button, its row, or Edit in its ⋮ menu.
    EditWorkspace(String),
    /// Remove in a workspace's ⋮ menu.
    RemoveWorkspace(String),
    /// Add machine.
    AddMachine,
    /// A machine's Configure.
    ConfigureMachine(String),
}

/// One effective tag on a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// Declared on the workspace's machine, not the workspace itself.
    pub inherited: bool,
}

/// A row of the workspace table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub name: String,
    /// The worktree, `<machine work_dir>/.shelbi/wt/<name>`, with a local
    /// home folder shortened to `~`.
    pub path: String,
    pub machine: String,
    /// The machine's kind; `None` for a machine the project doesn't declare.
    pub machine_kind: Option<MachineKind>,
    /// The effective tags, sorted.
    pub tags: Vec<Tag>,
    /// The effective slot.
    pub slot: u32,
    /// Whether the slot is set in the config (else it's automatic).
    pub slot_explicit: bool,
}

/// A row of the machine list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub name: String,
    pub kind: MachineKind,
    /// The host SSH connects to (its `host:`, else its name); `None` for a
    /// local machine.
    pub host: Option<String>,
    /// The root project directory, `~`-shortened when local.
    pub work_dir: String,
    /// How many of the project's workspaces it hosts.
    pub workspaces: usize,
    /// The machine as declared, every field (its Configure form edits it).
    pub config: Machine,
}

/// The page's rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PoolIndex {
    pub workspaces: Vec<WorkspaceRow>,
    pub machines: Vec<MachineRow>,
    /// The file the pool is declared in (`projects/<p>/local.yaml` for an
    /// in-repo project, else `projects/<p>.yaml`).
    pub file: Option<PathBuf>,
}

impl PoolIndex {
    /// The rows for `project`, its local paths shortened against `home`.
    pub fn build(project: &Project, home: Option<&Path>) -> PoolIndex {
        let local = |kind: MachineKind, path: &Path| match kind {
            MachineKind::Local => tilde(path, home),
            MachineKind::Ssh => path.display().to_string(),
        };
        let workspaces = project
            .workspaces
            .iter()
            .map(|w| {
                let machine = project.machine(&w.machine);
                let own: BTreeSet<&str> = w.tags.iter().map(String::as_str).collect();
                let tags = project
                    .effective_tags(w)
                    .into_iter()
                    .map(|name| Tag {
                        inherited: !own.contains(name.as_str()),
                        name,
                    })
                    .collect();
                let path = match machine {
                    Some(m) => local(m.kind, &shelbi_orchestrator::workspace::workspace_worktree(m, w)),
                    None => String::new(),
                };
                WorkspaceRow {
                    name: w.name.clone(),
                    path,
                    machine: w.machine.clone(),
                    machine_kind: machine.map(|m| m.kind),
                    tags,
                    slot: project.workspace_slot(w),
                    slot_explicit: w.slot.is_some(),
                }
            })
            .collect();
        let machines = project
            .machines
            .iter()
            .map(|m| MachineRow {
                name: m.name.clone(),
                kind: m.kind,
                host: match m.kind {
                    MachineKind::Local => None,
                    MachineKind::Ssh => Some(m.host.clone().unwrap_or_else(|| m.name.clone())),
                },
                work_dir: local(m.kind, &m.work_dir),
                workspaces: project.workspaces.iter().filter(|w| w.machine == m.name).count(),
                config: m.clone(),
            })
            .collect();
        PoolIndex {
            workspaces,
            machines,
            file: None,
        }
    }

    /// The `(probe key, ssh host)` pairs to probe: every SSH machine.
    pub fn probe_targets(&self) -> Vec<(String, String)> {
        self.machines
            .iter()
            .filter_map(|m| m.host.as_ref().map(|h| (probe_key(&m.name, h), h.clone())))
            .collect()
    }
}

/// Read `project`'s pool from disk. Blocks (it loads the project config):
/// call it off the UI thread.
pub fn read(project: &str) -> Result<PoolIndex, String> {
    let loaded = shelbi_state::load_project(project).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut index = PoolIndex::build(&loaded, home.as_deref());
    index.file = pool_file(project);
    Ok(index)
}

/// The file `project`'s machines and workspaces are declared in, as
/// `shelbi_state::load_project` picks it: the in-repo split's `local.yaml`
/// when there is one, else the global YAML.
pub fn pool_file(project: &str) -> Option<PathBuf> {
    let projects = shelbi_state::projects_dir().ok()?;
    let split = projects.join(project).join("local.yaml");
    Some(if split.is_file() { split } else { projects.join(format!("{project}.yaml")) })
}

/// `path` with `home` shortened to `~`.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    shelbi_app::add_project_wizard::tilde(path, home)
}

/// A remote's probe key: its name and the host probed, so a machine whose
/// host changes is probed afresh rather than showing the old host's result.
pub fn probe_key(machine: &str, host: &str) -> String {
    format!("{machine}\n{host}")
}

/// A machine's status badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    /// This Mac: never probed.
    Local,
    /// A remote not probed yet, or its host just changed.
    Checking,
    /// SSH answered.
    Reachable,
    /// SSH didn't answer; why.
    Unreachable(String),
}

impl MachineStatus {
    pub fn label(&self) -> &'static str {
        match self {
            MachineStatus::Local => "Local",
            MachineStatus::Checking => "Checking…",
            MachineStatus::Reachable => "Reachable",
            MachineStatus::Unreachable(_) => "Unreachable",
        }
    }

    /// What the badge means, as its tooltip: reachability is only SSH.
    pub fn help(&self) -> String {
        match self {
            MachineStatus::Local => "Workspaces here run on this Mac.".into(),
            MachineStatus::Checking => "Checking whether SSH can reach it.".into(),
            MachineStatus::Reachable => {
                "SSH reached it. This doesn’t check the Shelbi CLI, Git, or the project there.".into()
            }
            MachineStatus::Unreachable(e) => format!("SSH couldn’t reach it: {e}"),
        }
    }
}

/// Where the page's read stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    /// Reading (the first read, or a Retry after a failure).
    Loading,
    /// The config couldn't be read, and nothing was read before.
    Failed(String),
    Loaded(PoolIndex),
}

/// The page's state.
#[derive(Debug)]
pub struct WorkspacesState {
    project: String,
    load: Load,
    /// A reread failed after a good read: the rows stay, the error shows.
    error: Option<String>,
    /// A note under the header (where an Edit went).
    notice: Option<String>,
    /// The prober's latest results, by [`probe_key`].
    reachability: HashMap<String, Reachability>,
}

impl WorkspacesState {
    pub fn new(project: &str) -> Self {
        WorkspacesState {
            project: project.to_string(),
            load: Load::Loading,
            error: None,
            notice: None,
            reachability: HashMap::new(),
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn load(&self) -> &Load {
        &self.load
    }

    /// The rows, once a read has landed.
    pub fn index(&self) -> Option<&PoolIndex> {
        match &self.load {
            Load::Loaded(index) => Some(index),
            _ => None,
        }
    }

    /// A reread's error, shown over rows still from the last good read.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// Show `error` over the rows (an Edit that couldn't open its file).
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Retry after a failure: back to reading.
    pub fn retry(&mut self) {
        if matches!(self.load, Load::Failed(_)) {
            self.load = Load::Loading;
        }
        self.error = None;
    }

    /// A read for `project` landed. Returns whether the rows changed from an
    /// earlier read's (an edit took: the window refreshes what it shows from
    /// the config). A read for another project than the page's is dropped.
    pub fn loaded(&mut self, project: &str, read: Result<PoolIndex, String>) -> bool {
        if project != self.project {
            return false;
        }
        match (read, &self.load) {
            (Ok(index), Load::Loaded(before)) => {
                self.error = None;
                let changed = *before != index;
                self.load = Load::Loaded(index);
                changed
            }
            (Ok(index), _) => {
                self.error = None;
                self.load = Load::Loaded(index);
                false
            }
            (Err(e), Load::Loaded(_)) => {
                self.error = Some(format!("Couldn’t read the workspaces again: {e}"));
                false
            }
            (Err(e), _) => {
                self.load = Load::Failed(e);
                false
            }
        }
    }

    /// What the prober should probe.
    pub fn probe_targets(&self) -> Vec<(String, String)> {
        self.index().map(PoolIndex::probe_targets).unwrap_or_default()
    }

    /// Fold the prober's latest results in. Returns whether anything changed.
    pub fn set_reachability(&mut self, results: HashMap<String, Reachability>) -> bool {
        if results == self.reachability {
            return false;
        }
        self.reachability = results;
        true
    }

    /// Machine `m`'s badge.
    pub fn status(&self, m: &MachineRow) -> MachineStatus {
        let Some(host) = &m.host else {
            return MachineStatus::Local;
        };
        match self.reachability.get(&probe_key(&m.name, host)) {
            Some(Reachability::Reachable) => MachineStatus::Reachable,
            Some(Reachability::Unreachable { error }) => MachineStatus::Unreachable(error.clone()),
            Some(Reachability::Unknown) | None => MachineStatus::Checking,
        }
    }
}

/// `n workspaces`, `1 workspace`.
pub fn workspace_count(n: usize) -> String {
    match n {
        1 => "1 workspace".into(),
        n => format!("{n} workspaces"),
    }
}

/// A machine's kind, as the table's Machine column says it.
pub fn kind_label(kind: Option<MachineKind>) -> &'static str {
    match kind {
        Some(MachineKind::Local) => "Local",
        Some(MachineKind::Ssh) => "SSH",
        None => "Not declared",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A project from `yaml`'s machines and workspaces.
    pub(crate) fn project(pool: &str) -> Project {
        let yaml = format!(
            "name: My project\nrepo: ''\ndefault_branch: main\norchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n{pool}"
        );
        let mut p = Project::from_yaml_str(&yaml).unwrap();
        p.name = "my-project".into();
        p
    }

    /// Replace project `name`'s YAML under `root` (a test's
    /// `SHELBI_HOME`) with one declaring `pool`.
    pub(crate) fn write_pool(root: &Path, name: &str, pool: &str) -> PathBuf {
        let file = root.join("projects").join(format!("{name}.yaml"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            format!(
                "name: My project\nrepo: ''\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n{pool}"
            ),
        )
        .unwrap();
        file
    }

    pub(crate) const POOL: &str = "\
machines:
  - name: This Mac
    kind: local
    work_dir: /Users/me/Projects/my-project
  - name: Build server
    kind: ssh
    host: build-server
    work_dir: /home/me/my-project
    tags: [linux]
workspaces:
  - name: alpha
    machine: This Mac
    tags: [frontend]
  - name: bravo
    machine: This Mac
  - name: review
    machine: This Mac
    tags: [review]
    slot: 7
  - name: charlie
    machine: Build server
  - name: delta
    machine: Build server
    tags: [gpu, linux]
";

    fn index() -> PoolIndex {
        PoolIndex::build(&project(POOL), Some(Path::new("/Users/me")))
    }

    fn tags(row: &WorkspaceRow) -> Vec<(&str, bool)> {
        row.tags.iter().map(|t| (t.name.as_str(), t.inherited)).collect()
    }

    #[test]
    fn workspaces_list_in_declaration_order_with_derived_paths() {
        let index = index();
        let names: Vec<_> = index.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["alpha", "bravo", "review", "charlie", "delta"]);
        assert_eq!(index.workspaces[0].path, "~/Projects/my-project/.shelbi/wt/alpha");
        assert_eq!(
            index.workspaces[3].path, "/home/me/my-project/.shelbi/wt/charlie",
            "a remote path is the remote's own, never shortened against this Mac's home"
        );
        assert_eq!(index.workspaces[0].machine_kind, Some(MachineKind::Local));
        assert_eq!(kind_label(index.workspaces[3].machine_kind), "SSH");
    }

    #[test]
    fn tags_include_the_machines_and_mark_them_inherited() {
        let index = index();
        assert_eq!(tags(&index.workspaces[0]), [("frontend", false)]);
        assert!(index.workspaces[1].tags.is_empty(), "bravo shows No tags");
        assert_eq!(tags(&index.workspaces[3]), [("linux", true)], "charlie gets the machine's");
        assert_eq!(
            tags(&index.workspaces[4]),
            [("gpu", false), ("linux", false)],
            "a tag declared on both is the workspace's own"
        );
    }

    #[test]
    fn slots_are_explicit_or_the_place_on_their_machine() {
        let index = index();
        let slots: Vec<_> = index.workspaces.iter().map(|w| (w.slot, w.slot_explicit)).collect();
        assert_eq!(slots, [(0, false), (1, false), (7, true), (0, false), (1, false)]);
    }

    #[test]
    fn machines_carry_host_root_and_workspace_count() {
        let index = index();
        assert_eq!(
            index.machines,
            [
                MachineRow {
                    name: "This Mac".into(),
                    kind: MachineKind::Local,
                    host: None,
                    work_dir: "~/Projects/my-project".into(),
                    workspaces: 3,
                    config: project(POOL).machines[0].clone(),
                },
                MachineRow {
                    name: "Build server".into(),
                    kind: MachineKind::Ssh,
                    host: Some("build-server".into()),
                    work_dir: "/home/me/my-project".into(),
                    workspaces: 2,
                    config: project(POOL).machines[1].clone(),
                },
            ]
        );
        assert_eq!(workspace_count(3), "3 workspaces");
        assert_eq!(workspace_count(1), "1 workspace");
        assert_eq!(workspace_count(0), "0 workspaces");
    }

    #[test]
    fn an_ssh_machine_without_a_host_connects_by_its_name() {
        let p = project(
            "machines:\n  - name: gpu.lan\n    kind: ssh\n    work_dir: /srv\nworkspaces: []\n",
        );
        let index = PoolIndex::build(&p, None);
        assert_eq!(index.machines[0].host.as_deref(), Some("gpu.lan"));
        assert_eq!(index.probe_targets(), [(probe_key("gpu.lan", "gpu.lan"), "gpu.lan".to_string())]);
        assert_eq!(index.machines[0].workspaces, 0);
    }

    #[test]
    fn a_local_only_project_has_nothing_to_probe() {
        let p = project(
            "machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp/p\nworkspaces:\n  - name: alpha\n    machine: hub\n",
        );
        let mut state = WorkspacesState::new("my-project");
        state.loaded("my-project", Ok(PoolIndex::build(&p, None)));
        assert!(state.probe_targets().is_empty());
        assert_eq!(state.status(&state.index().unwrap().machines[0]), MachineStatus::Local);
    }

    #[test]
    fn remote_status_follows_the_probe_for_its_current_host() {
        let mut state = WorkspacesState::new("my-project");
        state.loaded("my-project", Ok(index()));
        let remote = state.index().unwrap().machines[1].clone();
        assert_eq!(state.status(&remote), MachineStatus::Checking, "until a probe lands");
        let key = probe_key("Build server", "build-server");
        assert!(state.set_reachability(HashMap::from([(key.clone(), Reachability::Reachable)])));
        assert_eq!(state.status(&remote), MachineStatus::Reachable);
        assert!(!state.set_reachability(HashMap::from([(key.clone(), Reachability::Reachable)])));
        state.set_reachability(HashMap::from([(
            key,
            Reachability::Unreachable {
                error: "Connection timed out".into(),
            },
        )]));
        let down = state.status(&remote);
        assert_eq!(down.label(), "Unreachable");
        assert!(down.help().contains("Connection timed out"));
        assert_eq!(
            state.index().unwrap().machines[1].work_dir,
            "/home/me/my-project",
            "a failed probe keeps the machine's configuration"
        );

        // Its host changes: the old host's result doesn't count for it.
        let moved = PoolIndex::build(
            &project(&POOL.replace("host: build-server", "host: build-2")),
            Some(Path::new("/Users/me")),
        );
        assert!(state.loaded("my-project", Ok(moved)));
        let remote = state.index().unwrap().machines[1].clone();
        assert_eq!(state.status(&remote), MachineStatus::Checking);
        assert_eq!(state.probe_targets(), [(probe_key("Build server", "build-2"), "build-2".to_string())]);
    }

    #[test]
    fn reachable_never_claims_more_than_ssh() {
        let help = MachineStatus::Reachable.help();
        assert!(help.contains("SSH") && help.contains("doesn’t check"));
        assert_ne!(MachineStatus::Reachable.label(), "Connected");
    }

    #[test]
    fn a_failed_first_read_retries_and_a_failed_reread_keeps_the_rows() {
        let mut state = WorkspacesState::new("my-project");
        assert_eq!(state.load(), &Load::Loading);
        state.loaded("my-project", Err("bad yaml".into()));
        assert_eq!(state.load(), &Load::Failed("bad yaml".into()));
        state.retry();
        assert_eq!(state.load(), &Load::Loading);
        assert!(!state.loaded("my-project", Ok(index())), "a first read isn't a change");
        assert!(!state.loaded("my-project", Err("gone".into())));
        assert!(state.index().is_some(), "the rows stay");
        assert!(state.error().unwrap().contains("gone"));
        assert!(!state.loaded("my-project", Ok(index())), "the same rows");
        assert_eq!(state.error(), None);
    }

    #[test]
    fn a_read_for_another_project_is_dropped() {
        let mut state = WorkspacesState::new("beta");
        assert!(!state.loaded("alpha", Ok(index())));
        assert_eq!(state.load(), &Load::Loading);
        assert!(!state.loaded("alpha", Err("x".into())));
        assert_eq!(state.load(), &Load::Loading);
    }

    #[test]
    fn an_empty_pool_loads_with_no_rows() {
        let p = project("machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp/p\nworkspaces: []\n");
        let mut state = WorkspacesState::new("my-project");
        state.loaded("my-project", Ok(PoolIndex::build(&p, None)));
        let index = state.index().unwrap();
        assert!(index.workspaces.is_empty());
        assert_eq!(index.machines.len(), 1);
    }

    #[test]
    fn an_edit_that_changes_the_pool_reports_it() {
        let mut state = WorkspacesState::new("my-project");
        state.loaded("my-project", Ok(index()));
        let retagged = PoolIndex::build(
            &project(&POOL.replace("tags: [frontend]", "tags: [frontend, web]")),
            Some(Path::new("/Users/me")),
        );
        assert!(state.loaded("my-project", Ok(retagged)));
    }
}
