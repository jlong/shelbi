//! Picking up a teammate's committed `<repo>/.shelbi/project.yaml`: the
//! Folder step's offer when detection finds one
//! ([`shelbi_app::new_project::Facts::pick_up`]), and `shelbi init
//! --pick-up`.
//!
//! The committed file is never touched. Only the user-local half is
//! registered (`~/.shelbi/projects/<alias>/local.yaml`), and the teammate's
//! agents, workflows and statuses then resolve to (and materialize under) the
//! repo's committed `.shelbi/`. When the committed name is already taken
//! locally, the local alias is suffixed (`-2`, `-3`, …).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use shelbi_core::{AgentDefaults, IssueTrackerConfig};

use super::detect::{detect_facts, read_in_repo_name};
use super::{
    ensure_home, project_name_collides, render_project_yaml, render_split_project_yaml, run_stage,
    scaffold_agents, scaffold_workflows, validate_project_name, write_new_project_registration,
    write_workspace_settings_template, ScaffoldReporter, ScaffoldStage, Starter, IN_REPO_CONFIG_REL,
};

/// What [`pick_up`] registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedUp {
    /// The committed `name:`.
    pub canonical_name: String,
    /// The id this machine registered it under.
    pub alias: String,
    /// `Some(canonical_name)` when that name was taken here and the alias
    /// was suffixed.
    pub suffixed_from: Option<String>,
    /// The registration written: the split `<alias>/local.yaml`, or the flat
    /// `<alias>.yaml` for a legacy name-only committed config.
    pub registration_path: PathBuf,
}

/// Register the committed config in `root` on this machine, under `alias`
/// (normalized; an error when it's taken) or else the committed name
/// (suffixed when taken). Runs as the [`ScaffoldStage`]s, like a creation.
pub fn pick_up(root: &Path, alias: Option<&str>, out: &mut dyn ScaffoldReporter) -> Result<PickedUp> {
    let config_path = root.join(IN_REPO_CONFIG_REL);
    if !config_path.is_file() {
        bail!(
            "no committed config at {} — expected `<repo>/{}`. Did you mean plain \
             `shelbi init` for a fresh project?",
            config_path.display(),
            IN_REPO_CONFIG_REL
        );
    }
    let canonical_name = read_in_repo_name(&config_path).ok_or_else(|| {
        anyhow!(
            "the committed {} is missing a `name:` key — refusing to pick up an unnamed project. \
             Ask a teammate to fix the committed file, or scaffold a fresh one with `shelbi init`.",
            config_path.display()
        )
    })?;
    // The committed name is attacker-influenced by design (pick-up runs on a
    // repo we didn't create), so it clears the same charset gate as any
    // other name BEFORE it's joined into a registry path.
    validate_project_name(&canonical_name)
        .with_context(|| format!("the committed {} carries an unusable project name", config_path.display()))?;

    let (registration_path, picked, _scaffold_lock) = run_stage(out, ScaffoldStage::Config, |out| {
        ensure_home(out)?;
        // The same registration transaction lock as a fresh scaffold, so a
        // creation can't seed Welcome state for an alias this claims.
        let lock = shelbi_state::lock_project_scaffold().map_err(|e| anyhow!(e))?;
        let (alias, suffixed_from) = match alias {
            Some(raw) => {
                let alias = shelbi_core::normalize_project_name(raw)
                    .map_err(|_| anyhow!("`{raw}` has no letters or digits to build a project id from"))?;
                if project_name_collides(&alias)? {
                    bail!(
                        "a shelbi project named `{alias}` already exists locally — pick a \
                         different name, or leave it out to let shelbi suffix `{canonical_name}`."
                    );
                }
                (alias, None)
            }
            None => next_available_alias(&canonical_name)?,
        };
        if let Some(from) = &suffixed_from {
            out.note(&format!(
                "note: local alias `{from}` was already taken — using `{alias}` on this machine instead \
                 (the committed name is unchanged)"
            ));
        }
        let path = register(root, &config_path, &alias, out)?;
        write_workspace_settings_template(&alias, out)?;
        // Held through the last write.
        Ok((path, (alias, suffixed_from), lock))
    })?;
    let (alias, suffixed_from) = picked;
    run_stage(out, ScaffoldStage::Agents, |out| {
        // The committed config names the runner; the agents keep theirs.
        scaffold_agents(&alias, shelbi_core::RunnerKind::Claude, out)
    })?;
    run_stage(out, ScaffoldStage::Workflows, |out| scaffold_workflows(&alias, out))?;

    if suffixed_from.is_some() {
        out.note(&format!(
            "✓ picked up `{canonical_name}` from {} as local alias `{alias}`.",
            config_path.display()
        ));
        out.note("  Tip: `shelbi project rename` can retitle the local alias to something friendlier.");
    } else {
        out.note(&format!("✓ picked up `{canonical_name}` from {}.", config_path.display()));
    }
    Ok(PickedUp { canonical_name, alias, suffixed_from, registration_path })
}

