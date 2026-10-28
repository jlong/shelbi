//! Settings ▸ Workflows' state, toolkit-free (`dt-wf-settings-index`). It
//! moved to `shelbi_app::settings::workflows` (`tui-settings-workflows`) so
//! the TUI's Workflows page runs the same edits, queue and guards; this
//! module re-exports it under its old path, and keeps the desktop tests'
//! throwaway home.

pub use shelbi_app::settings::workflows::*;

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    use super::fixture;

    /// A throwaway `SHELBI_HOME` holding one project (its YAML, an `agents/`
    /// for the developer and review agents, and the frame's two workflows
    /// over the default catalog), set for the test's length. Tests
    /// that set it run one at a time ([`crate::test_env::lock`]).
    pub(crate) struct Home {
        pub root: PathBuf,
        project: String,
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        /// The fixture project, named `project`.
        pub(crate) fn with_project(tag: &str, project: &str) -> Home {
            let lock = crate::test_env::lock();
            let root = PathBuf::from(format!("/tmp/shb-wf-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let prev = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", &root);
            write_project(&root, project);
            Home {
                root,
                project: project.to_string(),
                prev,
                _lock: lock,
            }
        }

        pub(crate) fn workflows(&self) -> PathBuf {
            self.root.join("projects").join(&self.project).join("workflows")
        }

        pub(crate) fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.workflows().join(file)).unwrap()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The fixture project: its YAML (Task the default), and the frame's
    /// workflows ([`fixture::write`]).
    pub(crate) fn write_project(root: &Path, project: &str) {
        fixture::write_project(&root.join("projects"), project);
    }
}
