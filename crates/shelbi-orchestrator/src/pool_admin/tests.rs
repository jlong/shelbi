use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const PROJECT: &str = "pa";

const GLOBAL: &str = "\
# Pool admin fixture.
repo: /tmp/pa
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude: { command: claude, flags: [] }
machines:
  - name: hub
    kind: local
    work_dir: /tmp/pa-hub
  # Remote box.
  - name: devbox
    kind: ssh
    host: devbox.lan
    work_dir: /home/u/pa
    forward: tcp
workspaces:
  - name: alpha
    machine: hub
  - name: bravo
    machine: hub
    slot: 0
    hand_key: keep
";

/// A temp `SHELBI_HOME` holding project `pa`; restores the env on drop.
struct Home {
    root: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    fn new() -> Home {
        let lock = crate::test_lock::acquire();
        let root = std::env::temp_dir().join(format!(
            "shelbi-pool-admin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &root);
        fs::create_dir_all(shelbi_state::projects_dir().unwrap()).unwrap();
        Home { root, _lock: lock }
    }

    fn global(text: &str) -> Home {
        let home = Home::new();
        fs::write(home.global_path(), text).unwrap();
        home
    }

    fn in_repo(shared: &str, local: &str) -> Home {
        let home = Home::new();
        let repo = home.root.join("repo");
        fs::create_dir_all(repo.join(".shelbi")).unwrap();
        fs::write(repo.join(".shelbi/project.yaml"), shared).unwrap();
        let dir = shelbi_state::projects_dir().unwrap().join(PROJECT);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("local.yaml"), format!("repo: {}\n{local}", repo.display())).unwrap();
        home
    }

    fn global_path(&self) -> PathBuf {
        shelbi_state::projects_dir().unwrap().join(format!("{PROJECT}.yaml"))
    }

    fn text(&self) -> String {
        fs::read_to_string(self.global_path()).unwrap()
    }

    fn pool(&self) -> PoolConfig {
        PoolConfig::load(PROJECT).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A scripted [`PoolOps`]: worktrees are a map keyed by (machine, workspace)
/// that materialize/move/remove update, every call is recorded, and any step
/// can be made to fail.
#[derive(Default)]
struct Script {
    calls: RefCell<Vec<String>>,
    version: Option<String>,
    active: HashMap<String, String>,
    assigned: HashMap<String, Vec<String>>,
    sessions: HashMap<String, SessionState>,
    /// Keyed by where the worktree is: (host, path).
    worktrees: RefCell<HashMap<(String, PathBuf), WorktreeState>>,
    fail: HashMap<&'static str, String>,
    hooks: Option<HookWiring>,
    /// Runs once, during the first `move_worktree` / `materialize` (a
    /// concurrent edit).
    during: RefCell<Option<Box<dyn FnMut()>>>,
}

/// Where a workspace's worktree is: the machine's host and the path.
fn location(machine: &Machine, w: &str) -> (String, PathBuf) {
    let spec = WorkspaceSpec {
        name: w.to_string(),
        machine: machine.name.clone(),
        tags: Vec::new(),
        slot: None,
    };
    (format!("{:?}", machine.host()), ws::workspace_worktree(machine, &spec))
}

impl Script {
    /// A worktree for `ws` on the fixture's machine `machine`.
    fn with_worktree(self, machine: &str, ws: &str, state: WorktreeState) -> Self {
        let m = fixture_project().machine(machine).unwrap().clone();
        self.with_worktree_on(&m, ws, state)
    }

    fn with_worktree_on(self, machine: &Machine, ws: &str, state: WorktreeState) -> Self {
        self.worktrees.borrow_mut().insert(location(machine, ws), state);
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn state(&self, machine: &str, ws: &str) -> WorktreeState {
        let m = fixture_project().machine(machine).unwrap().clone();
        self.state_on(&m, ws)
    }

    fn state_on(&self, machine: &Machine, ws: &str) -> WorktreeState {
        self.worktrees
            .borrow()
            .get(&location(machine, ws))
            .cloned()
            .unwrap_or(WorktreeState::Missing)
    }

    fn record(&self, call: String) {
        self.calls.borrow_mut().push(call);
    }

    fn failing(&self, op: &str) -> Result<(), String> {
        match self.fail.get(op) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn run_during(&self) {
        let f = self.during.borrow_mut().take();
        if let Some(mut f) = f {
            f();
        }
    }
}

impl PoolOps for Script {
    fn check_version(&self) -> Result<(), String> {
        match &self.version {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn active_task(&self, _: &Project, workspace: &str) -> Result<Option<String>, String> {
        self.failing("active_task")?;
        Ok(self.active.get(workspace).cloned())
    }

    fn open_assignments(&self, _: &Project, workspace: &str) -> Result<Vec<String>, String> {
        Ok(self.assigned.get(workspace).cloned().unwrap_or_default())
    }

    fn session(&self, _: &Project, w: &WorkspaceSpec) -> SessionState {
        self.sessions.get(&w.name).cloned().unwrap_or(SessionState::Dead)
    }

    fn worktree(&self, machine: &Machine, w: &WorkspaceSpec) -> WorktreeState {
        self.state_on(machine, &w.name)
    }

    fn materialize(&self, _: &Project, machine: &Machine, w: &WorkspaceSpec) -> Result<(), String> {
        self.record(format!("materialize {}/{}", machine.name, w.name));
        self.run_during();
        self.failing("materialize")?;
        self.worktrees
            .borrow_mut()
            .entry(location(machine, &w.name))
            .or_insert(WorktreeState::Clean);
        Ok(())
    }

    fn wire_hooks(&self, _: &Project, machine: &Machine, w: &WorkspaceSpec) -> Result<HookWiring, String> {
        self.record(format!("wire {}/{}", machine.name, w.name));
        self.failing("wire")?;
        Ok(self.hooks.clone().unwrap_or(HookWiring::Wired))
    }

    fn move_worktree(&self, _: &Project, machine: &Machine, from: &WorkspaceSpec, to: &WorkspaceSpec) -> Result<(), String> {
        self.record(format!("move {}/{} -> {}", machine.name, from.name, to.name));
        self.run_during();
        self.failing("move")?;
        let mut map = self.worktrees.borrow_mut();
        let state = map
            .remove(&location(machine, &from.name))
            .unwrap_or(WorktreeState::Missing);
        map.insert(location(machine, &to.name), state);
        Ok(())
    }

    fn remove_worktree(&self, _: &Project, machine: &Machine, w: &WorkspaceSpec) -> Result<(), String> {
        self.record(format!("remove {}/{}", machine.name, w.name));
        self.failing("remove")?;
        self.worktrees.borrow_mut().remove(&location(machine, &w.name));
        Ok(())
    }

    fn kill_session(&self, _: &Project, w: &WorkspaceSpec) -> Result<(), String> {
        self.record(format!("kill {}", w.name));
        self.failing("kill")
    }
}

fn add_req(name: &str, machine: Option<&str>, tags: &[&str], slot: Option<u32>) -> AddWorkspace {
    AddWorkspace {
        name: name.into(),
        machine: machine.map(str::to_string),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        slot,
    }
}

fn update_req(name: &str, new_name: &str, machine: &str) -> UpdateWorkspace {
    UpdateWorkspace {
        name: name.into(),
        expected: None,
        new_name: new_name.into(),
        machine: machine.into(),
        tags: Vec::new(),
        slot: None,
    }
}

fn remove_req(name: &str, force: bool) -> RemoveWorkspace {
    RemoveWorkspace {
        name: name.into(),
        expected: None,
        force,
    }
}

fn machine_fields(name: &str, kind: MachineKind, host: Option<&str>, dir: &str) -> MachineFields {
    MachineFields {
        name: name.into(),
        kind,
        host: host.map(str::to_string),
        work_dir: dir.into(),
        tags: Vec::new(),
    }
}

fn fixture_project() -> Project {
    let mut p = Project::from_yaml_str(GLOBAL).unwrap();
    p.name = PROJECT.into();
    p
}

// ---------------------------------------------------------------------------
// Planning and field helpers

#[test]
fn plan_workspace_add_defaults_to_the_local_machine() {
    let spec = plan_workspace_add(&fixture_project(), &add_req("charlie", None, &[], None)).unwrap();
    assert_eq!(spec.name, "charlie");
    assert_eq!(spec.machine, "hub");
    assert!(spec.tags.is_empty());
    assert_eq!(spec.slot, None);
    let spec = plan_workspace_add(&fixture_project(), &add_req("charlie", Some("devbox"), &[], None)).unwrap();
    assert_eq!(spec.machine, "devbox");
}

#[test]
fn plan_workspace_add_rejects_collisions_bad_names_and_unknown_machines() {
    let p = fixture_project();
    let err = plan_workspace_add(&p, &add_req("alpha", None, &[], None)).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    // `alpha-` is a valid id but normalizes onto `alpha`.
    let err = plan_workspace_add(&p, &add_req("alpha-", None, &[], None)).unwrap_err();
    assert!(err.to_string().contains("slug-collides"), "{err}");
    let err = plan_workspace_add(&p, &add_req("Has Spaces", None, &[], None)).unwrap_err();
    assert!(err.to_string().contains("invalid workspace name"), "{err}");
    assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Name), .. }));
    let err = plan_workspace_add(&p, &add_req("charlie", Some("ghost"), &[], None)).unwrap_err();
    assert!(err.to_string().contains("unknown machine"), "{err}");
    assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Machine), .. }));
}