/// Publish the user-local registration for `alias`. A complete committed
/// shared half gets the split `local.yaml`; a legacy name-only commit (from
/// before the split) gets a flat registration carrying the whole picture, so
/// the project still loads.
fn register(root: &Path, config_path: &Path, alias: &str, out: &mut dyn ScaffoldReporter) -> Result<PathBuf> {
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    // The registration's board and runner are the committed config's
    // (shared fields), so the local half's defaults are inert; the branch
    // and remote are detected like any new project's.
    let facts = detect_facts(root, shelbi_app::new_project::Repo::Git);
    let tracker = IssueTrackerConfig::default();
    let defaults = AgentDefaults::default();
    let starter = Starter {
        name: alias,
        display_name: None,
        work_dir: root,
        default_branch: &facts.default_branch,
        github_url: facts.remote.as_deref(),
        issue_tracker: &tracker,
        defaults: &defaults,
    };
    let (_shared, local_body) = render_split_project_yaml(&starter)?;
    let committed = std::fs::read_to_string(config_path).with_context(|| format!("reading {}", config_path.display()))?;
    let complete_shared = shelbi_core::Project::from_split_yaml_str(&committed, &local_body).is_ok();
    let concurrently = || {
        anyhow!(
            "a shelbi project named `{alias}` was registered concurrently; retry \
             the pick-up to choose the next available local alias"
        )
    };
    if complete_shared {
        let path = projects_dir.join(alias).join("local.yaml");
        if !write_new_project_registration(alias, &path, &local_body)? {
            return Err(concurrently());
        }
        out.note(&format!("✓ registered project (in-repo): {}", path.display()));
        Ok(path)
    } else {
        let path = projects_dir.join(format!("{alias}.yaml"));
        let yaml = render_project_yaml(&starter, Some("in-repo"))?;
        if !write_new_project_registration(alias, &path, &yaml)? {
            return Err(concurrently());
        }
        out.note(&format!("✓ registered project: {}", path.display()));
        Ok(path)
    }
}

