use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),

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

    /// A workflow's `git:` block references one or more `{{var}}`
    /// placeholders that aren't present in the task's frontmatter
    /// parameters. The message is hand-tuned (singular vs. plural,
    /// concrete example) so the user immediately knows what to add —
    /// see `Plans/workflows.md` §12 "Parameterization".
    #[error("{}", missing_task_params_message(.workflow, .params))]
    MissingTaskParams { workflow: String, params: Vec<String> },

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
