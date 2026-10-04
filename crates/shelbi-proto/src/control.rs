//! The **mutation control protocol** spoken over the daemon's control socket
//! (`control.sock`), distinct from both the session protocol ([`crate::frame`])
//! and the line-based `hub.sock` worker/event protocol.
//!
//! Shelbi makes the daemon the single owner of issue mutations (see the
//! "Removing tmux" plan, "The daemon executes mutations"). A client — `shelbi
//! issue move|start|assign|edit|add`, the review approve/reject commands, or a
//! `shelbi-app` executor — sends one [`ClientMsg::Mutate`] and receives a stream
//! of [`ServerMsg`]: zero or more [`ServerMsg::Line`] (the exact stdout/stderr
//! the in-process path would have printed, kept byte-identical by construction),
//! terminated by [`ServerMsg::Done`] or [`ServerMsg::Failed`]. The daemon
//! additionally pushes [`ServerMsg::Changed`] to every *other* connected client.
//!
//! Framing is the same length-prefix shape the session protocol uses —
//! `[len: u32 BE][json]` — but the payload is a whole serde-tagged message, and
//! there is no frozen-core guarantee: this protocol rides the exact-version
//! match the mutation guard already enforces, so it may change in lockstep with
//! the daemon/CLI version. Encoding and decoding here are **pure** (operate on
//! byte buffers); socket I/O lives in `shelbi-client` and the daemon.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::ProtoError;

/// Version of the control protocol this build speaks, carried in the hello.
/// Unlike the session [`crate::PROTOCOL_VERSION`] this is not a frozen core;
/// a mismatch is surfaced like the daemon-version mutation guard.
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;

/// Upper bound on a single control frame. Generous enough for a large issue body
/// in an `edit`/`add`, far below the session [`crate::MAX_FRAME_LEN`].
pub const MAX_CONTROL_FRAME_LEN: usize = 16 * 1024 * 1024;

const LEN_PREFIX: usize = 4;

/// Which standard stream a [`ServerMsg::Line`] belongs on, so the client can
/// reproduce the in-process path's stdout/stderr split exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// The `(status, revision)` the client was looking at when it issued the
/// mutation. The daemon rejects the command if the issue has moved on, and
/// rechecks this immediately before any irreversible step. There is no issue
/// etag today, so the revision is [`Issue::updated_at`] serialized as RFC3339.
///
/// [`Issue::updated_at`]: https://docs.rs/shelbi-core
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedState {
    /// The issue's status id as the client last read it.
    pub status: String,
    /// The issue's `updated_at` as RFC3339, the stand-in revision.
    pub updated_at: String,
}

/// One in-place body substitution for [`EditSpec`], in command-line order.
/// Literal and regex variants interleave, so they share one ordered list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubOp {
    Literal { from: String, to: String },
    Regex { pattern: String, replacement: String },
}

/// A new issue to create. Everything the client could resolve locally (a body
/// read from `--description` or piped stdin) is already resolved into `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSpec {
    pub title: String,
    pub id: Option<String>,
    pub status: String,
    /// The resolved body, or `None` to default it to the title.
    pub body: Option<String>,
    pub depends_on: Vec<String>,
    pub prefers_machine: Option<String>,
    pub workflow: Option<String>,
    pub branch: Option<String>,
}

/// A non-interactive edit. The `$EDITOR` path and reading piped stdin / a
/// `--body-file` happen on the client; only the resolved result crosses the
/// wire. A `None` field is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditSpec {
    pub title: Option<String>,
    /// Replace the whole body with this text (from `--body`/`--body-file`/stdin
    /// without `--append`).
    pub body_replace: Option<String>,
    /// Append this text to the existing body (the `--append` path).
    pub body_append: Option<String>,
    /// Ordered in-place substitutions.
    pub subs: Vec<SubOp>,
    pub allow_no_match: bool,
    pub workflow: Option<String>,
    pub branch: Option<String>,
    /// `Some(Some(m))` sets the prefers-machine hint, `Some(None)` clears it,
    /// `None` leaves it unchanged.
    pub prefers_machine: Option<Option<String>>,
    pub reason: Option<String>,
}

