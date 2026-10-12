//! `shelbi machine <subcommand>` — find or install a compatible `shelbi`
//! binary on the project's declared remote machines (Phase 5 of the
//! remove-tmux plan). The reusable logic lives in
//! [`shelbi_orchestrator::machine`]; this module is the CLI surface: it maps a
//! project's [`shelbi_core::Machine`] to the SSH exec seam, runs the
//! probe/install, persists the per-machine record, and prints the result.
//!
//! Nothing here touches dispatch — the recorded path is consumed later by the
//! `rt-remote-spawn` subtask.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use shelbi_core::{Machine, MachineKind};
use shelbi_orchestrator::machine as m;
use shelbi_state::machine_state::{self, MachineRecord, SOURCE_PATH, SOURCE_SHELBI_BIN};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum MachineCmd {
    /// Find or install a compatible `shelbi` on a remote machine. Probes the
    /// machine's interactive-login PATH for a compatible binary; otherwise
    /// installs the matching release to `~/.shelbi/bin/shelbi`. Idempotent: a
    /// second run with a compatible binary in place changes nothing.
    Setup {
        /// Machine name, as declared under `machines:` in the project YAML.
        name: String,
    },
    /// Show each machine's reachability, resolved `shelbi` path, version, and
    /// whether it is compatible with this hub. With NAME, shows just that one.
    Status {
        /// Machine to inspect. Omit to show every declared machine.
        name: Option<String>,
    },
}

pub fn run(project_opt: Option<String>, cmd: MachineCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    let p = shelbi_state::load_project(&project).map_err(|e| anyhow!(e))?;
    match cmd {
        MachineCmd::Setup { name } => {
            let machine = p
                .machine(&name)
                .ok_or_else(|| anyhow!("no machine named `{name}` in project `{project}`"))?;
            setup(machine)
        }
        MachineCmd::Status { name } => match name {
            Some(n) => {
                let machine = p
                    .machine(&n)
                    .ok_or_else(|| anyhow!("no machine named `{n}` in project `{project}`"))?;
                status(std::slice::from_ref(machine))
            }
            None => status(&p.machines),
        },
    }
}

// ---------------------------------------------------------------------------
// setup

fn setup(machine: &Machine) -> Result<()> {
    if matches!(machine.kind, MachineKind::Local) {
        println!(
            "{}: local machine — it runs the hub's own `shelbi` binary; no remote setup needed.",
            machine.name
        );
        return Ok(());
    }

    let hub = m::hub_version();
    let exec = m::SshExec::new(machine.host(), machine.name.clone());
    let fetch = m::CurlHubFetch;

    match m::setup(&exec, &fetch, &hub) {
        Ok(action) => {
            report_action(&machine.name, &action);
            // Persist the resolved binary so `status` can show it even when a
            // later reachability probe is slow.
            if let Some(rec) = record_from_action(&action) {
                let _ = machine_state::save_machine_record(&machine.name, Some(rec));
            }
            Ok(())
        }
        Err(e) => Err(anyhow!("{}: {}", machine.name, e.message())),
    }
}

fn report_action(name: &str, action: &m::SetupAction) {
    match action {
        m::SetupAction::AlreadyCompatible {
            path,
            version,
            source,
        } => {
            println!(
                "{name}: already set up — compatible shelbi {version} at {path} ({}).",
                source_label(source)
            );
        }
        m::SetupAction::Installed {
            path,
            version,
            alongside,
            via_hub_copy,
        } => {
            let how = if *via_hub_copy {
                " (fetched on the hub and copied over SSH; the machine had no outbound network)"
            } else {
                ""
            };
            println!("{name}: installed shelbi {version} to {path}{how}.");
            if let Some(old) = alongside {
                let old_v = old
                    .version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into());
                println!(
                    "{name}: left the existing shelbi {old_v} at {} untouched (too old for this \
                     hub); the managed copy sits alongside it.",
                    old.path
                );
            }
        }
    }
}

fn record_from_action(action: &m::SetupAction) -> Option<MachineRecord> {
    let (path, version, compatible, source) = match action {
        m::SetupAction::AlreadyCompatible {
            path,
            version,
            source,
        } => (path.clone(), *version, true, *source),
        m::SetupAction::Installed { path, version, .. } => {
            (path.clone(), *version, true, SOURCE_SHELBI_BIN)
        }
    };
    Some(MachineRecord {
        path,
        version: version.to_string(),
        compatible,
        source: source.to_string(),
        checked_at: chrono::Utc::now(),
    })
}

// ---------------------------------------------------------------------------
// status

fn status(machines: &[Machine]) -> Result<()> {
    let hub = m::hub_version();
    println!("hub version: {hub}");
    println!();

    let mut rows: Vec<[String; 5]> = vec![[
        "MACHINE".into(),
        "REACHABLE".into(),
        "PATH".into(),
        "VERSION".into(),
        "COMPATIBLE".into(),
    ]];

    for machine in machines {
        rows.push(status_row(machine, &hub));
    }
    print_table(&rows);
    Ok(())
}

/// Resolve one machine's display row, probing it live. Persists a fresh record
/// on a reachable probe.
fn status_row(machine: &Machine, hub: &m::Version) -> [String; 5] {
    let name = machine.name.clone();

    if matches!(machine.kind, MachineKind::Local) {
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "(this shelbi)".into());
        return [name, "local".into(), path, hub.to_string(), "yes".into()];
    }

    let exec = m::SshExec::new(machine.host(), machine.name.clone());
    match m::probe_machine(&exec, hub) {
        m::MachineProbe::Unreachable { .. } => unreachable_row(machine, "unreachable"),
        m::MachineProbe::AuthDenied { .. } => unreachable_row(machine, "auth-denied"),
        m::MachineProbe::Reachable(r) => {
            let eff = r.effective(hub);
            // Refresh the durable record for a resolved, present binary.
            if let Some(rec) = m::record_for(&eff) {
                let _ = machine_state::save_machine_record(&machine.name, Some(rec));
            }
            let version = eff
                .version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into());
            let compatible = if eff.needs_install {
                "not installed".into()
            } else if eff.compatible {
                "yes".into()
            } else {
                "no".into()
            };
            [name, "yes".into(), eff.path, version, compatible]
        }
    }
}

/// Row for an unreachable/auth-denied machine, falling back to the last
/// recorded resolution for context (never "missing").
fn unreachable_row(machine: &Machine, reach: &str) -> [String; 5] {
    match machine_state::load_machine_record(&machine.name) {
        Some(rec) => [
            machine.name.clone(),
            reach.into(),
            format!("{} (last known)", rec.path),
            rec.version,
            if rec.compatible { "yes".into() } else { "no".into() },
        ],
        None => [
            machine.name.clone(),
            reach.into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ],
    }
}

fn source_label(source: &str) -> &str {
    match source {
        SOURCE_PATH => "on PATH",
        SOURCE_SHELBI_BIN => "shelbi-managed",
        other => other,
    }
}

/// Minimal left-aligned column printer.
fn print_table(rows: &[[String; 5]]) {
    let mut widths = [0usize; 5];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}