#[test]
fn plan_workspace_add_without_a_local_machine_needs_one_named() {
    let mut p = fixture_project();
    p.machines.retain(|m| m.name != "hub");
    p.workspaces.clear();
    let err = plan_workspace_add(&p, &add_req("charlie", None, &[], None)).unwrap_err();
    assert!(err.to_string().contains("no local machine"), "{err}");
}

#[test]
fn plan_workspace_update_checks_the_new_name_machine_and_tags() {
    let p = fixture_project();
    let alpha = p.workspace("alpha").unwrap().clone();
    let mut req = update_req("alpha", "alpha", "devbox");
    req.tags = vec![" gpu ".into(), "gpu".into()];
    req.slot = Some(0);
    let spec = plan_workspace_update(&p, &alpha, &req).unwrap();
    assert_eq!(spec.machine, "devbox");
    assert_eq!(spec.tags, ["gpu"]);
    assert_eq!(spec.slot, Some(0));
    // Keeping its own name is no collision; taking bravo's is.
    let err = plan_workspace_update(&p, &alpha, &update_req("alpha", "bravo", "hub")).unwrap_err();
    assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Name), .. }), "{err}");
    let err = plan_workspace_update(&p, &alpha, &update_req("alpha", "alpha", "ghost")).unwrap_err();
    assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Machine), .. }), "{err}");
    let mut req = update_req("alpha", "alpha", "hub");
    req.tags = vec!["two words".into()];
    let err = plan_workspace_update(&p, &alpha, &req).unwrap_err();
    assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Tags), .. }), "{err}");
}

