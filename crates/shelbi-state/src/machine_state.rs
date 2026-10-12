//! Per-machine binary resolution recorded in hub state.
//!
//! `shelbi machine setup|status` resolves which `shelbi` binary a remote
//! machine runs (a compatible one already on the PATH, otherwise the copy
//! Shelbi installs to `~/.shelbi/bin/shelbi`) and remembers the answer here so
//! `status` can show the last-resolved path and version even while a live
//! reachability probe is in flight. Reachability itself is **never** persisted
//! — it is probed live each time and is its own display state (an unreachable
//! host is not a host with a missing binary).
//!
//! The on-disk shape mirrors [`crate::ssh_control`]'s `forward-modes.json`: a
//! flat `{ "<machine>": MachineRecord }` map at
//! `$SHELBI_HOME/machines.json`, small enough to read-modify-write atomically
//! on each update so a concurrent update for a *different* machine is never
//! lost.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shelbi_home;
use shelbi_core::Result;

/// Which binary a machine's resolved `shelbi` is: the one already on the
/// user's PATH, or the copy Shelbi manages under `~/.shelbi/bin`. Stored as a
/// stable snake-case string rather than an enum so the state crate carries no
/// resolution policy — the orchestrator owns the meaning.
pub const SOURCE_PATH: &str = "path";
/// The Shelbi-managed copy at `~/.shelbi/bin/shelbi`.
pub const SOURCE_SHELBI_BIN: &str = "shelbi-bin";

/// Last-resolved `shelbi` binary for a single machine. This is the durable
/// half of what `shelbi machine status` shows; the live reachability check is
/// layered on top at display time and never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRecord {
    /// Absolute path to the resolved binary on the remote (e.g.
    /// `/usr/local/bin/shelbi` or `/home/u/.shelbi/bin/shelbi`).
    pub path: String,
    /// The binary's reported semver (`shelbi --version`), e.g. `0.9.0`.
    pub version: String,
    /// Whether that version was judged compatible with the hub at resolve
    /// time (see the orchestrator's `machine` module for the policy).
    pub compatible: bool,
    /// [`SOURCE_PATH`] or [`SOURCE_SHELBI_BIN`].
    pub source: String,
    /// When this resolution was last refreshed.
    pub checked_at: DateTime<Utc>,
}

/// State file recording each machine's resolved binary:
/// `$SHELBI_HOME/machines.json`.
pub fn machine_state_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("machines.json"))
}

/// Read the whole machine-record map. Best-effort: a missing or unparseable
/// file yields an empty map so a torn write never wedges the setup path — the
/// worst case is re-resolving on the next `status`/`setup`.
pub fn load_machine_records() -> HashMap<String, MachineRecord> {
    let path = match machine_state_path() {
        Ok(p) => p,
        Err(_) => return HashMap::new(),
    };
    match fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// The remembered record for `machine`, if any. `None` means the machine has
/// never been resolved (or was reset).
pub fn load_machine_record(machine: &str) -> Option<MachineRecord> {
    load_machine_records().get(machine).cloned()
}

/// Record (or clear) the resolved binary for `machine`. Read-modify-write
/// against the whole map so a concurrent update for a *different* machine is
/// not lost. Passing `None` forgets the machine.
pub fn save_machine_record(machine: &str, record: Option<MachineRecord>) -> Result<()> {
    let mut map = load_machine_records();
    match record {
        Some(rec) => {
            map.insert(machine.to_string(), rec);
        }
        None => {
            map.remove(machine);
        }
    }
    let body = serde_json::to_vec_pretty(&map)
        .map_err(|e| shelbi_core::Error::Other(format!("serializing machine state: {e}")))?;
    let path = machine_state_path()?;
    crate::atomic_write(&path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-machine-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn rec(path: &str, version: &str, compatible: bool, source: &str) -> MachineRecord {
        MachineRecord {
            path: path.to_string(),
            version: version.to_string(),
            compatible,
            source: source.to_string(),
            checked_at: Utc::now(),
        }
    }

    #[test]
    fn missing_file_is_an_empty_map() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert!(load_machine_records().is_empty());
        assert_eq!(load_machine_record("devbox"), None);
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn save_then_load_round_trips_and_isolates_machines() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        save_machine_record("devbox", Some(rec("/usr/bin/shelbi", "0.9.0", true, SOURCE_PATH)))
            .unwrap();
        // A write for a different machine must not clobber the first.
        save_machine_record(
            "gpu",
            Some(rec("/home/u/.shelbi/bin/shelbi", "0.9.0", true, SOURCE_SHELBI_BIN)),
        )
        .unwrap();

        let devbox = load_machine_record("devbox").unwrap();
        assert_eq!(devbox.path, "/usr/bin/shelbi");
        assert_eq!(devbox.source, SOURCE_PATH);
        assert!(devbox.compatible);
        assert_eq!(load_machine_record("gpu").unwrap().source, SOURCE_SHELBI_BIN);
        assert_eq!(load_machine_records().len(), 2);

        // Clearing one leaves the other intact.
        save_machine_record("devbox", None).unwrap();
        assert_eq!(load_machine_record("devbox"), None);
        assert!(load_machine_record("gpu").is_some());

        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn unparseable_file_degrades_to_empty() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        fs::write(machine_state_path().unwrap(), b"{ not json").unwrap();
        assert!(load_machine_records().is_empty());
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&home);
    }
}
