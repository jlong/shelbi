use super::*;
use crate::test_lock::LOCK as TEST_LOCK;
use shelbi_core::MachineKind;

const PROJECT: &str = "pc";

/// A temp `SHELBI_HOME`; restores the environment on drop.
struct Home {
    root: PathBuf,
    projects: PathBuf,
    _lock: Box<dyn std::any::Any>,
}

impl Home {
    fn new() -> Home {
        let lock = TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "shelbi-pool-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &root);
        let projects = crate::projects_dir().unwrap();
        fs::create_dir_all(&projects).unwrap();
        Home {
            root,
            projects,
            _lock: Box::new(lock),
        }
    }

    /// A global project whose YAML is `text`.
    fn global(text: &str) -> Home {
        let home = Home::new();
        fs::write(home.global_path(), text).unwrap();
        home
    }

    /// An in-repo project: `shared` at `<repo>/.shelbi/project.yaml`, `local`
    /// (with `repo:` prepended) at `<name>/local.yaml`.
    fn in_repo(shared: &str, local: &str) -> Home {
        let home = Home::new();
        let repo = home.root.join("repo");
        fs::create_dir_all(repo.join(".shelbi")).unwrap();
        fs::write(repo.join(".shelbi/project.yaml"), shared).unwrap();
        fs::create_dir_all(home.projects.join(PROJECT)).unwrap();
        fs::write(
            home.local_path(),
            format!("repo: {}\n{local}", repo.display()),
        )
        .unwrap();
        home
    }

    fn global_path(&self) -> PathBuf {
        self.projects.join(format!("{PROJECT}.yaml"))
    }

    fn local_path(&self) -> PathBuf {
        self.projects.join(PROJECT).join("local.yaml")
    }

    fn shared_path(&self) -> PathBuf {
        self.root.join("repo/.shelbi/project.yaml")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&self.root);
    }
}

const GLOBAL: &str = "\
# Project header.
repo: /tmp/pc
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude: { command: claude, flags: [] }
# The machines.
machines:
  - name: hub
    kind: local
    work_dir: /tmp/pc
  # The build box.
  - name: devbox
    kind: ssh
    host: devbox.lan
    work_dir: /home/u/pc
    forward: tcp
    future_key: keep-me
workspaces:
  - name: alpha
    machine: hub
  # Review slot.
  - name: review-1
    machine: hub
    role: review
    hand_key: 7
";

fn spec(name: &str, machine: &str, tags: &[&str], slot: Option<u32>) -> WorkspaceSpec {
    WorkspaceSpec {
        name: name.to_string(),
        machine: machine.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        slot,
    }
}

fn kept_machines(pool: &PoolConfig) -> Vec<PoolEntry<Machine>> {
    pool.machines().iter().cloned().map(PoolEntry::kept).collect()
}

fn kept_workspaces(pool: &PoolConfig) -> Vec<PoolEntry<WorkspaceSpec>> {
    pool.workspaces().iter().cloned().map(PoolEntry::kept).collect()
}

#[test]
fn loads_global_pool_from_the_project_yaml() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    assert_eq!(pool.mode(), ConfigMode::Global);
    assert_eq!(pool.path(), home.global_path());
    assert_eq!(pool.machines().len(), 2);
    assert_eq!(pool.machine("devbox").unwrap().host.as_deref(), Some("devbox.lan"));
    // Legacy `role: review` reads as the tag.
    assert_eq!(pool.workspace("review-1").unwrap().tags, vec!["review"]);
}

#[test]
fn adding_a_workspace_keeps_comments_unknown_keys_and_unrelated_settings() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("bravo", "devbox", &["gpu"], Some(0))));
    let saved = pool.save(&kept_machines(&pool), &workspaces).unwrap();

    let bravo = saved.workspace("bravo").unwrap();
    assert_eq!(bravo.tags, vec!["gpu"]);
    assert_eq!(bravo.slot, Some(0), "an explicit slot 0 is kept, not elided");
    let text = fs::read_to_string(home.global_path()).unwrap();
    for kept in [
        "# Project header.",
        "# The machines.",
        "# The build box.",
        "# Review slot.",
        "forward: tcp",
        "future_key: keep-me",
        "hand_key: 7",
        "role: review",
        "agent_runners:\n  claude: { command: claude, flags: [] }",
    ] {
        assert!(text.contains(kept), "lost `{kept}`:\n{text}");
    }
}

#[test]
fn editing_a_workspace_merges_onto_its_raw_entry() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let workspaces: Vec<_> = pool
        .workspaces()
        .iter()
        .map(|w| {
            if w.name == "review-1" {
                PoolEntry {
                    was: Some("review-1".into()),
                    value: spec("review-2", "hub", &["fast"], Some(3)),
                }
            } else {
                PoolEntry::kept(w.clone())
            }
        })
        .collect();
    let saved = pool.save(&kept_machines(&pool), &workspaces).unwrap();

    // The legacy role is dropped with the rewrite, so the removed `review`
    // tag stays removed.
    let renamed = saved.workspace("review-2").unwrap();
    assert_eq!(renamed.tags, vec!["fast"]);
    assert_eq!(renamed.slot, Some(3));
    assert!(saved.workspace("review-1").is_none());
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(!text.contains("role:"), "{text}");
    assert!(text.contains("hand_key: 7"), "unknown key kept on rename:\n{text}");
    assert!(text.contains("# Review slot."), "{text}");
}