#[test]
fn tags_are_trimmed_deduplicated_and_keep_their_case() {
    let tags: Vec<String> = [" gpu ", "", "GPU", "gpu", "review", "  "]
        .iter()
        .map(|t| t.to_string())
        .collect();
    assert_eq!(normalize_tags(&tags).unwrap(), vec!["gpu", "GPU", "review"]);
    for bad in ["two words", "a,b"] {
        let err = normalize_tags(&[bad.to_string()]).unwrap_err();
        assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Tags), .. }), "{bad}");
    }
}

#[test]
fn slot_input_is_automatic_when_empty_and_any_u32_otherwise() {
    assert_eq!(parse_slot("").unwrap(), None);
    assert_eq!(parse_slot("   ").unwrap(), None);
    assert_eq!(parse_slot("0").unwrap(), Some(0));
    assert_eq!(parse_slot(" 7 ").unwrap(), Some(7));
    assert_eq!(parse_slot("4294967295").unwrap(), Some(u32::MAX));
    let err = parse_slot("4294967296").unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
    for bad in ["-1", "1.5", "abc", "+3"] {
        let err = parse_slot(bad).unwrap_err();
        assert!(matches!(err, PoolError::Invalid { field: Some(PoolField::Slot), .. }), "{bad}");
    }
}

#[test]
fn machine_fields_are_validated() {
    let p = fixture_project();
    let ok = plan_machine(&p, &machine_fields("Build server", MachineKind::Ssh, Some(" build-server "), "/srv/pa"), None).unwrap();
    assert_eq!(ok.name, "Build server", "names are free-form labels");
    assert_eq!(ok.host.as_deref(), Some("build-server"));
    // A local machine has no host, and its `~` is this machine's home.
    let local = plan_machine(&p, &machine_fields("laptop", MachineKind::Local, Some("x"), "~/Projects/pa"), None).unwrap();
    assert_eq!(local.host, None);
    assert_eq!(local.work_dir, shelbi_state::expand_tilde_str("~/Projects/pa"));
    assert!(local.work_dir.is_absolute());
    // An SSH machine named like a host can leave the host empty.
    let bare = plan_machine(&p, &machine_fields("gpu.lan", MachineKind::Ssh, Some(" "), "/srv/pa"), None).unwrap();
    assert_eq!(bare.host, None);
    let cases = [
        (machine_fields("  ", MachineKind::Local, None, "/srv"), PoolField::Name),
        (machine_fields("DevBox", MachineKind::Local, None, "/srv"), PoolField::Name),
        (machine_fields("devbox!", MachineKind::Local, None, "/srv"), PoolField::Name),
        (machine_fields("Build server", MachineKind::Ssh, None, "/srv"), PoolField::Host),
        (machine_fields("box", MachineKind::Ssh, Some("-oProxyCommand=x"), "/srv"), PoolField::Host),
        (machine_fields("box", MachineKind::Ssh, Some("a b"), "/srv"), PoolField::Host),
        (machine_fields("box", MachineKind::Ssh, None, ""), PoolField::WorkDir),
        (machine_fields("box", MachineKind::Ssh, None, "relative/dir"), PoolField::WorkDir),
        (machine_fields("box", MachineKind::Ssh, None, "~/pa"), PoolField::WorkDir),
    ];
    for (fields, field) in cases {
        let err = plan_machine(&p, &fields, None).unwrap_err();
        assert!(matches!(err, PoolError::Invalid { field: Some(f), .. } if f == field), "{fields:?}: {err}");
    }
}

// ---------------------------------------------------------------------------
// Adding a workspace

#[test]
fn add_materializes_wires_then_declares() {
    let home = Home::global(GLOBAL);
    let ops = Script::default();
    let added = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[" gpu", "gpu"], None)).unwrap();
    assert_eq!(ops.calls(), vec!["materialize hub/charlie", "wire hub/charlie"]);
    assert_eq!(added.workspace.tags, vec!["gpu"]);
    assert_eq!(added.hooks, HooksOutcome::Wired);
    assert_eq!(added.worktree, PathBuf::from("/tmp/pa-hub/.shelbi/wt/charlie"));
    // Third on `hub` with no explicit slot: the zero-based fallback index.
    assert_eq!(added.slot, 2);
    let pool = home.pool();
    assert_eq!(pool.workspace("charlie").unwrap().tags, vec!["gpu"]);
    let text = home.text();
    assert!(text.contains("# Remote box."), "{text}");
    assert!(text.contains("forward: tcp"), "{text}");
    assert!(text.contains("hand_key: keep"), "{text}");
}

#[test]
fn add_keeps_an_explicit_slot_zero() {
    let home = Home::global(GLOBAL);
    let added = add_workspace_with(&Script::default(), PROJECT, &add_req("charlie", Some("devbox"), &[], Some(0))).unwrap();
    assert_eq!(added.slot, 0);
    assert_eq!(home.pool().workspace("charlie").unwrap().slot, Some(0));
}

#[test]
fn add_needing_a_hook_merge_declares_nothing_and_a_retry_succeeds() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        hooks: Some(HookWiring::MergeRequired("merge these hooks".into())),
        ..Default::default()
    };
    let err = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[], None)).unwrap_err();
    assert_eq!(
        err,
        PoolError::HooksMergeRequired {
            workspace: "charlie".into(),
            message: "merge these hooks".into()
        }
    );
    assert_eq!(home.text(), GLOBAL);
    // The worktree stays for the retry.
    assert_eq!(ops.state("hub", "charlie"), WorktreeState::Clean);

    let retry = Script {
        hooks: Some(HookWiring::SelfHealed),
        ..Default::default()
    }
    .with_worktree("hub", "charlie", WorktreeState::Clean);
    let added = add_workspace_with(&retry, PROJECT, &add_req("charlie", None, &[], None)).unwrap();
    assert_eq!(added.hooks, HooksOutcome::SelfHealed);
    assert!(home.pool().workspace("charlie").is_some());
}

