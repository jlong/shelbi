use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Discovery walked up from cwd, found an in-repo `.shelbi/project.yaml`
    /// naming project `name`, but the per-user companion `local.yaml`
    /// (which pins this user's machines and workspace pool) is absent.
    /// Fresh-clone case: the caller should prompt for
    /// `shelbi init --pick-up`. Distinct from [`Error::Yaml`] so callers
    /// can tell "config exists but not registered locally" apart from
    /// "config is broken".
    #[error(
        "project `{name}` is registered in-repo at {} but has not been picked up \
         on this machine (missing {}); run `shelbi init --pick-up` to register \
         your local machines and workspaces",
        config_path.display(),
        expected_local.display()
    )]
    ProjectNotPickedUp {
        name: String,
        config_path: PathBuf,
        expected_local: PathBuf,
    },

    /// The in-repo `.shelbi/project.yaml` located by the discovery walk-up
    /// exists but does not parse. Surfaces the exact file path so the user
    /// isn't left guessing which of the many project YAMLs was corrupt.
    #[error("failed to parse in-repo project config at {}: {source}", path.display())]
    InRepoProjectParse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("invalid agent id: {0}")]
    InvalidAgentId(String),

    #[error(
        "task id `{id}` is too long: {len} bytes (max {max}); git ref names \
         (`shelbi/<id>`) must stay under GitHub's 255-byte limit"
    )]
    TaskIdTooLong { id: String, len: usize, max: usize },

    #[error("machine `{0}` not found in project")]
    UnknownMachine(String),

    #[error("agent runner `{0}` not declared in project")]
    UnknownRunner(String),

    #[error("external command failed: {cmd}: {status}\n--- stderr ---\n{stderr}")]
    Command {
        cmd: String,
        status: String,
        stderr: String,
    },

    #[error("unknown task id(s) in depends_on: {0}")]
    UnknownDepends(String),

    #[error("dependency cycle: {0}")]
    DependencyCycle(String),

    #[error("invalid workflow: {0}")]
    InvalidWorkflow(String),

    #[error("invalid statuses.yml: {0}")]
    InvalidProjectStatuses(String),

    /// A workflow's `git:` block references one or more `{{var}}`
    /// placeholders that aren't present in the task's frontmatter
    /// parameters. The message is hand-tuned (singular vs. plural,
    /// concrete example) so the user immediately knows what to add —
    /// see `Plans/workflows.md` §12 "Parameterization".
    #[error("{}", missing_task_params_message(.workflow, .params))]
    MissingTaskParams { workflow: String, params: Vec<String> },

    /// A split project YAML (in-repo mode) has a key that belongs on the
    /// other side of the split — e.g. `machines:` in the shared file, or
    /// `zen:` in the user-local file. The message names the field and
    /// points at the correct file so the fix is obvious. See
    /// `Plans/in-repo-vs-global-project-config.md` §3.
    #[error(
        "project YAML field `{field}` is in the {found_in} file but belongs \
         in the {expected_in} file; move it to the {expected_in} YAML"
    )]
    MisplacedProjectField {
        field: String,
        found_in: &'static str,
        expected_in: &'static str,
    },

    #[error("{0}")]
    Other(String),
}

fn missing_task_params_message(workflow: &str, params: &[String]) -> String {
    match params {
        [] => format!("workflow `{workflow}` requires unknown parameters"),
        [one] => format!(
            "workflow `{workflow}` requires parameter `{one}`; \
             add `{one}: <value>` to the task frontmatter"
        ),
        many => {
            let list = many
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let first = &many[0];
            format!(
                "workflow `{workflow}` requires parameters {list}; \
                 add them to the task frontmatter (e.g. `{first}: <value>`)"
            )
        }
    }
}