#[test]
fn renaming_a_machine_moves_its_workspaces_in_one_write() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let machines: Vec<_> = pool
        .machines()
        .iter()
        .map(|m| {
            let mut value = m.clone();
            if m.name == "hub" {
                value.name = "laptop".into();
            }
            PoolEntry {
                was: Some(m.name.clone()),
                value,
            }
        })
        .collect();
    let workspaces: Vec<_> = pool
        .workspaces()
        .iter()
        .map(|w| {
            let mut value = w.clone();
            value.machine = "laptop".into();
            PoolEntry::kept(value)
        })
        .collect();
    let saved = pool.save(&machines, &workspaces).unwrap();
    assert!(saved.machine("hub").is_none());
    assert!(saved.workspaces().iter().all(|w| w.machine == "laptop"));
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(text.contains("future_key: keep-me"), "{text}");
}

#[test]
fn a_save_that_breaks_the_config_writes_nothing() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("ghost", "nowhere", &[], None)));
    let err = pool.save(&kept_machines(&pool), &workspaces).unwrap_err();
    assert!(matches!(err, PoolConfigError::Invalid(_)), "{err}");
    assert_eq!(fs::read_to_string(home.global_path()).unwrap(), GLOBAL);
}

#[test]
fn an_unchanged_save_writes_nothing() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    pool.save(&kept_machines(&pool), &kept_workspaces(&pool))
        .unwrap();
    assert_eq!(fs::read_to_string(home.global_path()).unwrap(), GLOBAL);
}

#[test]
fn a_pool_changed_on_disk_since_the_load_is_a_conflict() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let edited = GLOBAL.replace("  - name: alpha\n", "  - name: alpha\n    slot: 9\n");
    fs::write(home.global_path(), &edited).unwrap();

    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("bravo", "hub", &[], None)));
    let err = pool.save(&kept_machines(&pool), &workspaces).unwrap_err();
    assert!(matches!(err, PoolConfigError::Conflict { .. }), "{err}");
    assert_eq!(fs::read_to_string(home.global_path()).unwrap(), edited);
}

#[test]
fn an_unrelated_change_on_disk_is_kept_and_the_edit_applied_to_it() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let edited = GLOBAL.replace("default_branch: main", "default_branch: trunk");
    fs::write(home.global_path(), &edited).unwrap();

    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("bravo", "hub", &[], None)));
    let saved = pool.save(&kept_machines(&pool), &workspaces).unwrap();
    assert!(saved.workspace("bravo").is_some());
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(text.contains("default_branch: trunk"), "{text}");
}

#[test]
fn in_repo_pools_are_written_to_local_yaml_only() {
    let shared = "\
name: PC
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude: { command: claude, flags: [] }
";
    let local = "\
# Local half.
machines:
  - { name: hub, kind: local, work_dir: /tmp/pc }
workspaces:
  - { name: alpha, machine: hub }
";
    let home = Home::in_repo(shared, local);
    let pool = PoolConfig::load(PROJECT).unwrap();
    assert_eq!(pool.mode(), ConfigMode::InRepo);
    assert_eq!(pool.path(), home.local_path());

    let mut machines = kept_machines(&pool);
    machines.push(PoolEntry::new(Machine {
        name: "devbox".into(),
        kind: MachineKind::Ssh,
        work_dir: "/home/u/pc".into(),
        host: Some("devbox.lan".into()),
        tags: vec!["gpu".into()],
        forward: None,
    }));
    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("bravo", "devbox", &[], None)));
    let saved = pool.save(&machines, &workspaces).unwrap();
    assert_eq!(saved.workspace("bravo").unwrap().machine, "devbox");

    assert_eq!(fs::read_to_string(home.shared_path()).unwrap(), shared);
    let text = fs::read_to_string(home.local_path()).unwrap();
    assert!(text.contains("# Local half."), "{text}");
    assert!(text.contains("devbox.lan"), "{text}");
    assert!(!home.global_path().exists());
}

#[test]
fn a_legacy_workers_key_is_left_alone_until_the_list_changes() {
    let legacy = GLOBAL.replace("workspaces:", "workers:");
    let home = Home::global(&legacy);
    let pool = PoolConfig::load(PROJECT).unwrap();

    // A machine-only edit doesn't touch the workspace list or its key.
    let mut machines = kept_machines(&pool);
    machines[0].value.tags = vec!["fast".into()];
    let pool = pool.save(&machines, &kept_workspaces(&pool)).unwrap();
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(text.contains("workers:"), "{text}");

    let mut workspaces = kept_workspaces(&pool);
    workspaces.push(PoolEntry::new(spec("bravo", "hub", &[], None)));
    let saved = pool.save(&kept_machines(&pool), &workspaces).unwrap();
    assert_eq!(saved.workspaces().len(), 3);
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(!text.contains("workers:"), "{text}");
    assert!(text.contains("workspaces:"), "{text}");
    // Rewritten in place: still after the machines.
    assert!(text.find("machines:").unwrap() < text.find("workspaces:").unwrap());
}

#[test]
fn removing_a_workspace_keeps_the_others_verbatim() {
    let home = Home::global(GLOBAL);
    let pool = PoolConfig::load(PROJECT).unwrap();
    let workspaces: Vec<_> = kept_workspaces(&pool)
        .into_iter()
        .filter(|w| w.value.name != "alpha")
        .collect();
    let saved = pool.save(&kept_machines(&pool), &workspaces).unwrap();
    assert!(saved.workspace("alpha").is_none());
    let text = fs::read_to_string(home.global_path()).unwrap();
    assert!(text.contains("role: review"), "untouched entry stays as written:\n{text}");
    assert!(text.contains("hand_key: 7"), "{text}");
}