#[test]
fn add_skips_hook_wiring_for_a_non_claude_runner() {
    let home = Home::global(&GLOBAL.replace(
        "runner: claude\nagent_runners:\n  claude: { command: claude, flags: [] }",
        "runner: codex\nagent_runners:\n  codex: { command: codex, flags: [] }",
    ));
    let ops = Script::default();
    let added = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[], None)).unwrap();
    assert_eq!(added.hooks, HooksOutcome::Skipped);
    assert_eq!(ops.calls(), vec!["materialize hub/charlie"]);
    assert!(home.pool().workspace("charlie").is_some());
}

#[test]
fn a_failed_materialize_declares_nothing() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([("materialize", "git worktree add failed".to_string())]),
        ..Default::default()
    };
    let err = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[], None)).unwrap_err();
    assert!(matches!(&err, PoolError::Provisioning(m) if m.contains("git worktree add failed")), "{err}");
    assert_eq!(home.text(), GLOBAL);
}

#[test]
fn add_checks_the_daemon_version_first() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        version: Some("hub daemon is 0.1.0".into()),
        ..Default::default()
    };
    let err = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[], None)).unwrap_err();
    assert!(matches!(err, PoolError::Version(_)), "{err}");
    assert!(ops.calls().is_empty());
    assert_eq!(home.text(), GLOBAL);
}

#[test]
fn a_concurrent_pool_edit_during_provisioning_is_a_conflict() {
    let home = Home::global(GLOBAL);
    let path = home.global_path();
    let ops = Script::default();
    *ops.during.borrow_mut() = Some(Box::new(move || {
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace("  - name: alpha\n", "  - name: alpha\n    slot: 5\n")).unwrap();
    }));
    let err = add_workspace_with(&ops, PROJECT, &add_req("charlie", None, &[], None)).unwrap_err();
    assert!(matches!(&err, PoolError::Conflict(m) if m.contains("retrying the add")), "{err}");
    assert!(home.pool().workspace("charlie").is_none());
    assert_eq!(home.pool().workspace("alpha").unwrap().slot, Some(5));
}

#[test]
fn add_in_repo_writes_local_yaml_and_leaves_the_shared_half_alone() {
    let shared = "\
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude: { command: claude, flags: [] }
";
    let local = "\
machines:
  - { name: hub, kind: local, work_dir: /tmp/pa-hub }
workspaces:
  - { name: alpha, machine: hub }
";
    let home = Home::in_repo(shared, local);
    add_workspace_with(&Script::default(), PROJECT, &add_req("charlie", None, &["fast"], Some(4))).unwrap();
    assert_eq!(fs::read_to_string(home.root.join("repo/.shelbi/project.yaml")).unwrap(), shared);
    let local_now = fs::read_to_string(shelbi_state::projects_dir().unwrap().join(PROJECT).join("local.yaml")).unwrap();
    assert!(local_now.contains("charlie"), "{local_now}");
    assert!(!home.global_path().exists());
    assert_eq!(home.pool().workspace("charlie").unwrap().slot, Some(4));
}

// ---------------------------------------------------------------------------
// Removing a workspace

#[test]
fn remove_stops_the_session_removes_the_worktree_then_undeclares() {
    let home = Home::global(GLOBAL);
    let ops = Script::default().with_worktree("hub", "alpha", WorktreeState::Clean);
    let removed = remove_workspace_with(&ops, PROJECT, &remove_req("alpha", false)).unwrap();
    assert_eq!(removed.machine, "hub");
    assert_eq!(ops.calls(), vec!["kill alpha", "remove hub/alpha"]);
    assert!(home.pool().workspace("alpha").is_none());
    assert!(home.text().contains("hand_key: keep"));
}

#[test]
fn remove_refuses_an_active_task_unless_forced() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        active: HashMap::from([("alpha".to_string(), "feat-x".to_string())]),
        ..Default::default()
    };
    let err = remove_workspace_with(&ops, PROJECT, &remove_req("alpha", false)).unwrap_err();
    assert_eq!(
        err,
        PoolError::ActiveTask {
            workspace: "alpha".into(),
            task: "feat-x".into()
        }
    );
    assert!(ops.calls().is_empty());
    assert_eq!(home.text(), GLOBAL);

    remove_workspace_with(&ops, PROJECT, &remove_req("alpha", true)).unwrap();
    assert!(home.pool().workspace("alpha").is_none());
}

#[test]
fn remove_refuses_work_at_risk_or_an_unknown_task_state_unless_forced() {
    let home = Home::global(GLOBAL);
    let dirty = Script::default().with_worktree("hub", "alpha", WorktreeState::AtRisk("has uncommitted changes".into()));
    let err = remove_workspace_with(&dirty, PROJECT, &remove_req("alpha", false)).unwrap_err();
    assert!(matches!(&err, PoolError::Unsafe(m) if m.contains("uncommitted")), "{err}");
    assert!(dirty.calls().is_empty());

    let unknown = Script {
        fail: HashMap::from([("active_task", "board unreadable".to_string())]),
        ..Default::default()
    };
    let err = remove_workspace_with(&unknown, PROJECT, &remove_req("alpha", false)).unwrap_err();
    assert!(matches!(&err, PoolError::Unsafe(m) if m.contains("board unreadable")), "{err}");
    assert_eq!(home.text(), GLOBAL);

    remove_workspace_with(&dirty, PROJECT, &remove_req("alpha", true)).unwrap();
    assert!(home.pool().workspace("alpha").is_none());
}