/// The mutation to perform. Carries the full CLI argument surface so the
/// daemon reproduces the in-process behavior exactly. The issue id (and the
/// project) ride on the enclosing [`MutationRequest`]; [`MutationKind::Add`]
/// carries its own optional id inside [`AddSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationKind {
    Move {
        to: String,
        reason: Option<String>,
        skip_transition_actions: bool,
    },
    Start {
        workspace: Option<String>,
        branch: Option<String>,
        reason: Option<String>,
        force: bool,
    },
    Assign {
        to: String,
        force: bool,
    },
    Unassign,
    Add(Box<AddSpec>),
    Edit(Box<EditSpec>),
    Approve,
    Reject {
        reason: String,
    },
}

impl MutationKind {
    /// A short noun for the `Changed` notification and logging.
    pub fn verb(&self) -> &'static str {
        match self {
            MutationKind::Move { .. } => "move",
            MutationKind::Start { .. } => "start",
            MutationKind::Assign { .. } => "assign",
            MutationKind::Unassign => "unassign",
            MutationKind::Add(_) => "add",
            MutationKind::Edit(_) => "edit",
            MutationKind::Approve => "approve",
            MutationKind::Reject { .. } => "reject",
        }
    }
}

/// One mutation request. `request_id` correlates the stream of replies (the
/// session protocol has no request ids; this one needs them because a single
/// connection may carry several in-flight mutations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRequest {
    pub request_id: u64,
    pub project: String,
    /// The target issue id. Empty for an [`MutationKind::Add`] with no explicit
    /// id (the daemon generates one).
    pub id: String,
    /// The `(status, revision)` the client saw; `None` skips the staleness
    /// gate (e.g. `add`, which has no prior state).
    pub expected: Option<ExpectedState>,
    pub kind: MutationKind,
}

/// Client → daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First frame on every connection. Lets the daemon version-gate.
    Hello {
        protocol: u32,
        client_version: String,
    },
    /// Subscribe this connection to [`ServerMsg::Changed`] (and
    /// [`ServerMsg::Reexec`]) notifications.
    Subscribe,
    /// Run a mutation.
    Mutate(MutationRequest),
    /// Quit a project (removing-tmux Phase 4f): end that project's sessions,
    /// cancel its in-flight daemon jobs through the quit barrier, and mark it
    /// closed. The daemon replies [`ServerMsg::Done`] once the project has
    /// drained (or [`ServerMsg::Failed`]). Other projects are untouched.
    QuitProject { request_id: u64, project: String },
    /// Quit Shelbi entirely (Phase 4f): mark every open project closed, end all
    /// sessions, reply [`ServerMsg::Done`], then stop the daemon. Nothing
    /// restarts it — the projects are closed before the sessions end, so no
    /// session watchdog resurrects the daemon.
    QuitShelbi { request_id: u64 },
    /// Ask the daemon to tell every *subscribed* client to re-exec — the
    /// `shelbi reload` signal. The daemon broadcasts [`ServerMsg::Reexec`] to
    /// its subscribers and replies [`ServerMsg::Done`] to the requester.
    ReloadClients { request_id: u64 },
}

/// Why a mutation did not run (or could not be accepted). `Display` is the
/// operator-facing message; the CLI turns it into the same error text /
/// exit code the in-process path produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationError {
    /// The issue moved on since the client read it (expected-state gate or a
    /// recheck before an irreversible step).
    Stale {
        expected: ExpectedState,
        actual: ExpectedState,
    },
    /// The issue was not found.
    NotFound { id: String },
    /// The daemon and client are different versions; the client must relaunch.
    VersionMismatch { message: String },
    /// Anything the library returned as an error. `message` is verbatim.
    Backend { message: String },
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MutationError::Stale { expected, actual } => write!(
                f,
                "issue changed since you looked at it (you saw `{}` rev {}, it is now `{}` rev {}); \
                 nothing was changed — refresh and retry",
                expected.status, expected.updated_at, actual.status, actual.updated_at
            ),
            MutationError::NotFound { id } => write!(f, "issue `{id}` not found"),
            MutationError::VersionMismatch { message } => write!(f, "{message}"),
            MutationError::Backend { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for MutationError {}

/// A change the daemon announces to other connected clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub project: String,
    pub id: String,
    /// The mutation verb ([`MutationKind::verb`]).
    pub verb: String,
    /// The issue's status after the change (best-effort; empty if unknown, e.g.
    /// a deleted issue).
    pub status: String,
    /// The issue's `updated_at` after the change, RFC3339 (empty if unknown).
    pub updated_at: String,
}

