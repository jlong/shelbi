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
//! - **Emoji** need a UTF-8 locale and a terminal with a pictograph font. The
//!   Linux virtual console (`TERM=linux`) and a non-UTF-8 locale can't draw
//!   them, so the sidebar footer's buttons fall back to plain words there.
//! - **Kitty graphics** (kitty, Ghostty, WezTerm) draw the Changelog's and the
//!   Documentation's images inline ([`crate::kitty`]). Probed with a graphics
//!   query in the same round-trip as the keyboard one; no `OK` reply means
//!   placeholders. Not probed inside tmux or Screen at all: an image placed
//!   through tmux passthrough lands at the outer terminal's cursor, not the
//!   pane's, and tmux repaints over it, so nested shells keep placeholders.
//!
//! Detection must never block the first frame (`rt-tui-headless-startup-block`).
//! crossterm's own `supports_keyboard_enhancement` waits up to **2 s** for the
//! terminal to answer the `CSI ? u` query, which a headless PTY (CI smoke jobs,
//! `tmux`/`screen` capture harnesses) never does — so the shell drew nothing for
//! seconds. We instead split detection: the env-derived signals (truecolor,
//! nesting) are read instantly at construction ([`Caps::detect_fast`]), and the
//! kitty round-trips (keyboard and graphics) are probed *after* the first
//! frame with a short timeout ([`probe_terminal`]), falling back to the safe
//! `false` defaults when no reply arrives.

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
    /// Emoji (and the pictographs beside them, like `⚙`) draw: a UTF-8
    /// locale on a terminal that isn't the Linux console.
    pub emoji: bool,
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

/// How long [`probe_terminal`] waits for the terminal to answer the
/// `CSI ? u` query before falling back to `kitty: false`. Short enough that a
/// non-answering (headless) terminal never visibly stalls the shell, generous
/// enough that a real terminal's near-instant reply always lands first.
pub const KITTY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