#[test]
fn a_worktree_that_cant_be_removed_keeps_the_workspace_declared() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([
            ("remove", "/tmp/pa-hub/.shelbi/wt/alpha is still there".to_string()),
            ("kill", "no such session".to_string()),
        ]),
        ..Default::default()
    };
    let err = remove_workspace_with(&ops, PROJECT, &remove_req("alpha", false)).unwrap_err();
    assert!(matches!(&err, PoolError::Provisioning(m) if m.contains("still declared")), "{err}");
    assert_eq!(home.text(), GLOBAL);
}

#[test]
fn a_forced_removal_drops_a_workspace_whose_worktree_cant_be_cleaned_up() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([("remove", "ssh: connect to host devbox.lan: timed out".to_string())]),
        ..Default::default()
    };
    let removed = remove_workspace_with(&ops, PROJECT, &remove_req("alpha", true)).unwrap();
    assert_eq!(removed.warnings.len(), 1, "{:?}", removed.warnings);
    assert!(removed.warnings[0].contains("remove it by hand"));
    assert!(home.pool().workspace("alpha").is_none());
}

#[test]
fn a_failed_session_kill_is_a_warning() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([("kill", "no such session".to_string())]),
        ..Default::default()
    };
    let removed = remove_workspace_with(&ops, PROJECT, &remove_req("alpha", false)).unwrap();
    assert_eq!(removed.warnings, vec!["killing pane for `alpha`: no such session"]);
    assert!(home.pool().workspace("alpha").is_none());
}

#[test]
fn remove_checks_the_expected_entry_and_names_unknown_workspaces() {
    let home = Home::global(GLOBAL);
    let stale = RemoveWorkspace {
        name: "alpha".into(),
        expected: Some(WorkspaceSpec {
            name: "alpha".into(),
            machine: "devbox".into(),
            tags: Vec::new(),
            slot: None,
        }),
        force: false,
    };
    let err = remove_workspace_with(&Script::default(), PROJECT, &stale).unwrap_err();
    assert!(matches!(err, PoolError::Conflict(_)), "{err}");
    let err = remove_workspace_with(&Script::default(), PROJECT, &remove_req("zulu", false)).unwrap_err();
    assert!(matches!(&err, PoolError::NotFound(m) if m.contains("known: alpha, bravo")), "{err}");
    assert_eq!(home.text(), GLOBAL);
}

// ---------------------------------------------------------------------------
// Editing a workspace

#[test]
fn editing_tags_and_slot_is_a_config_edit() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        active: HashMap::from([("bravo".to_string(), "feat-x".to_string())]),
        ..Default::default()
    };
    let mut req = update_req("bravo", "bravo", "hub");
    req.tags = vec!["gpu".into()];
    req.slot = None;
    req.expected = Some(home.pool().workspace("bravo").unwrap().clone());
    let changed = update_workspace_with(&ops, PROJECT, &req).unwrap();
    assert!(ops.calls().is_empty(), "{:?}", ops.calls());
    assert_eq!(changed.workspace.slot, None);
    // Second on `hub`.
    assert_eq!(changed.slot, 1);
    let text = home.text();
    assert!(text.contains("hand_key: keep"), "{text}");
    assert!(!text.contains("slot: 0"), "{text}");
}

#[test]
fn an_edit_of_a_changed_workspace_is_a_conflict() {
    let home = Home::global(GLOBAL);
    let mut req = update_req("bravo", "bravo", "hub");
    let mut expected = home.pool().workspace("bravo").unwrap().clone();
    expected.slot = Some(9);
    req.expected = Some(expected);
    let err = update_workspace_with(&Script::default(), PROJECT, &req).unwrap_err();
    assert!(matches!(err, PoolError::Conflict(_)), "{err}");
    assert_eq!(home.text(), GLOBAL);
}

#[test]
fn renaming_an_idle_workspace_moves_its_worktree() {
    let home = Home::global(GLOBAL);
    let ops = Script::default().with_worktree("hub", "bravo", WorktreeState::Clean);
    let mut req = update_req("bravo", "delta", "hub");
    req.slot = Some(0);
    let changed = update_workspace_with(&ops, PROJECT, &req).unwrap();
    assert_eq!(ops.calls(), vec!["move hub/bravo -> delta"]);
    assert_eq!(changed.previous_name.as_deref(), Some("bravo"));
    assert_eq!(changed.previous_machine, None);
    assert_eq!(changed.worktree, PathBuf::from("/tmp/pa-hub/.shelbi/wt/delta"));
    let pool = home.pool();
    assert!(pool.workspace("bravo").is_none());
    assert_eq!(pool.workspace("delta").unwrap().slot, Some(0));
    assert!(home.text().contains("hand_key: keep"), "unknown keys follow the rename");
}

#[test]
fn a_rename_carries_the_migration_state() {
    let home = Home::global(GLOBAL);
    shelbi_state::set_workspace_migration_state(PROJECT, "bravo", shelbi_state::MigrationState::Pending).unwrap();
    update_workspace_with(&Script::default(), PROJECT, &update_req("bravo", "delta", "hub")).unwrap();
    assert_eq!(
        shelbi_state::workspace_migration_state(PROJECT, "delta").unwrap(),
        Some(shelbi_state::MigrationState::Pending)
    );
    assert!(home.pool().workspace("delta").is_some());
}

