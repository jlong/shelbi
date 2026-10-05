//! Terminal capability detection for the single-process shell (plan, "Running
//! inside tmux or Screen").
//!
//! The shell runs as an ordinary full-screen program, so nesting inside tmux or
//! Screen works by construction — there is no `$TMUX` branching. What needs
//! care is detecting which capabilities the outer multiplexer passes through:
//!
//! - The **kitty keyboard protocol** is not forwarded by Screen, or by tmux
//!   without `extended-keys`. Without it Claude Code's Shift+Enter (and other
//!   disambiguated keys) cannot reach the agent. We detect the missing
//!   round-trip and say so once, with the one-line tmux mitigation (the Phase 0
//!   nesting spike confirmed `extended-keys` is the right thing to recommend).
//! - **Truecolor** falls back to 256 colors; the renderer quantizes RGB cells
//!   when this is false.
//!
//! Detection must never block the first frame (`rt-tui-headless-startup-block`).
//! crossterm's own `supports_keyboard_enhancement` waits up to **2 s** for the
//! terminal to answer the `CSI ? u` query, which a headless PTY (CI smoke jobs,
//! `tmux`/`screen` capture harnesses) never does — so the shell drew nothing for
//! seconds. We instead split detection: the env-derived signals (truecolor,
//! nesting) are read instantly at construction ([`Caps::detect_fast`]), and the
//! kitty round-trip is probed *after* the first frame with a short timeout
//! ([`probe_keyboard_enhancement`]), falling back to the safe `kitty: false`
//! default when no reply arrives.

/// An outer multiplexer the shell is running inside, detected from the
/// environment. We don't branch behavior on it (the shell is just a full-screen
/// program either way), but it tailors the keyboard-protocol notice to the
/// mitigation that actually applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nesting {
    /// Inside tmux (`$TMUX` set).
    Tmux,
    /// Inside GNU Screen (`$STY` set).
    Screen,
}

/// What the terminal (through any outer multiplexer) supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The kitty keyboard protocol round-trips, so modified keys such as
    /// Shift+Enter reach the agent.
    pub kitty: bool,
    /// The terminal advertises 24-bit truecolor.
    pub truecolor: bool,
    /// The outer multiplexer we are nested in, if any.
    pub nested: Option<Nesting>,
}

/// Inside tmux without `extended-keys`: name the exact one-line fix.
pub const KEYBOARD_NOTICE_TMUX: &str = "⚠ Modified keys like Shift+Enter may not reach the agent \
    here (the kitty keyboard protocol isn't passed through). Inside tmux: set -s extended-keys on";
/// Inside GNU Screen, which cannot forward the protocol at all.
pub const KEYBOARD_NOTICE_SCREEN: &str = "⚠ Modified keys like Shift+Enter may not reach the agent \
    here (GNU Screen doesn't pass the kitty keyboard protocol through). Run shelbi outside Screen, \
    or attach from a terminal that speaks it";
/// Not nested, but the terminal itself does not speak the protocol.
pub const KEYBOARD_NOTICE_GENERIC: &str = "⚠ Modified keys like Shift+Enter may not reach the \
    agent here (this terminal doesn't support the kitty keyboard protocol)";

/// How long [`probe_keyboard_enhancement`] waits for the terminal to answer the
/// `CSI ? u` query before falling back to `kitty: false`. Short enough that a
/// non-answering (headless) terminal never visibly stalls the shell, generous
/// enough that a real terminal's near-instant reply always lands first.
pub const KITTY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

impl Caps {
    /// The instant, non-blocking signals, read from the environment: truecolor
    /// from `COLORTERM`, nesting from `$TMUX` / `$STY`. The kitty round-trip is
    /// *not* probed here (it can block up to 2 s on a terminal that never
    /// answers — see the module docs); it defaults to the safe `false` and is
    /// filled in later by [`probe_keyboard_enhancement`] after the first frame.
    ///
    /// Truecolor over-reporting inside a multiplexer is harmless: the renderer
    /// still quantizes correctly because the outer terminal does, so the worst
    /// case is a true-color escape the multiplexer itself downsamples.
    pub fn detect_fast() -> Self {
        Caps {
            kitty: false,
            truecolor: truecolor_from_env(std::env::var("COLORTERM").ok().as_deref()),
            nested: detect_nesting(
                std::env::var("TMUX").ok().as_deref(),
                std::env::var("STY").ok().as_deref(),
            ),
        }
    }

