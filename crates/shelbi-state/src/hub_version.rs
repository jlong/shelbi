//! Version handshake on the hub socket.
//!
//! Root cause of the shaft-project outage: Homebrew upgrades swap the
//! `shelbi` binary but never restart the long-lived hub daemon, so an
//! old daemon keeps writing state in its old shape while new CLI
//! one-shots read the new layout — surfacing as undiagnosable, path-less
//! `io: No such file or directory` failures deep in the transition path.
//!
//! The handshake closes that gap: **the daemon speaks first** on every
//! accepted hub-socket connection, writing one newline-terminated JSON
//! hello frame before it reads anything:
//!
//! ```text
//! {"hello":"shelbi-daemon","version":"0.4.0","protocol":1}
//! ```
//!
//! Clients compare `version` against their own `CARGO_PKG_VERSION`
//! (exact match — daemon and CLI ship from the same workspace version)
//! and `protocol` against [`HUB_PROTOCOL_VERSION`]. A daemon that sends
//! *no* hello (the pre-handshake generations — exactly the 0.1 case) is
//! detected by read-timeout and treated as a mismatch, not an error
//! loop. Shell clients (`nc` one-liners) are unaffected: they see the
//! hello line followed by the usual `ok` ack and keep grepping for `ok`.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hub_socket_path;

/// Socket protocol number. Bump on any incompatible change to the hub
/// socket's frame format so a newer/older peer degrades to a clean
/// version-mismatch error instead of misparsing frames.
pub const HUB_PROTOCOL_VERSION: u32 = 1;

/// The `hello` discriminator value the daemon stamps on its hello frame.
/// Parsing requires it verbatim, so an unrelated JSON line (or a future
/// different speaker on the socket) can't be mistaken for a daemon hello.
const HELLO_DISCRIMINATOR: &str = "shelbi-daemon";

/// How long a probing client waits for the daemon's hello before
/// concluding it's a pre-handshake daemon. A live post-handshake daemon
/// writes the hello immediately on accept, so a healthy round-trip is
/// sub-millisecond; the full second is only ever paid on the (already
/// broken) old-daemon path.
const HELLO_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Upper bound on the hello line a client will read. A real hello is
/// ~60 bytes; anything past this is not a hello and the read stops
/// rather than buffering unbounded garbage from a confused peer.
const MAX_HELLO_LINE_BYTES: u64 = 4096;

/// The hello frame the daemon writes as the first line of every accepted
/// hub-socket connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHello {
    /// Always [`HELLO_DISCRIMINATOR`] — lets parsers reject non-hello
    /// JSON lines outright.
    pub hello: String,
    /// The daemon binary's semver (`CARGO_PKG_VERSION` at build time).
    pub version: String,
    /// The daemon's [`HUB_PROTOCOL_VERSION`].
    pub protocol: u32,
}

impl DaemonHello {
    pub fn new(version: &str) -> Self {
        Self {
            hello: HELLO_DISCRIMINATOR.to_string(),
            version: version.to_string(),
            protocol: HUB_PROTOCOL_VERSION,
        }
    }

    /// Serialize to the newline-terminated wire line the daemon writes.
    pub fn to_line(&self) -> String {
        // A struct of two strings and a u32 can't fail to serialize;
        // fall back to an empty line rather than panicking in the
        // daemon's accept path if that ever changes.
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }

    /// Parse one line as a hello frame. `None` for anything that isn't
    /// one — malformed JSON, a different discriminator, a missing field.
    pub fn parse(line: &str) -> Option<Self> {
        let hello: DaemonHello = serde_json::from_str(line.trim()).ok()?;
        (hello.hello == HELLO_DISCRIMINATOR).then_some(hello)
    }
}

/// Outcome of probing the hub socket for the daemon's hello frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProbe {
    /// Connecting to the hub socket failed — no daemon is listening.
    NotRunning,
    /// A daemon accepted the connection but sent no parseable hello
    /// within the timeout: a pre-handshake daemon (0.3.x or older).
    NoHello,
    /// A post-handshake daemon answered with its hello frame.
    Hello(DaemonHello),
}

/// Connect to the hub socket and read the daemon's hello frame.
///
/// The probe writes nothing: a post-handshake daemon speaks first, so
/// the hello arrives immediately; a pre-handshake daemon blocks waiting
/// for *our* line, the read times out, and we report [`DaemonProbe::NoHello`].
/// Dropping the connection afterwards is clean on both — the daemon's
/// read loop sees EOF and closes the handler.
pub fn probe_daemon_hello() -> DaemonProbe {
    let Ok(sock) = hub_socket_path() else {
        return DaemonProbe::NotRunning;
    };
    let Ok(stream) = UnixStream::connect(&sock) else {
        return DaemonProbe::NotRunning;
    };
    let _ = stream.set_read_timeout(Some(HELLO_READ_TIMEOUT));
    match read_hello_line(&stream) {
        Some(hello) => DaemonProbe::Hello(hello),
        None => DaemonProbe::NoHello,
    }
}