#[test]
fn renaming_refuses_a_workspace_in_use() {
    let home = Home::global(GLOBAL);
    let cases: Vec<(Script, &str)> = vec![
        (
            Script {
                active: HashMap::from([("bravo".to_string(), "feat-x".to_string())]),
                ..Default::default()
            },
            "active task",
        ),
        (
            Script {
                assigned: HashMap::from([("bravo".to_string(), vec!["queued-1".to_string()])]),
                ..Default::default()
            },
            "queued-1",
        ),
        (
            Script {
                sessions: HashMap::from([("bravo".to_string(), SessionState::Alive { user_shell: true })]),
                ..Default::default()
            },
            "live shell",
        ),
        (
            Script::default().with_worktree("hub", "bravo", WorktreeState::AtRisk("has 2 unpushed commits".into())),
            "unpushed",
        ),
        (
            Script::default().with_worktree("hub", "bravo", WorktreeState::Unknown("ssh timed out".into())),
            "ssh timed out",
        ),
        (
            Script::default()
                .with_worktree("hub", "bravo", WorktreeState::Clean)
                .with_worktree("hub", "delta", WorktreeState::Clean),
            "already at",
        ),
    ];
    for (ops, expect) in cases {
        let err = update_workspace_with(&ops, PROJECT, &update_req("bravo", "delta", "hub")).unwrap_err();
        assert!(err.to_string().contains(expect), "expected `{expect}`: {err}");
        assert!(ops.calls().is_empty(), "{expect}: {:?}", ops.calls());
        assert_eq!(home.text(), GLOBAL);
    }
}

#[test]
fn a_rename_whose_save_fails_moves_the_worktree_back() {
    let home = Home::global(GLOBAL);
    let path = home.global_path();
    let ops = Script::default().with_worktree("hub", "bravo", WorktreeState::Clean);
    *ops.during.borrow_mut() = Some(Box::new(move || {
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace("  - name: alpha\n", "  - name: alpha\n    slot: 5\n")).unwrap();
    }));
    let err = update_workspace_with(&ops, PROJECT, &update_req("bravo", "delta", "hub")).unwrap_err();
    assert!(matches!(&err, PoolError::Conflict(m) if m.contains("moved back")), "{err}");
    assert_eq!(ops.calls(), vec!["move hub/bravo -> delta", "move hub/delta -> bravo"]);
    assert_eq!(ops.state("hub", "bravo"), WorktreeState::Clean);
    assert!(home.pool().workspace("bravo").is_some());
}

#[test]
fn an_interrupted_rename_is_finished_by_retrying() {
    let home = Home::global(GLOBAL);
    // The worktree already moved; the config write never happened.
    let ops = Script::default().with_worktree("hub", "delta", WorktreeState::Clean);
    update_workspace_with(&ops, PROJECT, &update_req("bravo", "delta", "hub")).unwrap();
    assert!(ops.calls().is_empty(), "the moved worktree is adopted: {:?}", ops.calls());
    assert!(home.pool().workspace("delta").is_some());
}

#[test]
fn moving_to_another_machine_provisions_first_and_retires_the_old_worktree_last() {
    let home = Home::global(GLOBAL);
    let ops = Script::default().with_worktree("hub", "bravo", WorktreeState::Clean);
    let changed = update_workspace_with(&ops, PROJECT, &update_req("bravo", "bravo", "devbox")).unwrap();
    assert_eq!(
        ops.calls(),
        vec!["materialize devbox/bravo", "wire devbox/bravo", "remove hub/bravo"]
    );
    assert_eq!(changed.previous_machine.as_deref(), Some("hub"));
    assert_eq!(changed.hooks, HooksOutcome::Wired);
    assert!(changed.warnings.is_empty());
    assert_eq!(changed.worktree, PathBuf::from("/home/u/pa/.shelbi/wt/bravo"));
    assert_eq!(home.pool().workspace("bravo").unwrap().machine, "devbox");
}

#[test]
fn a_failed_move_after_partial_provisioning_undoes_it_and_can_be_retried() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        hooks: Some(HookWiring::MergeRequired("merge".into())),
        ..Default::default()
    }
    .with_worktree("hub", "bravo", WorktreeState::Clean);
    let err = update_workspace_with(&ops, PROJECT, &update_req("bravo", "echo", "devbox")).unwrap_err();
    assert!(matches!(err, PoolError::HooksMergeRequired { .. }), "{err}");
    assert_eq!(
        ops.calls(),
        vec!["materialize devbox/echo", "wire devbox/echo", "remove devbox/echo"]
    );
    assert_eq!(ops.state("devbox", "echo"), WorktreeState::Missing);
    assert_eq!(ops.state("hub", "bravo"), WorktreeState::Clean);
    assert_eq!(home.text(), GLOBAL);

    let retry = Script::default().with_worktree("hub", "bravo", WorktreeState::Clean);
    let changed = update_workspace_with(&retry, PROJECT, &update_req("bravo", "echo", "devbox")).unwrap();
    assert_eq!(changed.previous_name.as_deref(), Some("bravo"));
    assert_eq!(home.pool().workspace("echo").unwrap().machine, "devbox");
}

#[test]
fn a_worktree_already_at_the_destination_is_reused_not_removed_on_failure() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([("wire", "scp failed".to_string())]),
        ..Default::default()
    }
    .with_worktree("devbox", "bravo", WorktreeState::AtRisk("has notes".into()));
    let err = update_workspace_with(&ops, PROJECT, &update_req("bravo", "bravo", "devbox")).unwrap_err();
    assert!(matches!(err, PoolError::Provisioning(_)), "{err}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("remove")), "{:?}", ops.calls());
    assert_eq!(home.text(), GLOBAL);
}