/// Daemon → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Reply to [`ClientMsg::Hello`].
    Hello {
        protocol: u32,
        daemon_version: String,
    },
    /// A line of user-facing output for an in-flight mutation, to be printed
    /// verbatim on `stream`.
    Line {
        request_id: u64,
        stream: Stream,
        text: String,
    },
    /// The mutation finished successfully.
    Done { request_id: u64 },
    /// The mutation failed (or was refused).
    Failed {
        request_id: u64,
        error: MutationError,
    },
    /// A change made by another client (broadcast; no `request_id`).
    Changed(ChangeNote),
    /// The client should re-exec (a TUI) or prompt for relaunch and send no
    /// further commands (the desktop app). Pushed to a subscriber when the
    /// daemon reloads ([`ClientMsg::ReloadClients`]) or when the subscriber's
    /// hello announced a version different from the daemon's (it is out of
    /// date). `reason` is a short operator-facing phrase. Broadcast; no
    /// `request_id`.
    Reexec { reason: String },
}

/// Encode a message to its full wire bytes (`[len: u32 BE][json]`).
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = serde_json::to_vec(msg)?;
    if payload.len() > MAX_CONTROL_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(LEN_PREFIX + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decode a single message from the front of `buf`, returning it and the number
/// of bytes consumed so a caller streaming from a socket can drain frame by
/// frame. [`ProtoError::Incomplete`] means `buf` does not yet hold a whole
/// frame — read more and retry.
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Result<(T, usize), ProtoError> {
    if buf.len() < LEN_PREFIX {
        return Err(ProtoError::Incomplete {
            needed: Some(LEN_PREFIX - buf.len()),
        });
    }
    let payload_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if payload_len > MAX_CONTROL_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(payload_len));
    }
    let total = LEN_PREFIX + payload_len;
    if buf.len() < total {
        return Err(ProtoError::Incomplete {
            needed: Some(total - buf.len()),
        });
    }
    let msg = serde_json::from_slice(&buf[LEN_PREFIX..total])?;
    Ok((msg, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mutate_request() {
        let msg = ClientMsg::Mutate(MutationRequest {
            request_id: 7,
            project: "p".into(),
            id: "t-1".into(),
            expected: Some(ExpectedState {
                status: "review".into(),
                updated_at: "2026-10-03T00:00:00Z".into(),
            }),
            kind: MutationKind::Move {
                to: "done".into(),
                reason: Some("user:tui".into()),
                skip_transition_actions: false,
            },
        });
        let bytes = encode(&msg).unwrap();
        let (back, n): (ClientMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(back, msg);
    }

    #[test]
    fn decode_reports_incomplete_until_the_whole_frame_is_present() {
        let bytes = encode(&ServerMsg::Done { request_id: 1 }).unwrap();
        // Just the length prefix, no payload yet.
        let err = decode::<ServerMsg>(&bytes[..LEN_PREFIX]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { .. }));
        // One byte short of the full frame.
        let err = decode::<ServerMsg>(&bytes[..bytes.len() - 1]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { .. }));
        // The whole frame decodes.
        let (_m, n): (ServerMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn round_trips_the_lifecycle_messages() {
        // The Phase 4f additions must survive a wire round-trip like every other
        // control message, so a quit/reload is never misparsed.
        let msgs = [
            ClientMsg::QuitProject {
                request_id: 3,
                project: "alpha".into(),
            },
            ClientMsg::QuitShelbi { request_id: 4 },
            ClientMsg::ReloadClients { request_id: 5 },
        ];
        for msg in msgs {
            let bytes = encode(&msg).unwrap();
            let (back, n): (ClientMsg, usize) = decode(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(back, msg);
        }
        let reexec = ServerMsg::Reexec {
            reason: "daemon reloaded".into(),
        };
        let bytes = encode(&reexec).unwrap();
        let (back, n): (ServerMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(back, reexec);
    }

    #[test]
    fn two_frames_decode_back_to_back() {
        let mut buf = encode(&ServerMsg::Line {
            request_id: 1,
            stream: Stream::Stdout,
            text: "→ launching".into(),
        })
        .unwrap();
        buf.extend(encode(&ServerMsg::Done { request_id: 1 }).unwrap());
        let (first, n1): (ServerMsg, usize) = decode(&buf).unwrap();
        let (second, n2): (ServerMsg, usize) = decode(&buf[n1..]).unwrap();
        assert!(matches!(first, ServerMsg::Line { .. }));
        assert!(matches!(second, ServerMsg::Done { request_id: 1 }));
        assert_eq!(n1 + n2, buf.len());
    }
}
