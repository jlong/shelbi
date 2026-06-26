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

    #[error("{0}")]
    Other(String),
}