#[test]
fn an_old_worktree_that_cant_be_retired_is_a_warning() {
    let home = Home::global(GLOBAL);
    let ops = Script {
        fail: HashMap::from([("remove", "permission denied".to_string())]),
        ..Default::default()
    }
    .with_worktree("hub", "bravo", WorktreeState::Clean);
    let changed = update_workspace_with(&ops, PROJECT, &update_req("bravo", "bravo", "devbox")).unwrap();
    assert_eq!(changed.warnings.len(), 1, "{:?}", changed.warnings);
    assert!(changed.warnings[0].contains("permission denied"));
    assert_eq!(home.pool().workspace("bravo").unwrap().machine, "devbox");
}

// ---------------------------------------------------------------------------
// Machines

#[test]
fn adding_a_machine_declares_it_without_provisioning() {
    let home = Home::global(GLOBAL);
    let ops = Script::default();
    let mut fields = machine_fields("gpu", MachineKind::Ssh, Some("gpu.lan"), "/srv/pa");
    fields.tags = vec!["cuda".into(), " cuda ".into()];
    let added = add_machine_with(&ops, PROJECT, &fields).unwrap();
    assert!(ops.calls().is_empty());
    assert_eq!(added.machine.tags, vec!["cuda"]);
    assert!(added.workspaces.is_empty());
    assert_eq!(home.pool().machine("gpu").unwrap().host.as_deref(), Some("gpu.lan"));
    assert!(home.text().contains("# Remote box."));
}

#[test]
fn renaming_a_machine_carries_its_workspaces_and_keeps_forwarding() {
    let home = Home::global(GLOBAL);
    add_workspace_with(&Script::default(), PROJECT, &add_req("remote-1", Some("devbox"), &[], None)).unwrap();
    let pool = home.pool();
    let req = UpdateMachine {
        name: "devbox".into(),
        expected: Some(pool.machine("devbox").unwrap().clone()),
        fields: machine_fields("buildbox", MachineKind::Ssh, Some("devbox.lan"), "/home/u/pa"),
    };
    let changed = update_machine_with(&Script::default(), PROJECT, &req).unwrap();
    assert_eq!(changed.previous_name.as_deref(), Some("devbox"));
    assert_eq!(changed.workspaces, vec!["remote-1"]);
    let pool = home.pool();
    assert_eq!(pool.workspace("remote-1").unwrap().machine, "buildbox");
    assert!(pool.machine("devbox").is_none());
    assert_eq!(pool.machine("buildbox").unwrap().forward, Some(shelbi_core::ForwardMode::Tcp));
}

#[test]
fn changing_a_machine_location_refuses_to_orphan_worktrees() {
    let home = Home::global(GLOBAL);
    let to = |dir: &str| UpdateMachine {
        name: "hub".into(),
        expected: None,
        fields: machine_fields("hub", MachineKind::Local, None, dir),
    };
    let before = home.text();

    let orphaning = Script::default().with_worktree("hub", "alpha", WorktreeState::Clean);
    let err = update_machine_with(&orphaning, PROJECT, &to("/tmp/elsewhere")).unwrap_err();
    assert!(matches!(&err, PoolError::Unsafe(m) if m.contains("orphan")), "{err}");
    assert_eq!(home.text(), before);

    let busy = Script {
        sessions: HashMap::from([("bravo".to_string(), SessionState::Alive { user_shell: false })]),
        ..Default::default()
    };
    let err = update_machine_with(&busy, PROJECT, &to("/tmp/elsewhere")).unwrap_err();
    assert!(matches!(&err, PoolError::Unsafe(m) if m.contains("live session")), "{err}");
    assert_eq!(home.text(), before);

    // No worktrees yet: nothing to orphan.
    update_machine_with(&Script::default(), PROJECT, &to("/tmp/elsewhere")).unwrap();
    assert_eq!(home.pool().machine("hub").unwrap().work_dir, PathBuf::from("/tmp/elsewhere"));
}

#[test]
fn a_machine_location_that_already_has_the_worktrees_is_accepted() {
    let home = Home::global(GLOBAL);
    add_workspace_with(&Script::default(), PROJECT, &add_req("remote-1", Some("devbox"), &[], None)).unwrap();
    let old = home.pool().machine("devbox").unwrap().clone();
    let mut new = old.clone();
    new.host = Some("devbox.tailnet".into());
    let req = UpdateMachine {
        name: "devbox".into(),
        expected: None,
        fields: machine_fields("devbox", MachineKind::Ssh, Some("devbox.tailnet"), "/home/u/pa"),
    };

    // The worktree is at the old host only: refused.
    let only_old = Script::default().with_worktree_on(&old, "remote-1", WorktreeState::Clean);
    let err = update_machine_with(&only_old, PROJECT, &req).unwrap_err();
    assert!(matches!(&err, PoolError::Unsafe(m) if m.contains("orphan")), "{err}");

    // A new name for the same host: the worktree is there too.
    let alias = Script::default()
        .with_worktree_on(&old, "remote-1", WorktreeState::Clean)
        .with_worktree_on(&new, "remote-1", WorktreeState::Clean);
    update_machine_with(&alias, PROJECT, &req).unwrap();
    assert_eq!(home.pool().machine("devbox").unwrap(), &new);
}

