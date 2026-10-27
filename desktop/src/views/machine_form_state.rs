//! The Add machine and Configure machine forms' state
//! (`dt-machine-settings-forms`, Figma frames 2526:9557 and 2528:9870): the
//! shared draft and its connection check
//! (`shelbi_app::settings::machine_check::ReadinessForm`, which the TUI's
//! form runs too), with Choose… beside this Mac's directory.
//!
//! The draft is the fields' text as typed; Add machine / Save changes hands
//! it to the shared machine service as a [`SaveRequest`] and its refusal
//! comes back to `finish_save`. Test connection, Test again and Install CLI
//! each make a [`ReadinessRequest`] for the draft as it stands, which the
//! readiness service (`shelbi_orchestrator::machine_readiness::service`) runs
//! off the UI thread; `panel` says what the region shows, and only a check
//! that verified everything is "Connection successful" (see the shared
//! module for how results are bound to the draft).
//!
//! Keys: the shared form's (Tab and ⇧Tab move, Return submits from a field or
//! activates the focused button, Space activates the focused button, Esc
//! cancels), with the check's buttons in the Tab order.

use std::path::Path;

use shelbi_core::Machine;

pub use shelbi_app::settings::machine_check::{Action, Check, Field, Focus, ReadinessRequest, SaveRequest, Tone};
/// What tests and `--visual-figma` land on a form.
#[cfg(any(test, feature = "visual-check"))]
pub use shelbi_app::settings::machine_check::{CheckOutcome, ReadinessIntent};

/// An add or configure form's state.
pub type MachineForm = shelbi_app::settings::machine_check::ReadinessForm;

/// Add machine: an empty SSH draft.
pub fn add(project: &str) -> MachineForm {
    MachineForm::add(project).with_browse()
}

/// Configure `machine` (as the page read it), which hosts `workspaces` of the
/// project's workspaces. This Mac's directory is shortened against `home`.
pub fn configure(project: &str, machine: &Machine, workspaces: usize, home: Option<&Path>) -> MachineForm {
    MachineForm::configure(project, machine, workspaces, home).with_browse()
}

/// Sample outcomes (tests and `--visual-figma`): a remote's answers as the
/// readiness check parses them, reported as the live service reports them.
#[cfg(any(test, feature = "visual-check"))]
pub mod samples {
    use shelbi_app::settings::machine_check::Report;
    use shelbi_orchestrator::machine::{parse_version, Platform, Version};
    use shelbi_orchestrator::machine_readiness::{
        self as readiness, CliStatus, DirStatus, GitStatus, Readiness, ReadinessTarget, RemoteReport,
    };

    fn platform() -> Result<Platform, shelbi_orchestrator::machine::PlatformError> {
        shelbi_orchestrator::machine::detect_platform("Linux", "x86_64")
    }

    fn hub() -> Version {
        shelbi_orchestrator::machine::hub_version()
    }

    fn target() -> ReadinessTarget {
        ReadinessTarget {
            host: "gpu.lan".into(),
            work_dir: "/home/me/my-project".into(),
        }
    }

    fn report(r: Readiness) -> Report {
        readiness::report(&r, &target())
    }

    fn ready_readiness() -> RemoteReport {
        RemoteReport {
            platform: platform(),
            cli: CliStatus::Compatible {
                path: "/home/me/.shelbi/bin/shelbi".into(),
                version: hub(),
                source: shelbi_state::machine_state::SOURCE_SHELBI_BIN,
            },
            git: GitStatus::Usable {
                path: "/usr/bin/git".into(),
                version: "git version 2.43.0".into(),
            },
            dir: DirStatus::Usable,
        }
    }

    /// Everything checked out.
    pub fn ready() -> Report {
        report(Readiness::Reached(ready_readiness()))
    }

    /// Reached, but an older CLI is on the PATH.
    pub fn needs_cli() -> Report {
        let mut r = ready_readiness();
        let old = parse_version("0.1.0").unwrap();
        r.cli = CliStatus::Incompatible {
            path: "/usr/local/bin/shelbi".into(),
            version: Some(old),
            compat: Some(shelbi_orchestrator::machine::classify_compat(&old, &hub())),
        };
        report(Readiness::Reached(r))
    }

    /// Reached, with the project directory missing.
    pub fn no_directory() -> Report {
        let mut r = ready_readiness();
        r.dir = DirStatus::Missing;
        report(Readiness::Reached(r))
    }

    /// SSH couldn't connect.
    pub fn unreachable() -> Report {
        report(Readiness::Unreachable {
            detail: "ssh: connect to host gpu.lan port 22: Operation timed out".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live service's reports, through the shared panel, land the
    /// frames' titles (`--visual-figma` renders these).
    #[test]
    fn samples_land_the_frames_titles() {
        let machine = Machine {
            name: "gpu".into(),
            kind: shelbi_core::MachineKind::Ssh,
            work_dir: "/home/me/my-project".into(),
            host: Some("gpu.lan".into()),
            tags: vec![],
            forward: None,
        };
        for (report, title) in [
            (samples::ready(), "Connection successful"),
            (samples::needs_cli(), "Shelbi CLI needed"),
            (samples::no_directory(), "Project directory needs attention"),
            (samples::unreachable(), "Couldn’t connect"),
        ] {
            let mut f = configure("p", &machine, 0, None);
            let Action::Check(request) = f.press(Focus::Test) else { panic!("a check") };
            f.finish_check(request.run, CheckOutcome::Checked(report));
            assert_eq!(f.panel().title, title);
        }
    }
}