impl Caps {
    /// The instant, non-blocking signals, read from the environment: truecolor
    /// from `COLORTERM`, nesting from `$TMUX` / `$STY`. The kitty round-trip is
    /// *not* probed here (it can block up to 2 s on a terminal that never
    /// answers — see the module docs); it defaults to the safe `false` and is
    /// filled in later by [`probe_terminal`] after the first frame.
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
            emoji: emoji_from_env(
                std::env::var("TERM").ok().as_deref(),
                locale_from_env().as_deref(),
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

/// The locale that governs character handling, POSIX order: `LC_ALL`, then
/// `LC_CTYPE`, then `LANG` (the first one set and non-empty).
fn locale_from_env() -> Option<String> {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
}

/// Pure `TERM` + locale → emoji decision, split out for testing. Safe unless
/// something says otherwise: the Linux console (and other bare VT terminals)
/// has no pictograph font, and a locale that names a charset other than UTF-8
/// can't encode one. No locale at all (common for a GUI-launched terminal on
/// macOS) is taken as UTF-8.
pub(crate) fn emoji_from_env(term: Option<&str>, locale: Option<&str>) -> bool {
    let bare_terminal = term.is_some_and(|t| t == "linux" || t == "dumb" || t.starts_with("vt"));
    let non_utf8 = locale.is_some_and(|l| {
        let l = l.to_ascii_lowercase();
        // `C` / `POSIX` are ASCII-only; a named charset must be UTF-8.
        l == "c" || l == "posix" || (l.contains('.') && !(l.contains("utf-8") || l.contains("utf8")))
    });
    !bare_terminal && !non_utf8
}

/// Pure `COLORTERM` → truecolor decision, split out for testing.
pub(crate) fn truecolor_from_env(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor") | Some("24bit"))
}

/// What the post-first-frame probe found the terminal answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Probe {
    /// The kitty keyboard protocol (a `CSI ? … u` flags reply).
    pub keyboard: bool,
    /// The kitty graphics protocol (an `OK` to the graphics query).
    pub graphics: bool,
}

/// The id the graphics query asks about (any id: the query stores nothing).
const GRAPHICS_QUERY_ID: u32 = 31;

/// Whether to ask the terminal about kitty graphics at all: not inside tmux or
/// Screen (see the module docs), not in Apple's Terminal (it doesn't speak
/// the protocol), and not on a bare console.
pub(crate) fn graphics_wanted(nested: Option<Nesting>, term_program: Option<&str>, term: Option<&str>) -> bool {
    let bare = term.is_some_and(|t| t == "linux" || t == "dumb" || t.starts_with("vt"));
    nested.is_none() && term_program != Some("Apple_Terminal") && !bare
}

/// The probe's query: the kitty graphics query (a 1×1 RGB pixel, `a=q`, so
/// nothing is stored) when `graphics`, then the progressive-flags query
/// `CSI ? u`, then primary device attributes `CSI c`, which every terminal
/// answers and so ends the reply.
pub(crate) fn probe_query(graphics: bool) -> Vec<u8> {
    let mut q = Vec::new();
    if graphics {
        q.extend_from_slice(format!("\x1b_Gi={GRAPHICS_QUERY_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\").as_bytes());
    }
    q.extend_from_slice(b"\x1b[?u\x1b[c");
    q
}

/// Probe what the terminal speaks, waiting at most `timeout` for a reply:
/// the kitty keyboard protocol, and (when `graphics`, see [`graphics_wanted`])
/// kitty graphics. Writes [`probe_query`] to the controlling terminal and reads
/// the reply from stdin ([`parse_probe_reply`]). A terminal that answers
/// nothing within `timeout` (a headless PTY, or an outer tmux/Screen that
/// swallows the query) yields the safe defaults.
///
/// **Precondition:** the terminal is already in raw mode and nothing else is
/// draining stdin — the shell calls this once, after the first frame and before
/// the event loop begins reading input, so the reply isn't line-buffered and
/// isn't stolen by a concurrent reader.
#[cfg(unix)]
pub(crate) fn probe_terminal(timeout: std::time::Duration, graphics: bool) -> Probe {
    use std::io::Write;

    let query = probe_query(graphics);
    // Write the query to the controlling terminal; fall back to stdout if
    // `/dev/tty` can't be opened (e.g. no controlling terminal).
    let wrote_tty = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()
        .and_then(|mut f| f.write_all(&query).and_then(|()| f.flush()).ok())
        .is_some();
    if !wrote_tty {
        let mut out = std::io::stdout();
        if out.write_all(&query).and_then(|()| out.flush()).is_err() {
            return Probe::default();
        }
    }

    read_probe_reply(libc::STDIN_FILENO, timeout)
}

#[cfg(not(unix))]
pub(crate) fn probe_terminal(_timeout: std::time::Duration, _graphics: bool) -> Probe {
    Probe::default()
}

/// Read bytes from `fd` until the terminal's reply ends (its device
/// attributes, see [`parse_probe_reply`]) or `timeout` elapses. Returns what
/// the reply showed by then: on a timeout or a read error, whatever had
/// arrived (nothing, usually).
#[cfg(unix)]
fn read_probe_reply(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> Probe {
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut chunk = [0u8; 128];
    loop {
        let (probe, done) = parse_probe_reply(&buf);
        if done {
            return probe;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return probe;
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
            return probe;
        }
        if r == 0 || pfd.revents & libc::POLLIN == 0 {
            // Timed out, or POLLERR / POLLHUP / POLLNVAL: no more reply coming.
            return probe;
        }
        // SAFETY: `fd` is readable (POLLIN); `chunk` is a valid writable buffer.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast::<libc::c_void>(), chunk.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return probe;
        }
        if n == 0 {
            return probe; // EOF
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
}

/// Read the terminal's reply to [`probe_query`] so far: what it showed, and
/// whether it's complete.
///
/// The replies come in the order asked: a kitty-graphics terminal answers the
/// graphics query with an APC string, `ESC _ G i=31;OK ESC \` (an error text
/// in place of `OK` when it can't); a kitty-keyboard terminal answers the
/// flags query with `CSI ? <flags> u`; every terminal answers the device
/// attributes with `CSI ? … c`, which ends the reply. Sequences are read
/// whole, so a `u` or `c` inside the graphics reply's text decides nothing.
/// Anything else (a key typed meanwhile, another reply) is skipped.
pub(crate) fn parse_probe_reply(buf: &[u8]) -> (Probe, bool) {
    let mut probe = Probe::default();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        match buf.get(i + 1) {
            None => break,
            // APC: up to the string terminator `ESC \`.
            Some(b'_') => {
                let body = &buf[i + 2..];
                let Some(end) = body.windows(2).position(|w| w == b"\x1b\\") else {
                    break; // not all here yet
                };
                let apc = &body[..end];
                if let Some(rest) = apc.strip_prefix(b"G") {
                    if let Some(semi) = rest.iter().position(|&b| b == b';') {
                        probe.graphics |= rest[semi + 1..].starts_with(b"OK");
                    }
                }
                i += 2 + end + 2;
            }
            // CSI: parameter and intermediate bytes, then a final byte.
            Some(b'[') => {
                let body = &buf[i + 2..];
                let Some(fin) = body.iter().position(|&b| (0x40..=0x7e).contains(&b)) else {
                    break; // not all here yet
                };
                let private = body.first() == Some(&b'?');
                match body[fin] {
                    b'u' if private => probe.keyboard = true,
                    b'c' if private => return (probe, true),
                    _ => {}
                }
                i += 2 + fin + 1;
            }
            Some(_) => i += 2,
        }
    }
    (probe, false)
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
        let with = Caps { kitty: true, truecolor: true, nested: Some(Nesting::Tmux), emoji: true };
        assert!(with.keyboard_notice().is_none(), "kitty present → no notice even inside tmux");
        let without = Caps { kitty: false, truecolor: true, nested: None, emoji: true };
        assert_eq!(without.keyboard_notice(), Some(KEYBOARD_NOTICE_GENERIC));
    }

    #[test]
    fn emoji_need_a_utf8_locale_off_the_linux_console() {
        assert!(emoji_from_env(Some("xterm-256color"), Some("en_US.UTF-8")));
        assert!(emoji_from_env(Some("xterm-ghostty"), Some("en_US.utf8")));
        assert!(emoji_from_env(Some("tmux-256color"), None), "no locale reads as UTF-8");
        assert!(emoji_from_env(None, Some("en_US")), "no charset named");
        assert!(!emoji_from_env(Some("linux"), Some("en_US.UTF-8")), "the Linux console");
        assert!(!emoji_from_env(Some("vt100"), None));
        assert!(!emoji_from_env(Some("dumb"), None));
        assert!(!emoji_from_env(Some("xterm"), Some("C")));
        assert!(!emoji_from_env(Some("xterm"), Some("POSIX")));
        assert!(!emoji_from_env(Some("xterm"), Some("en_US.ISO8859-1")));
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
    fn probe_reply_reads_each_answer_until_the_device_attributes() {
        let both = Probe { keyboard: true, graphics: true };
        // Ghostty / kitty: graphics OK, keyboard flags, device attributes.
        assert_eq!(parse_probe_reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?1u\x1b[?62;c"), (both, true));
        // Keyboard only (the graphics query ignored): the flags, then the end.
        let keyboard = Probe { keyboard: true, graphics: false };
        assert_eq!(parse_probe_reply(b"\x1b[?1u\x1b[?62;c"), (keyboard, true));
        // Device attributes only (Terminal.app): neither.
        assert_eq!(parse_probe_reply(b"\x1b[?62;1;6c"), (Probe::default(), true));
        // A graphics error isn't support, and its text (`c`, `u`) decides nothing.
        let err = b"\x1b_Gi=31;ENOTSUPPORTED:cannot use u\x1b\\\x1b[?62;c";
        assert_eq!(parse_probe_reply(err), (Probe::default(), true));
        // Not all here yet: what's arrived, not done.
        assert_eq!(parse_probe_reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;1"), (Probe { keyboard: false, graphics: true }, false));
        assert_eq!(parse_probe_reply(b"\x1b_Gi=31;O"), (Probe::default(), false));
        assert_eq!(parse_probe_reply(b""), (Probe::default(), false));
        // A key typed meanwhile, or a non-private CSI, is skipped.
        assert_eq!(parse_probe_reply(b"j\x1b[2c\x1b[?0u"), (keyboard, false));
    }

    #[test]
    fn graphics_are_asked_about_outside_multiplexers_and_apple_terminal() {
        assert!(graphics_wanted(None, Some("ghostty"), Some("xterm-ghostty")));
        assert!(graphics_wanted(None, None, Some("xterm-kitty")));
        assert!(!graphics_wanted(Some(Nesting::Tmux), Some("tmux"), Some("tmux-256color")));
        assert!(!graphics_wanted(Some(Nesting::Screen), None, Some("screen")));
        assert!(!graphics_wanted(None, Some("Apple_Terminal"), Some("xterm-256color")));
        assert!(!graphics_wanted(None, None, Some("linux")));
        // The query carries the graphics command only when asked.
        assert!(probe_query(true).starts_with(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"));
        assert_eq!(probe_query(false), b"\x1b[?u\x1b[c".to_vec());
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
        let probe = super::read_probe_reply(reader.as_raw_fd(), timeout);
        let elapsed = start.elapsed();
        assert_eq!(probe, Probe::default(), "a terminal that never answers is treated as no-kitty");
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
        writer.write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[?1u\x1b[?62;c").expect("seed reply");
        drop(writer); // EOF after the reply so a mis-parse can't block
        let probe = super::read_probe_reply(reader.as_raw_fd(), Duration::from_secs(1));
        assert_eq!(probe, Probe { keyboard: true, graphics: true });
    }

    #[test]
    fn notice_is_tailored_to_the_outer_multiplexer() {
        // Inside tmux without kitty: the notice names the `extended-keys` fix.
        let tmux = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Tmux), emoji: true };
        let msg = tmux.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("extended-keys"), "tmux notice names the fix: {msg}");
        // Inside Screen: a Screen-specific message, with no tmux-only command.
        let screen = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Screen), emoji: true };
        let msg = screen.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("Screen"), "screen notice mentions Screen: {msg}");
        assert!(!msg.contains("extended-keys"), "screen notice omits the tmux-only fix: {msg}");
    }
}