#[test]
fn an_edit_of_a_changed_machine_is_a_conflict() {
    let home = Home::global(GLOBAL);
    let mut expected = home.pool().machine("devbox").unwrap().clone();
    expected.work_dir = PathBuf::from("/old");
    let req = UpdateMachine {
        name: "devbox".into(),
        expected: Some(expected),
        fields: machine_fields("devbox", MachineKind::Ssh, Some("devbox.lan"), "/home/u/pa"),
    };
    let err = update_machine_with(&Script::default(), PROJECT, &req).unwrap_err();
    assert!(matches!(err, PoolError::Conflict(_)), "{err}");
    assert_eq!(home.text(), GLOBAL);
}

// ---------------------------------------------------------------------------
// The live probes, against real git

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Shelbi Test")
        .env("GIT_AUTHOR_EMAIL", "test@shelbi.local")
        .env("GIT_COMMITTER_NAME", "Shelbi Test")
        .env("GIT_COMMITTER_EMAIL", "test@shelbi.local")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repo with one commit and a detached idle worktree for `alpha`.
fn repo_with_worktree(home: &Home) -> (Machine, WorkspaceSpec) {
    let repo = home.root.join("work");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "# r\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let machine = Machine {
        name: "hub".into(),
        kind: MachineKind::Local,
        work_dir: repo.clone(),
        host: None,
        tags: Vec::new(),
        forward: None,
    };
    let spec = WorkspaceSpec {
        name: "alpha".into(),
        machine: "hub".into(),
        tags: Vec::new(),
        slot: None,
    };
    let wt = ws::workspace_worktree(&machine, &spec);
    git(&repo, &["worktree", "add", "-q", "--detach", &wt.to_string_lossy(), "main"]);
    (machine, spec)
}

#[test]
fn worktree_state_reads_missing_clean_and_at_risk_worktrees() {
    let home = Home::new();
    let (machine, spec) = repo_with_worktree(&home);
    let wt = ws::workspace_worktree(&machine, &spec);
    let other = WorkspaceSpec {
        name: "nobody".into(),
        ..spec.clone()
    };
    assert_eq!(worktree_state(&machine, &other), WorktreeState::Missing);
    assert_eq!(worktree_state(&machine, &spec), WorktreeState::Clean);

    // Shelbi's own footprint doesn't count.
    fs::create_dir_all(wt.join(".claude")).unwrap();
    fs::write(wt.join(".claude/settings.local.json"), "{}").unwrap();
    assert_eq!(worktree_state(&machine, &spec), WorktreeState::Clean);

    fs::write(wt.join("notes.txt"), "mine").unwrap();
    assert!(matches!(worktree_state(&machine, &spec), WorktreeState::AtRisk(r) if r.contains("uncommitted")));
    fs::remove_file(wt.join("notes.txt")).unwrap();

    // A commit on the detached HEAD that no branch holds.
    fs::write(wt.join("new.txt"), "x").unwrap();
    git(&wt, &["add", "new.txt"]);
    git(&wt, &["commit", "-q", "-m", "orphan"]);
    assert!(matches!(worktree_state(&machine, &spec), WorktreeState::AtRisk(r) if r.contains("no branch")));
    // Once a branch holds it, removing the worktree loses nothing.
    git(&wt, &["branch", "keep"]);
    assert_eq!(worktree_state(&machine, &spec), WorktreeState::Clean);
}

#[test]
fn live_move_and_remove_keep_files_and_verify_removal() {
    let home = Home::new();
    let (machine, spec) = repo_with_worktree(&home);
    fs::create_dir_all(shelbi_state::project_dir(PROJECT).unwrap()).unwrap();
    let mut p = fixture_project();
    p.machines = vec![machine.clone()];
    let wt = ws::workspace_worktree(&machine, &spec);
    fs::write(wt.join("ignored.env"), "SECRET=1").unwrap();
    let to = WorkspaceSpec {
        name: "delta".into(),
        ..spec.clone()
    };
    LiveOps.move_worktree(&p, &machine, &spec, &to).unwrap();
    let moved = ws::workspace_worktree(&machine, &to);
    assert!(!wt.exists());
    assert_eq!(fs::read_to_string(moved.join("ignored.env")).unwrap(), "SECRET=1");

    LiveOps.remove_worktree(&p, &machine, &to).unwrap();
    assert!(!moved.exists());
    assert_eq!(worktree_state(&machine, &to), WorktreeState::Missing);
}

#[test]
fn live_active_task_reads_in_progress_and_review_assignments() {
    let home = Home::global(GLOBAL);
    let _ = &home;
    let task = |id: &str, column: Column, ws: &str| {
        let now = chrono::Utc::now();
        shelbi_core::Issue {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: Some(ws.into()),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        }
    };
    shelbi_state::save_task(PROJECT, &task("feat-x", Column::in_progress(), "alpha"), "").unwrap();
    shelbi_state::save_task(PROJECT, &task("feat-y", Column::review(), "bravo"), "").unwrap();
    shelbi_state::save_task(PROJECT, &task("feat-z", Column::todo(), "alpha"), "").unwrap();
    let p = fixture_project();
    assert_eq!(LiveOps.active_task(&p, "alpha").unwrap().as_deref(), Some("feat-x"));
    assert_eq!(LiveOps.active_task(&p, "bravo").unwrap().as_deref(), Some("feat-y"));
    assert_eq!(LiveOps.active_task(&p, "charlie").unwrap(), None);
    let mut open = LiveOps.open_assignments(&p, "alpha").unwrap();
    open.sort();
    assert_eq!(open, vec!["feat-x", "feat-z"]);
}