/// Read and parse one bounded line as a hello frame. `None` on timeout,
/// EOF, over-length, or a line that isn't a hello.
fn read_hello_line(stream: &UnixStream) -> Option<DaemonHello> {
    let reader = BufReader::new(stream);
    let mut buf = Vec::with_capacity(128);
    let n = reader
        .take(MAX_HELLO_LINE_BYTES)
        .read_until(b'\n', &mut buf)
        .ok()?;
    if n == 0 {
        return None;
    }
    DaemonHello::parse(std::str::from_utf8(&buf).ok()?)
}

/// Read the daemon's per-line [`DAEMON_ACK`](crate::DAEMON_ACK) off
/// `stream`, tolerating (and discarding) the leading hello frame a
/// post-handshake daemon writes on connect. Pre-handshake daemons send
/// the ack as the first line, so both generations succeed here.
///
/// Reads at most two bounded lines; anything else is an error so a
/// caller's file fallback fires rather than mistaking garbage for
/// delivery.
pub fn read_daemon_ack<R: Read>(stream: R) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    for _ in 0..2 {
        let mut buf = Vec::with_capacity(64);
        let n = (&mut reader)
            .take(MAX_HELLO_LINE_BYTES)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF before an ack
        }
        if buf == crate::DAEMON_ACK {
            return Ok(());
        }
        if DaemonHello::parse(std::str::from_utf8(&buf).unwrap_or("")).is_some() {
            continue; // hello frame — the ack is the next line
        }
        break; // neither hello nor ack — don't keep reading
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "no daemon ack",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::Shutdown;

    #[test]
    fn hello_round_trips_through_the_wire_line() {
        let hello = DaemonHello::new("0.4.0");
        let line = hello.to_line();
        assert!(line.ends_with('\n'), "wire line must be newline-terminated");
        let parsed = DaemonHello::parse(&line).expect("round-trip parse");
        assert_eq!(parsed, hello);
        assert_eq!(parsed.protocol, HUB_PROTOCOL_VERSION);
    }

    #[test]
    fn parse_rejects_non_hello_lines() {
        for line in [
            "not json",
            "{}",
            r#"{"hello":"someone-else","version":"0.4.0","protocol":1}"#,
            r#"{"verb":"event","line":"x=1"}"#,
            "ok",
            "",
        ] {
            assert!(DaemonHello::parse(line).is_none(), "accepted: {line:?}");
        }
    }

    #[test]
    fn read_daemon_ack_accepts_bare_ack_from_pre_handshake_daemon() {
        read_daemon_ack(&b"ok\n"[..]).expect("bare ack");
    }

    #[test]
    fn read_daemon_ack_skips_leading_hello() {
        let wire = format!("{}ok\n", DaemonHello::new("0.4.0").to_line());
        read_daemon_ack(wire.as_bytes()).expect("hello then ack");
    }

    #[test]
    fn read_daemon_ack_errors_on_hello_without_ack() {
        let wire = DaemonHello::new("0.4.0").to_line();
        assert!(read_daemon_ack(wire.as_bytes()).is_err());
    }

    #[test]
    fn read_daemon_ack_errors_on_garbage() {
        assert!(read_daemon_ack(&b"nope\n"[..]).is_err());
        assert!(read_daemon_ack(&b""[..]).is_err());
    }

    #[test]
    fn probe_reports_not_running_without_a_daemon() {
        let _lock = crate::test_lock::LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "shelbi-hello-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var("SHELBI_HUB_SOCK").ok();
        std::env::set_var("SHELBI_HUB_SOCK", dir.join("hub.sock"));
        assert_eq!(probe_daemon_hello(), DaemonProbe::NotRunning);
        match prev {
            Some(v) => std::env::set_var("SHELBI_HUB_SOCK", v),
            None => std::env::remove_var("SHELBI_HUB_SOCK"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_classifies_hello_and_silent_listeners() {
        use std::os::unix::net::UnixListener;
        let _lock = crate::test_lock::LOCK.lock().unwrap();
        // macOS caps Unix-socket paths at ~104 bytes; keep it short.
        let sock = std::path::PathBuf::from(format!("/tmp/shb-hello-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let prev = std::env::var("SHELBI_HUB_SOCK").ok();
        std::env::set_var("SHELBI_HUB_SOCK", &sock);

        // A post-handshake daemon: writes the hello immediately.
        let server = std::thread::spawn(move || {
            // First connection: speak the hello. Second: stay silent
            // like a pre-handshake daemon waiting for the client's line.
            let (mut s1, _) = listener.accept().unwrap();
            s1.write_all(DaemonHello::new("9.9.9").to_line().as_bytes())
                .unwrap();
            let _ = s1.shutdown(Shutdown::Both);
            let (s2, _) = listener.accept().unwrap();
            // Hold s2 open, sending nothing, until the client times out.
            std::thread::sleep(Duration::from_millis(1500));
            drop(s2);
        });

        match probe_daemon_hello() {
            DaemonProbe::Hello(h) => {
                assert_eq!(h.version, "9.9.9");
                assert_eq!(h.protocol, HUB_PROTOCOL_VERSION);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        assert_eq!(
            probe_daemon_hello(),
            DaemonProbe::NoHello,
            "a silent listener is a pre-handshake daemon"
        );

        server.join().unwrap();
        match prev {
            Some(v) => std::env::set_var("SHELBI_HUB_SOCK", v),
            None => std::env::remove_var("SHELBI_HUB_SOCK"),
        }
        let _ = std::fs::remove_file(&sock);
    }
}
