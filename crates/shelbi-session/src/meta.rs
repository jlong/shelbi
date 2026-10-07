//! `meta.json` — the readable description of a session, written at startup.
//!
//! The on-disk directory name is a short hash (see [`crate::layout`]) so the
//! socket path stays under macOS's 104-byte `sun_path` limit; the human-facing
//! identity lives here instead. The orchestrator and the debug CLI
//! (`shelbi session ls`) read this to turn an opaque id back into
//! `<project>/ws/<workspace>` and friends.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The metadata recorded for a session at startup, serialized to `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Short directory id (the hash that keeps the socket path short).
    pub id: String,
    /// Human-readable name, e.g. `<project>/orch`, `<project>/ws/<workspace>`,
    /// `<project>/review/<slot>/<role>`, or `<project>/shell/<workspace>`.
    pub name: String,
    /// The child command line this session runs (program + args).
    pub argv: Vec<String>,
    /// Working directory the child was spawned in.
    pub cwd: PathBuf,
    /// The task id this session is serving, when it serves one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// RFC3339 launch time (UTC).
    pub launched_at: String,
    /// The session protocol's frozen-core version this session speaks
    /// ([`shelbi_proto::PROTOCOL_VERSION`]).
    pub protocol_version: u16,
    /// OS process id of the session process, recorded at startup. A supervisor
    /// needs this to terminate an "alive but not listening" zombie *out of band*:
    /// such a session refuses every socket connect, so the usual over-the-socket
    /// `kill` can't reach it. `0` means "unknown" — a session written before this
    /// field existed (an older build), which the reaper can still drop from
    /// discovery but cannot signal (`rt-re-entering-a-review-fails-to-attach`).
    #[serde(default)]
    pub pid: u32,
}

impl Meta {
    /// Serialize to pretty JSON (written to `meta.json`).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from the JSON stored in `meta.json`.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// The exit record written to `exit.json` when the child exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    /// Exit status code, if the child exited normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// Terminating signal number, if the child was killed by a signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// RFC3339 exit time (UTC).
    pub exited_at: String,
    /// A short human-readable reason, when the session has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExitRecord {
    /// Serialize to pretty JSON (written to `exit.json`).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrips_through_json() {
        let meta = Meta {
            id: "0123456789abcdef".into(),
            name: "demo/ws/alpha".into(),
            argv: vec!["claude".into(), "--continue".into()],
            cwd: PathBuf::from("/tmp/wt/alpha"),
            task: Some("fix-login".into()),
            launched_at: "2026-10-03T12:00:00Z".into(),
            protocol_version: shelbi_proto::PROTOCOL_VERSION,
            pid: 4242,
        };
        let json = meta.to_json().unwrap();
        assert_eq!(Meta::from_json(&json).unwrap(), meta);
    }

    #[test]
    fn meta_without_pid_defaults_to_zero() {
        // An older build's `meta.json` has no `pid` key; it must still parse, with
        // `pid` defaulting to 0 (unknown) so the reaper degrades gracefully.
        let json = r#"{
            "id": "abc",
            "name": "demo/ws/alpha",
            "argv": ["claude"],
            "cwd": "/tmp",
            "launched_at": "2026-10-03T12:00:00Z",
            "protocol_version": 1
        }"#;
        let meta = Meta::from_json(json).unwrap();
        assert_eq!(meta.pid, 0);
    }

    #[test]
    fn meta_without_task_omits_the_key() {
        let meta = Meta {
            id: "abc".into(),
            name: "demo/shell/alpha".into(),
            argv: vec!["/bin/zsh".into()],
            cwd: PathBuf::from("/tmp"),
            task: None,
            launched_at: "2026-10-03T12:00:00Z".into(),
            protocol_version: 1,
            pid: 0,
        };
        let json = meta.to_json().unwrap();
        assert!(!json.contains("task"), "task should be elided when None");
        assert_eq!(Meta::from_json(&json).unwrap(), meta);
    }

    #[test]
    fn exit_record_elides_absent_fields() {
        let rec = ExitRecord {
            code: Some(0),
            signal: None,
            exited_at: "2026-10-03T12:34:56Z".into(),
            reason: None,
        };
        let json = rec.to_json().unwrap();
        assert!(json.contains("\"code\""));
        assert!(!json.contains("signal"));
        assert!(!json.contains("reason"));
    }
}