    /// The notice to show once at startup, or `None` when the kitty protocol is
    /// available (everything the shell needs). Tailored to how we are nested so
    /// the suggested fix matches the actual outer multiplexer.
    pub fn keyboard_notice(&self) -> Option<&'static str> {
        if self.kitty {
            return None;
        }
        Some(match self.nested {
            Some(Nesting::Tmux) => KEYBOARD_NOTICE_TMUX,
            Some(Nesting::Screen) => KEYBOARD_NOTICE_SCREEN,
            None => KEYBOARD_NOTICE_GENERIC,
        })
    }
}

/// Pure `COLORTERM` → truecolor decision, split out for testing.
pub(crate) fn truecolor_from_env(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor") | Some("24bit"))
}

/// Probe whether the terminal speaks the kitty keyboard protocol, waiting at
/// most `timeout` for a reply. Writes the standard detection query — the
/// progressive-flags query `CSI ? u` followed by the primary-device-attributes
/// query `CSI c` — to the controlling terminal, then reads the reply from
/// stdin. A terminal that supports the protocol answers the flags query (a
/// `CSI ? … u` reply) *before* the device-attributes reply (`CSI ? … c`); one
/// that doesn't answers only the device attributes. A terminal that answers
/// neither within `timeout` (a headless PTY, or an outer tmux/Screen that
/// swallows the query) yields the safe `false`.
///
/// **Precondition:** the terminal is already in raw mode and nothing else is
/// draining stdin — the shell calls this once, after the first frame and before
/// the event loop begins reading input, so the reply isn't line-buffered and
/// isn't stolen by a concurrent reader.
#[cfg(unix)]
pub(crate) fn probe_keyboard_enhancement(timeout: std::time::Duration) -> bool {
    use std::io::Write;

    // ESC [ ? u   progressive keyboard enhancement flags (kitty protocol)
    // ESC [ c     primary device attributes
    const QUERY: &[u8] = b"\x1b[?u\x1b[c";

    // Write the query to the controlling terminal; fall back to stdout if
    // `/dev/tty` can't be opened (e.g. no controlling terminal).
    let wrote_tty = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()
        .and_then(|mut f| f.write_all(QUERY).and_then(|()| f.flush()).ok())
        .is_some();
    if !wrote_tty {
        let mut out = std::io::stdout();
        if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
            return false;
        }
    }

    read_kitty_reply(libc::STDIN_FILENO, timeout)
}

#[cfg(not(unix))]
pub(crate) fn probe_keyboard_enhancement(_timeout: std::time::Duration) -> bool {
    false
}

/// Read bytes from `fd` until the terminal's reply classifies (see
/// [`classify_kitty_reply`]) or `timeout` elapses. Returns the classification,
/// or `false` on timeout / read error.
#[cfg(unix)]
fn read_kitty_reply(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> bool {
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 64];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let ms = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid single-entry poll set; `ms` is non-negative.
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if r == 0 {
            return false; // timed out with no reply
        }
        if pfd.revents & libc::POLLIN == 0 {
            return false; // POLLERR / POLLHUP / POLLNVAL — no reply coming
        }
        // SAFETY: `fd` is readable (POLLIN); `chunk` is a valid writable buffer.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast::<libc::c_void>(), chunk.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if n == 0 {
            return false; // EOF
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if let Some(verdict) = classify_kitty_reply(&buf) {
            return verdict;
        }
    }
}

