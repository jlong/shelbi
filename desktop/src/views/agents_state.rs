//! Settings ▸ Agents' state (`dt-agents-settings-page`). The state itself is
//! toolkit-free and shared with the TUI's page: it lives in
//! [`shelbi_app::settings::agents`] (`tui-settings-agents` moved it there),
//! re-exported here so the page's paths stay as they were. What stays is the
//! desktop's Figma-frame fixture and the test homes built on it.

pub use shelbi_app::settings::agents::*;

/// The Agents frame's content (`--visual-figma`, and the tests): the
/// Workflows fixture's catalog and workflows (which assign the
/// orchestrator, developer and review agents), the shipped agents as
/// `shelbi init` writes them with the orchestrator's and the review agent's
/// instructions edited (the frame's two Customized badges), and the frame's
/// shared instructions.
#[cfg(any(test, feature = "visual-check"))]
pub(crate) mod fixture {
    use std::path::Path;

    use super::super::workflows_state::fixture::{STATUSES, SUBTASK, TASK};

    /// The frame's shared instructions.
    pub const PREAMBLE: &str = "\
You are working on Shelbi, an agent orchestrator built with Rust.

Keep changes focused on the assigned task. Follow the project’s conventions and run the relevant checks before handing work back.

Work on a task branch in your assigned workspace. Preserve unrelated changes and never commit directly to main.
";

    /// Write the frame's `workflows/` and `agents/` into project
    /// `project`'s directory `project_dir` (under the current
    /// `SHELBI_HOME`).
    pub fn write(project_dir: &Path, project: &str) {
        let workflows = project_dir.join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("statuses.yaml"), STATUSES).unwrap();
        std::fs::write(workflows.join("task.yaml"), TASK).unwrap();
        std::fs::write(workflows.join("subtask.yaml"), SUBTASK).unwrap();
        let agents = project_dir.join("agents");
        let _ = std::fs::remove_dir_all(&agents);
        shelbi_state::materialize_default_agents(project).expect("the shipped agents");
        for agent in ["orchestrator", "review"] {
            let path = agents.join(agent).join("instructions.md");
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str("\nKeep this project's release notes current.\n");
            std::fs::write(path, text).unwrap();
        }
        std::fs::create_dir_all(agents.join("_shared")).unwrap();
        std::fs::write(agents.join("_shared").join("preamble.md"), PREAMBLE).unwrap();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::workflows_state::tests::{Home, PROJECT};
    use super::*;

    /// A throwaway home holding the Agents fixture.
    pub(crate) fn home(tag: &str) -> Home {
        home_for(tag, PROJECT)
    }

    /// [`home`] with the fixture project named `project`.
    pub(crate) fn home_for(tag: &str, project: &str) -> Home {
        let home = Home::with_project(&format!("agents-{tag}"), project);
        fixture::write(&home.root.join("projects").join(project), project);
        home
    }
}
