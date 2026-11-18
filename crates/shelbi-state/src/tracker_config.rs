//! Write a project's `issue_tracker:` block: the one config edit behind
//! connecting a project to a tracker (`shelbi tracker linear connect`,
//! `shelbi tracker disconnect`, Settings' tracker page).
//!
//! The block lives in the project YAML that holds the shared settings: the
//! global `projects/<name>.yaml`, or an in-repo project's committed
//! `<repo>/.shelbi/project.yaml` (the tracker is a team decision, so it is
//! shared). The edit is comment-preserving ([`crate::yaml_edit::patch`]) and
//! replaces the whole block, so the file is left with exactly the
//! connection written: one external tracker block at most, or none for the
//! file system.

use std::path::PathBuf;

use shelbi_core::{Error, IssueTrackerConfig, Result};

use crate::yaml_edit;

/// The project YAML whose `issue_tracker:` block [`write_issue_tracker`]
/// edits.
pub fn issue_tracker_file(project: &str) -> Result<PathBuf> {
    crate::workflow_config::project_config_file(project).map_err(|e| Error::Other(e.to_string()))
}

/// Replace `project`'s `issue_tracker:` block with `tracker` (removing the
/// key when `tracker` is the file-system default), keeping every comment and
/// key the edit doesn't touch. `tracker` must validate. The project must
/// still load afterwards; when it doesn't, the old text is put back and the
/// load error is returned. Returns the file written.
pub fn write_issue_tracker(project: &str, tracker: &IssueTrackerConfig) -> Result<PathBuf> {
    tracker.validate()?;
    let path = issue_tracker_file(project)?;
    let old = std::fs::read_to_string(&path)
        .map_err(|e| Error::Other(format!("reading {}: {e}", path.display())))?;
    let mut tree = yaml_edit::parse(&old)
        .map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    let Some(map) = tree.as_mapping_mut() else {
        return Err(Error::Other(format!("{} is not a YAML mapping", path.display())));
    };
    let key = yaml_serde::Value::String("issue_tracker".into());
    if tracker.is_default() {
        map.shift_remove(&key);
    } else {
        map.insert(key, yaml_serde::to_value(tracker)?);
    }
    let patched = yaml_edit::patch(&old, &tree)
        .map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    if patched.text == old {
        return Ok(path);
    }
    crate::atomic_write(&path, patched.text.as_bytes())?;
    if let Err(e) = crate::load_project(project) {
        let _ = crate::atomic_write(&path, old.as_bytes());
        return Err(Error::Other(format!(
            "the new issue_tracker block left the project unloadable, so it was not kept: {e}"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{GithubConnection, IssueTrackerBackend, LinearConnection};

    struct Home {
        path: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn home() -> Home {
        let guard = crate::test_lock::LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let path = std::env::temp_dir().join(format!(
            "shelbi-tracker-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(path.join("projects")).unwrap();
        std::env::set_var("SHELBI_HOME", &path);
        Home { path, _guard: guard }
    }

    const PROJECT: &str = "\
# The app's project.
name: app
repo: /tmp/app
default_branch: main
orchestrator:
  runner: claude # the house runner
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/app
workspaces: []
issue_tracker:
  backend: github
  # where the cards live
  github:
    repo: acme/app
  refresh_secs: 60
";

    fn linear() -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Linear,
            linear: Some(LinearConnection {
                team: "ENG".into(),
                label: Some("shelbi".into()),
                ..LinearConnection::default()
            }),
            refresh_secs: Some(60),
            ..IssueTrackerConfig::default()
        }
    }

    #[test]
    fn the_block_is_replaced_whole_and_comments_survive() {
        let home = home();
        let file = home.path.join("projects/app.yaml");
        std::fs::write(&file, PROJECT).unwrap();

        assert_eq!(write_issue_tracker("app", &linear()).unwrap(), file);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# The app's project.\n"), "{text}");
        assert!(text.contains("runner: claude # the house runner"), "{text}");
        assert!(!text.contains("github:"), "the old block is gone:\n{text}");
        let p = crate::load_project("app").unwrap();
        assert_eq!(p.issue_tracker, linear());
        assert_eq!(p.issue_tracker.connection_blocks(), vec!["linear"]);

        let github = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: "acme/web".into() }),
            ..IssueTrackerConfig::default()
        };
        write_issue_tracker("app", &github).unwrap();
        let p = crate::load_project("app").unwrap();
        assert_eq!(p.issue_tracker.connection_blocks(), vec!["github"]);

        // The file system is the default: no block at all.
        write_issue_tracker("app", &IssueTrackerConfig::default()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("issue_tracker"), "{text}");
        assert!(text.contains("runner: claude # the house runner"), "{text}");
        assert!(crate::load_project("app").unwrap().issue_tracker.is_default());
    }

    #[test]
    fn an_invalid_block_is_refused_before_anything_is_written() {
        let home = home();
        let file = home.path.join("projects/app.yaml");
        std::fs::write(&file, PROJECT).unwrap();
        let bad = IssueTrackerConfig {
            backend: IssueTrackerBackend::Linear,
            linear: Some(LinearConnection::default()),
            ..IssueTrackerConfig::default()
        };
        let err = write_issue_tracker("app", &bad).unwrap_err().to_string();
        assert!(err.contains("issue_tracker.linear.team is required"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), PROJECT);
    }
}