/// `(local_alias, suffixed_from)`: `canonical` when it's free, else the
/// first free `canonical-2`, `canonical-3`, … (`suffixed_from` then names
/// `canonical`). Gives up at `-999`.
pub fn next_available_alias(canonical: &str) -> Result<(String, Option<String>)> {
    if !project_name_collides(canonical)? {
        return Ok((canonical.to_string(), None));
    }
    for n in 2..=999 {
        let candidate = format!("{canonical}-{n}");
        if !project_name_collides(&candidate)? {
            return Ok((candidate, Some(canonical.to_string())));
        }
    }
    bail!(
        "couldn't allocate a local alias for `{canonical}` — every suffix up to `{canonical}-999` \
         is taken. Rename or remove an existing project YAML in ~/.shelbi/projects/ first."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_create::{CollectReporter, NullReporter};

    /// A Shelbi home and a repo whose committed config is `committed`.
    fn sandbox(committed: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", temp.path().join("home"));
        let repo = temp.path().join("clone");
        std::fs::create_dir_all(repo.join(".shelbi")).unwrap();
        std::fs::write(repo.join(IN_REPO_CONFIG_REL), committed).unwrap();
        (temp, repo)
    }

    fn complete_shared(name: &str, repo: &Path) -> String {
        let (tracker, defaults) = (IssueTrackerConfig::default(), AgentDefaults::default());
        let starter = Starter {
            name,
            display_name: None,
            work_dir: repo,
            default_branch: "main",
            github_url: None,
            issue_tracker: &tracker,
            defaults: &defaults,
        };
        render_split_project_yaml(&starter).unwrap().0
    }

    #[test]
    fn a_complete_committed_config_gets_the_split_registration_and_stays_untouched() {
        let _g = crate::test_lock::acquire();
        let (_temp, repo) = sandbox("name: placeholder\n");
        let shared = complete_shared("teamproj", &repo);
        std::fs::write(repo.join(IN_REPO_CONFIG_REL), &shared).unwrap();

        let mut out = CollectReporter::default();
        let picked = pick_up(&repo, None, &mut out).unwrap();
        assert_eq!((picked.alias.as_str(), picked.suffixed_from.as_deref()), ("teamproj", None));
        let projects = shelbi_state::projects_dir().unwrap();
        assert_eq!(picked.registration_path, projects.join("teamproj/local.yaml"));
        assert!(!projects.join("teamproj.yaml").exists(), "no flat registration beside the split");
        assert_eq!(std::fs::read_to_string(repo.join(IN_REPO_CONFIG_REL)).unwrap(), shared, "never rewritten");
        let loaded = shelbi_state::load_project("teamproj").unwrap();
        assert_eq!(loaded.config_mode, Some(shelbi_core::ConfigMode::InRepo));
        assert_eq!(loaded.repo, repo.to_string_lossy());
        assert_eq!(out.stages.len(), 6, "it reports the creation's stages: {:?}", out.stages);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_legacy_name_only_commit_falls_back_to_a_flat_registration() {
        let _g = crate::test_lock::acquire();
        let (_temp, repo) = sandbox("name: legacy\n");
        let picked = pick_up(&repo, None, &mut NullReporter).unwrap();
        let projects = shelbi_state::projects_dir().unwrap();
        assert_eq!(picked.registration_path, projects.join("legacy.yaml"));
        let loaded = shelbi_state::load_project("legacy").unwrap();
        assert_eq!(loaded.repo, repo.to_string_lossy());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_taken_name_gets_the_next_free_alias_and_a_bad_one_is_refused() {
        let _g = crate::test_lock::acquire();
        let (_temp, repo) = sandbox("name: team\n");
        let projects = shelbi_state::projects_dir().unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("team.yaml"), "name: team\n").unwrap();
        std::fs::write(projects.join("team-2.yaml"), "name: team-2\n").unwrap();
        assert_eq!(next_available_alias("team").unwrap(), ("team-3".into(), Some("team".into())));
        assert_eq!(next_available_alias("free").unwrap(), ("free".into(), None));

        let picked = pick_up(&repo, None, &mut NullReporter).unwrap();
        assert_eq!((picked.alias.as_str(), picked.suffixed_from.as_deref()), ("team-3", Some("team")));
        // An alias the user names must be free.
        assert!(pick_up(&repo, Some("Team"), &mut NullReporter).is_err());

        std::fs::write(repo.join(IN_REPO_CONFIG_REL), "name: ../../evil\n").unwrap();
        assert!(pick_up(&repo, None, &mut NullReporter).is_err(), "a hostile committed name never reaches a path");
        std::env::remove_var("SHELBI_HOME");
    }
}
