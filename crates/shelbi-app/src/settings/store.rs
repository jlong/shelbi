//! Where Settings' config saves go.
//!
//! The workflow and agent pages and editors build each save with their own
//! comment-preserving patcher ([`WorkflowConfig::save_plan`],
//! [`AgentsConfig::save_plan`]) and hand the plan to a [`ConfigStore`]:
//!
//! - a window's [`ProjectHandle`] sends it to the project's daemon as one
//!   `SaveConfig` control message ([`crate::source::ProjectLink::save_config`]):
//!   this machine's for a local project, the far machine's for a project on
//!   another one, which writes it there and refuses it when a file changed
//!   since the window read it ("changed on devbox since it was loaded").
//!   The model as saved is read back through the window's source;
//! - [`Disk`] writes it here directly, for a client with no window handle
//!   and for tests.

use std::path::PathBuf;
use std::sync::Arc;

use shelbi_state::agents_config::{self, AgentError, AgentsConfig};
use shelbi_state::config_write::Plan;
use shelbi_state::workflow_config::{self, ConfigError, WorkflowConfig};

use crate::source::ProjectHandle;

/// Why a store didn't write a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteFailure {
    /// These files changed since they were read: nothing was written.
    /// `host` names the machine they're on when that isn't this one.
    Changed { paths: Vec<PathBuf>, host: Option<String> },
    /// Anything else, said for the user.
    Failed(String),
}

/// Where a project's config saves are written, and read back from.
pub trait ConfigStore: Send + Sync {
    /// Write `plan`, or refuse it whole.
    fn write(&self, plan: &Plan) -> Result<(), WriteFailure>;
    /// The project's workflow config as it is now.
    fn workflows(&self) -> Result<WorkflowConfig, String>;
    /// The project's agents as they are now.
    fn agents(&self) -> Result<AgentsConfig, String>;
}

/// A store shared with a save's worker thread.
pub type Store = Arc<dyn ConfigStore>;

impl ConfigStore for ProjectHandle {
    fn write(&self, plan: &Plan) -> Result<(), WriteFailure> {
        self.link.save_config(plan)
    }

    fn workflows(&self) -> Result<WorkflowConfig, String> {
        self.source.workflow_config()
    }

    fn agents(&self) -> Result<AgentsConfig, String> {
        self.source.agents_config()
    }
}

/// `project`'s files on this machine, written directly.
#[derive(Debug, Clone)]
pub struct Disk {
    pub project: String,
}

impl Disk {
    pub fn store(project: &str) -> Store {
        Arc::new(Disk {
            project: project.to_string(),
        })
    }
}

impl ConfigStore for Disk {
    fn write(&self, plan: &Plan) -> Result<(), WriteFailure> {
        shelbi_state::config_write::check_and_run(plan).map_err(local_failure)
    }

    fn workflows(&self) -> Result<WorkflowConfig, String> {
        WorkflowConfig::load(&self.project).map_err(|e| e.to_string())
    }

    fn agents(&self) -> Result<AgentsConfig, String> {
        AgentsConfig::load(&self.project).map_err(|e| e.to_string())
    }
}

/// `project` on this machine, as a client with no window handle saves it
/// (the TUI): through this machine's daemon when it's running
/// ([`save_here`]), read back from disk.
#[derive(Debug, Clone)]
pub struct Local {
    pub project: String,
}

impl Local {
    pub fn store(project: &str) -> Store {
        Arc::new(Local {
            project: project.to_string(),
        })
    }
}

impl ConfigStore for Local {
    fn write(&self, plan: &Plan) -> Result<(), WriteFailure> {
        save_here(&self.project, plan)
    }

    fn workflows(&self) -> Result<WorkflowConfig, String> {
        WorkflowConfig::load(&self.project).map_err(|e| e.to_string())
    }

    fn agents(&self) -> Result<AgentsConfig, String> {
        AgentsConfig::load(&self.project).map_err(|e| e.to_string())
    }
}

/// Save `plan` for `project` on this machine: as a `SaveConfig` to this
/// machine's daemon when one answers, so the daemon stays the one writer;
/// with none running nothing else writes through it, and the plan is
/// written here. Blocking.
pub fn save_here(project: &str, plan: &Plan) -> Result<(), WriteFailure> {
    match crate::exec_daemon::connect_running() {
        Some(mut client) => send_save(&mut client, project, plan, None),
        None => shelbi_state::config_write::check_and_run(plan).map_err(local_failure),
    }
}

/// Send `plan` for `project` as one `SaveConfig` on `client` (the daemon
/// on `host`, `None` for this machine's) and wait for its answer.
pub(crate) fn send_save(
    client: &mut shelbi_client::ControlClient,
    project: &str,
    plan: &Plan,
    host: Option<String>,
) -> Result<(), WriteFailure> {
    let failed = |e: String| match &host {
        Some(host) => WriteFailure::Failed(format!("{host}: {e}")),
        None => WriteFailure::Failed(e),
    };
    if let Some(refused) = crate::exec_daemon::out_of_date(client) {
        return Err(WriteFailure::Failed(refused.to_string()));
    }
    let request = shelbi_proto::control::SaveConfigRequest {
        request_id: 1,
        project: project.to_string(),
        surface: plan.surface.as_str().to_string(),
        plan: serde_json::to_value(plan).map_err(|e| failed(e.to_string()))?,
    };
    match client.save_config(&request) {
        Ok(()) => Ok(()),
        Err(shelbi_client::ClientError::Mutation(shelbi_proto::control::MutationError::ConfigChanged { paths })) => {
            Err(WriteFailure::Changed {
                paths: paths.into_iter().map(PathBuf::from).collect(),
                host,
            })
        }
        Err(e) => Err(failed(e.to_string())),
    }
}

/// A plan this machine refused, as a store says it.
pub fn local_failure(e: shelbi_state::config_write::WriteError) -> WriteFailure {
    match e {
        shelbi_state::config_write::WriteError::Changed(paths) => WriteFailure::Changed { paths, host: None },
        other => WriteFailure::Failed(other.to_string()),
    }
}

/// Save `config` through `store`: plan it, write the plan, read the model
/// back.
pub fn save_workflows(store: &dyn ConfigStore, config: &WorkflowConfig) -> Result<workflow_config::Saved, ConfigError> {
    let plan = config.save_plan()?;
    store.write(&plan.plan).map_err(|e| match e {
        WriteFailure::Changed { paths, host: None } => ConfigError::Conflict(paths),
        WriteFailure::Changed { paths, host: Some(host) } => ConfigError::ChangedOn { host, paths },
        WriteFailure::Failed(message) => ConfigError::Invalid(message),
    })?;
    let config = store.workflows().map_err(ConfigError::Invalid)?;
    Ok(plan.saved(config))
}

/// Save `config` through `store`: plan it, write the plan, read the model
/// back.
pub fn save_agents(store: &dyn ConfigStore, config: &AgentsConfig) -> Result<agents_config::Saved, AgentError> {
    let plan = config.save_plan()?;
    store.write(&plan.plan).map_err(|e| match e {
        WriteFailure::Changed { paths, host: None } => AgentError::Conflict(paths),
        WriteFailure::Changed { paths, host: Some(host) } => AgentError::ChangedOn { host, paths },
        WriteFailure::Failed(message) => AgentError::Invalid(message),
    })?;
    let config = store.agents().map_err(AgentError::Invalid)?;
    Ok(plan.saved(config))
}
