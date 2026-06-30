//! `shelbi daemon` — hub-side Unix-socket listener for worker → hub
//! messages. Phase 1 of the Worker → Orchestrator Communication feature
//! (see `Plans/worker-orchestrator-communication.md` §5).
//!
//! The daemon listens on `~/.shelbi/hub.sock` (overridable via
//! `$SHELBI_HUB_SOCK`), reads newline-delimited JSON messages from any
//! number of concurrent clients, and dispatches them by `verb`. Phase 1
//! handles only the `event` verb: the body line is timestamped and
//! appended to `~/.shelbi/events.log`. Unknown verbs and malformed
//! payloads are logged to stderr and the daemon keeps running — a single
//! bad client must not be able to take the listener down.
//!
//! The daemon is stateless across restarts: `events.log` is the durable
//! record, and in-flight bytes live in the kernel's socket buffers. A
//! crash + restart resumes accepting messages.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// Foreground entry point. Binds the socket, installs signal handlers, and
/// runs the accept loop until SIGTERM/SIGINT/SIGHUP. Errors from
/// individual clients are swallowed (logged to stderr) so the daemon
/// keeps serving the rest of the fleet.
pub fn run() -> Result<()> {
    let sock = shelbi_state::hub_socket_path().map_err(|e| anyhow!(e))?;
    prepare_socket(&sock)?;

    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("binding hub socket at {}", sock.display()))?;
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", sock.display()))?;

    eprintln!("shelbi daemon: listening at {}", sock.display());

    let stop = Arc::new(AtomicBool::new(false));
    install_shutdown_listener(stop.clone(), sock.clone())?;

    for incoming in listener.incoming() {
        // Shutdown wakes us via a self-connect; the resulting accept
        // returns Ok with a stream we never read. Check the flag first
        // and bail before spawning a handler that will see EOF anyway.
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match incoming {
            Ok(stream) => {
                thread::spawn(move || handle_client(stream));
            }
            Err(e) => {
                eprintln!("shelbi daemon: accept error: {e}");
            }
        }
    }

    let _ = fs::remove_file(&sock);
    eprintln!("shelbi daemon: stopped");
    Ok(())
}

/// Make sure the socket parent directory exists with `0700` perms and
/// the socket file itself is free for `bind()`. A leftover socket from a
/// previous run is reclaimed only if no one is currently listening on it;
/// a live daemon at the same path is a hard error so two of us never
/// race on the same file descriptor.
fn prepare_socket(sock: &Path) -> Result<()> {
    if let Some(parent) = sock.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating socket parent {}", parent.display()))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 700 {}", parent.display()))?;
        }
    }

    if sock.exists() {
        // A live peer means another daemon owns this socket — refuse to
        // clobber it. A stale file (ECONNREFUSED / ENOENT on connect)
        // gets removed so we can rebind cleanly.
        match UnixStream::connect(sock) {
            Ok(_) => {
                return Err(anyhow!(
                    "another shelbi daemon is already listening at {} \
                     (delete the socket file if you're sure no daemon is running)",
                    sock.display()
                ));
            }
            Err(_) => {
                fs::remove_file(sock).with_context(|| {
                    format!("removing stale socket at {}", sock.display())
                })?;
            }
        }
    }
    Ok(())
}

/// Catch SIGTERM/SIGINT/SIGHUP on a background thread, flip the stop
/// flag, and wake the blocking `accept()` with a single self-connection
/// so the main loop notices and breaks out.
fn install_shutdown_listener(stop: Arc<AtomicBool>, sock: PathBuf) -> Result<()> {
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])
        .context("installing daemon signal handlers")?;
    thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            eprintln!("shelbi daemon: received signal {sig}, shutting down");
            stop.store(true, Ordering::SeqCst);
            // Wake the accept loop. The connection itself is unused —
            // it's just a syscall poke so accept() returns instead of
            // blocking on the next client.
            let _ = UnixStream::connect(&sock);
        }
    });
    Ok(())
}

/// One client → one BufReader → newline-delimited JSON. Each line is
/// dispatched independently so a bad line in the middle of a batch
/// doesn't kill the rest. EOF closes the handler cleanly.
fn handle_client(stream: UnixStream) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("shelbi daemon: client read error: {e}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = dispatch(&line) {
            // Log + continue; one bad message must not take down a
            // multi-message client connection.
            eprintln!("shelbi daemon: rejected message: {e}: {line}");
        }
    }
}

#[derive(Debug, Deserialize)]
struct Message {
    verb: String,
    /// Reserved for future verbs (`request-clarification`, `message-ack`)
    /// that route per-project. Phase 1 only logs it.
    #[serde(default)]
    project: Option<String>,
    /// Body of an `event` message. Required for `event`; ignored for
    /// other verbs (which Phase 1 doesn't support yet).
    #[serde(default)]
    line: Option<String>,
}

fn dispatch(raw: &str) -> Result<()> {
    let msg: Message = serde_json::from_str(raw).context("invalid JSON payload")?;
    match msg.verb.as_str() {
        "event" => handle_event(&msg),
        other => Err(anyhow!("unknown verb `{other}`")),
    }
}

fn handle_event(msg: &Message) -> Result<()> {
    let body = msg
        .line
        .as_deref()
        .ok_or_else(|| anyhow!("event message missing `line` field"))?;
    if body.is_empty() {
        return Err(anyhow!("event message has empty `line` field"));
    }
    // One event = one line. Embedded newlines would tear the body across
    // multiple records (the second of which would be unparseable) so we
    // reject them outright rather than silently mangling the payload.
    if body.contains('\n') || body.contains('\r') {
        return Err(anyhow!("event `line` may not contain newlines"));
    }
    shelbi_state::append_external_event(body).map_err(|e| anyhow!(e))?;
    if let Some(project) = msg.project.as_deref() {
        tracing::debug!(project, "shelbi daemon: appended event");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_rejects_malformed_json() {
        let err = dispatch("not json").unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
    }

    #[test]
    fn dispatch_rejects_unknown_verb() {
        let err =
            dispatch(r#"{"verb":"task-claim","project":"shelbi","line":"x=1"}"#).unwrap_err();
        assert!(err.to_string().contains("unknown verb"), "{err}");
    }

    #[test]
    fn dispatch_event_requires_line_field() {
        let err = dispatch(r#"{"verb":"event","project":"shelbi"}"#).unwrap_err();
        assert!(err.to_string().contains("missing `line`"), "{err}");
    }

    #[test]
    fn dispatch_event_rejects_empty_line() {
        let err = dispatch(r#"{"verb":"event","project":"shelbi","line":""}"#).unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn dispatch_event_rejects_embedded_newline() {
        let err = dispatch(r#"{"verb":"event","project":"shelbi","line":"a\nb"}"#).unwrap_err();
        assert!(err.to_string().contains("newlines"), "{err}");
    }
}
