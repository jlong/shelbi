//! `shelbi remote`: Shelbi on another machine.
//!
//! - `shelbi remote list` is this machine's Shelbi as another machine sees
//!   it. Another machine's Connect to Remote Session dialog runs
//!   `shelbi remote list --json` here over `ssh` (see
//!   `shelbi_app::launch::ssh`): the user Shelbi runs as, its version,
//!   whether the daemon is running, and the registered projects. It only
//!   reads: it never starts the daemon or touches a project.
//! - `shelbi remote connect <host> [project]` is the Connect dialog without
//!   the dialog: find the host's sessions, connect, remember it in the recent
//!   remote sessions, and hand this terminal to the host's Shelbi, reconnecting
//!   when `ssh` drops.

use std::io::IsTerminal;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use shelbi_app::launch::remote::{RemoteConnector, RemoteError, RemoteSession};
use shelbi_app::launch::{connect_remote, LaunchError, RecentRemotes, RemoteListing, SshRemote};

use super::hub_version::ASSUME_YES_ENV;

#[derive(Debug, Subcommand)]
pub enum RemoteCmd {
    /// List this machine's projects for a remote client: the user, the
    /// version, whether the daemon is running, and each project's name and
    /// folder. Starts nothing.
    List {
        /// Print the listing as JSON (what a remote client reads).
        #[arg(long)]
        json: bool,
    },
    /// Open a project of Shelbi on another machine in this terminal, over
    /// your own `ssh` (keys, agent and `~/.ssh/config`; never a password).
    /// Reconnects when the connection drops; Esc stops. If Shelbi isn't
    /// running there, asks before starting it (`--yes` starts it).
    Connect {
        /// An SSH alias or `user@hostname`.
        host: String,
        /// The project on that machine. Optional when it has just one.
        project: Option<String>,
    },
}

pub fn run(cmd: RemoteCmd) -> Result<()> {
    match cmd {
        RemoteCmd::List { json } => {
            let listing = RemoteListing::local().map_err(|e| anyhow!(e))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&listing)?);
            } else {
                print!("{}", human(&listing));
            }
            Ok(())
        }
        RemoteCmd::Connect { host, project } => connect(&host, project.as_deref()),
    }
}

fn connect(host: &str, project: Option<&str>) -> Result<()> {
    let remote = SshRemote::new();
    let (_, sessions) = remote.find_sessions(host)?;
    let session = pick(host, &sessions, project)?.clone();
    let recents = RecentRemotes::live();
    let session = match connect_remote(session.clone(), &remote, &recents) {
        Ok(session) => session,
        Err(LaunchError::Remote(RemoteError::DaemonStopped { host })) => {
            if !start_accepted(&host)? {
                bail!("Shelbi isn't running on {host}. Run again with --yes to start it.");
            }
            eprintln!("Starting Shelbi on {host}…");
            remote.start_daemon(&host)?;
            connect_remote(session, &remote, &recents)?
        }
        Err(e) => return Err(e.into()),
    };
    let status = shelbi_tui::run_remote_session(&session, &remote);
    eprintln!("{status}");
    Ok(())
}

/// The session `project` names, or the only one.
fn pick<'a>(
    host: &str,
    sessions: &'a [RemoteSession],
    project: Option<&str>,
) -> Result<&'a RemoteSession> {
    match (project, sessions) {
        (_, []) => bail!("Shelbi on {host} has no projects."),
        (Some(name), _) => sessions
            .iter()
            .find(|s| s.project == name)
            .ok_or_else(|| anyhow!("{host} has no Shelbi project named {name}. {}", names(sessions))),
        (None, [only]) => Ok(only),
        (None, _) => bail!("Name a project on {host}. {}", names(sessions)),
    }
}

fn names(sessions: &[RemoteSession]) -> String {
    let names: Vec<&str> = sessions.iter().map(|s| s.project.as_str()).collect();
    format!("Its projects: {}.", names.join(", "))
}

/// Whether to start the far daemon: `--yes`, or a yes at the prompt.
fn start_accepted(host: &str) -> Result<bool> {
    if std::env::var(ASSUME_YES_ENV).as_deref() == Ok("1") {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(false);
    }
    inquire::Confirm::new(&format!("Shelbi isn't running on {host}. Start it?"))
        .with_default(true)
        .prompt()
        .context("confirm prompt `Start it?`")
}

/// The plain-text listing.
fn human(listing: &RemoteListing) -> String {
    let daemon = if listing.daemon_running { "running" } else { "not running" };
    let mut out = format!(
        "Shelbi {} as {} (daemon {daemon})\n",
        listing.version, listing.user
    );
    if listing.projects.is_empty() {
        out.push_str("No projects.\n");
    }
    let width = listing
        .projects
        .iter()
        .map(|p| p.name.chars().count())
        .max()
        .unwrap_or(0);
    for p in &listing.projects {
        out.push_str(&format!("  {:width$}  {}\n", p.name, p.path));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::launch::ssh::{RemoteProject, REMOTE_PROTOCOL};

    fn session(project: &str) -> RemoteSession {
        RemoteSession {
            host: "devbox".into(),
            user: "jlong".into(),
            project: project.into(),
            label: None,
            path: format!("~/{project}"),
        }
    }

    #[test]
    fn a_project_is_named_or_the_only_one() {
        let one = [session("shelbi")];
        assert_eq!(pick("devbox", &one, None).unwrap().project, "shelbi");
        let two = [session("shelbi"), session("web")];
        assert_eq!(pick("devbox", &two, Some("web")).unwrap().project, "web");
        assert_eq!(
            pick("devbox", &two, None).unwrap_err().to_string(),
            "Name a project on devbox. Its projects: shelbi, web."
        );
        assert_eq!(
            pick("devbox", &two, Some("gone")).unwrap_err().to_string(),
            "devbox has no Shelbi project named gone. Its projects: shelbi, web."
        );
        assert_eq!(
            pick("devbox", &[], None).unwrap_err().to_string(),
            "Shelbi on devbox has no projects."
        );
    }

    #[test]
    fn the_plain_listing_names_the_user_daemon_and_projects() {
        let listing = RemoteListing {
            protocol: REMOTE_PROTOCOL,
            version: "0.42.0".into(),
            user: "jlong".into(),
            daemon_running: false,
            projects: vec![
                RemoteProject {
                    name: "shelbi".into(),
                    label: Some("Shelbi".into()),
                    path: "~/Workspaces/shelbi".into(),
                },
                RemoteProject {
                    name: "web".into(),
                    label: None,
                    path: "/srv/web".into(),
                },
            ],
        };
        assert_eq!(
            human(&listing),
            "Shelbi 0.42.0 as jlong (daemon not running)\n  shelbi  ~/Workspaces/shelbi\n  web     /srv/web\n"
        );
    }
}