/// Classify the terminal's reply to the `CSI ? u` + `CSI c` detection query.
///
/// A terminal that speaks the kitty keyboard protocol answers the flags query
/// first, with a `CSI ? … u` sequence; its device-attributes reply (`CSI ? … c`)
/// follows. A terminal that doesn't answers only the device attributes. Since
/// the parameter bytes of both replies are only digits and `;`, the final byte
/// `u` or `c` is unambiguous: whichever appears first decides.
///
/// Returns `Some(true)` once a `u` is seen before any `c` (kitty supported),
/// `Some(false)` once a `c` is seen first (device attributes only), or `None`
/// when neither terminator has arrived yet (read more).
pub(crate) fn classify_kitty_reply(buf: &[u8]) -> Option<bool> {
    let u = buf.iter().position(|&b| b == b'u');
    let c = buf.iter().position(|&b| b == b'c');
    match (u, c) {
        (Some(ui), Some(ci)) => Some(ui < ci),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

/// Pure `$TMUX` / `$STY` → nesting decision, split out for testing. tmux wins
/// if both are set (a Screen inside tmux still reaches a tmux that can be fixed
/// with `extended-keys`). An empty value counts as unset.
pub(crate) fn detect_nesting(tmux: Option<&str>, sty: Option<&str>) -> Option<Nesting> {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Some(Nesting::Tmux)
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Some(Nesting::Screen)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_reads_colorterm() {
        assert!(truecolor_from_env(Some("truecolor")));
        assert!(truecolor_from_env(Some("24bit")));
        assert!(!truecolor_from_env(Some("256")));
        assert!(!truecolor_from_env(None));
    }

    #[test]
    fn keyboard_notice_shows_only_without_kitty() {
        let with = Caps { kitty: true, truecolor: true, nested: Some(Nesting::Tmux) };
        assert!(with.keyboard_notice().is_none(), "kitty present → no notice even inside tmux");
        let without = Caps { kitty: false, truecolor: true, nested: None };
        assert_eq!(without.keyboard_notice(), Some(KEYBOARD_NOTICE_GENERIC));
    }

    #[test]
    fn nesting_reads_tmux_and_sty() {
        assert_eq!(detect_nesting(Some("/tmp/tmux-501/default,1,0"), None), Some(Nesting::Tmux));
        assert_eq!(detect_nesting(None, Some("12345.pts-0.host")), Some(Nesting::Screen));
        // tmux wins when both are present.
        assert_eq!(detect_nesting(Some("x"), Some("y")), Some(Nesting::Tmux));
        // Empty values count as unset.
        assert_eq!(detect_nesting(Some(""), Some("")), None);
        assert_eq!(detect_nesting(None, None), None);
    }

    #[test]
    fn classify_kitty_reply_reads_the_first_terminator() {
        // Flags reply (`u`) before device attributes (`c`) → kitty supported.
        assert_eq!(classify_kitty_reply(b"\x1b[?1u\x1b[?62;c"), Some(true));
        // Device-attributes reply only → not supported.
        assert_eq!(classify_kitty_reply(b"\x1b[?62;1;6c"), Some(false));
        // Neither terminator yet → inconclusive, keep reading.
        assert_eq!(classify_kitty_reply(b"\x1b[?62;1;6"), None);
        assert_eq!(classify_kitty_reply(b""), None);
        // A lone flags reply with no trailing device attributes still decides.
        assert_eq!(classify_kitty_reply(b"\x1b[?0u"), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn kitty_probe_times_out_fast_when_the_terminal_never_answers() {
        use std::os::unix::io::AsRawFd;
        use std::time::{Duration, Instant};

        // A pipe whose write end is held open but never written: the read end is
        // a valid fd that stays readable-never, standing in for a headless PTY
        // that swallows the query. The probe must give up at the timeout, not
        // hang (the bug was crossterm's hard-coded 2 s wait).
        let (reader, _writer) = std::io::pipe().expect("pipe");
        let timeout = Duration::from_millis(80);
        let start = Instant::now();
        let supported = super::read_kitty_reply(reader.as_raw_fd(), timeout);
        let elapsed = start.elapsed();
        assert!(!supported, "a terminal that never answers is treated as no-kitty");
        assert!(
            elapsed < timeout + Duration::from_millis(400),
            "the probe must return near its {timeout:?} budget, not block (took {elapsed:?})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kitty_probe_reads_a_pending_flags_reply() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        use std::time::Duration;

        // A kitty flags reply already waiting on the read end: the probe reads it
        // and reports support without waiting out the timeout.
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"\x1b[?1u\x1b[?62;c").expect("seed reply");
        drop(writer); // EOF after the reply so a mis-parse can't block
        assert!(super::read_kitty_reply(reader.as_raw_fd(), Duration::from_secs(1)));
    }

    #[test]
    fn notice_is_tailored_to_the_outer_multiplexer() {
        // Inside tmux without kitty: the notice names the `extended-keys` fix.
        let tmux = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Tmux) };
        let msg = tmux.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("extended-keys"), "tmux notice names the fix: {msg}");
        // Inside Screen: a Screen-specific message, with no tmux-only command.
        let screen = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Screen) };
        let msg = screen.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("Screen"), "screen notice mentions Screen: {msg}");
        assert!(!msg.contains("extended-keys"), "screen notice omits the tmux-only fix: {msg}");
    }
}
