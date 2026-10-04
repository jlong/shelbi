//! `snapshot` / `capture-pane -p -J` equivalence.
//!
//! The orchestrator's ~80 detector and baseline tests in `ready.rs` and
//! `submit.rs` are anchored on the exact shape `tmux capture-pane -p -J`
//! produces. When the session-process backend serves `snapshot` out of the
//! headless emulator instead of shelling out to tmux, that shape has to match or
//! every one of those detectors risks drifting.
//!
//! This test feeds **identical byte streams** to a real tmux pane and to the
//! session's [`Emulator`](shelbi_session::emulator::Emulator) and asserts the two
//! renderings agree across the detector fixture shapes: prompt / input-box lines,
//! the live spinner row, blocking dialogs, the usage-limit banner, wide
//! characters, and wrapped lines. It is **skipped when tmux is not on PATH** (CI
//! without tmux, or a contributor's box), so it never turns into a hard
//! dependency on tmux being installed.
//!
//! ## Why both sides are normalized before comparing
//!
//! `-J` *preserves* a line's trailing spaces; the emulator trims them (see
//! [`Emulator::visible_text`]). Exact trailing-space parity is unreachable
//! because tmux and this emulator track a line's "used" width differently once a
//! program issues erase-to-end-of-line — and the detectors this feeds are
//! trailing-whitespace insensitive regardless. So both captures are normalized
//! (each line `trim_end`ed, trailing blank lines dropped) before comparison,
//! which still pins down the parts that matter: content, leading/interior
//! whitespace, wide-character handling, line count, and wrapped-line joining.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use shelbi_session::emulator::Emulator;

/// Is a usable `tmux` on PATH? The parity assertions are skipped (not failed)
/// when it is absent.
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A unique tmux session name for this test process, so parallel runs (and the
/// user's own sessions) never collide.
fn unique_session() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    format!(
        "rt-capture-parity-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Normalize a capture for comparison: trim each line's trailing whitespace and
/// drop trailing blank lines. See the module docs for why trailing whitespace is
/// not compared verbatim.
fn normalize(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

/// Feed `bytes` to a detached tmux pane at `cols`x`rows` and return its
/// `capture-pane -p -J` (optionally with `history` lines of scrollback). The
/// pane disables the pty's `\n`→`\r\n` output translation first so tmux sees the
/// exact bytes the emulator is fed, then holds open on `sleep` so the rendered
/// screen can be captured.
fn tmux_capture(bytes: &[u8], cols: u16, rows: u16, history: Option<u32>) -> String {
    let session = unique_session();
    let mut file = tempfile::NamedTempFile::new().expect("tempfile");
    file.write_all(bytes).expect("write fixture");
    file.flush().expect("flush fixture");
    let path = file.path().to_string_lossy().into_owned();

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            &format!("stty -onlcr -ocrnl 2>/dev/null; cat {path:?}; sleep 300"),
        ])
        .status()
        .expect("spawn tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Poll until the capture stabilizes (cat has rendered), bounded so a wedged
    // pane can't hang the suite.
    let capture = || -> String {
        let mut argv = vec!["capture-pane", "-p", "-J"];
        let start;
        if let Some(h) = history {
            start = format!("-{h}");
            argv.extend_from_slice(&["-S", &start]);
        }
        argv.extend_from_slice(&["-t", &session]);
        let out = Command::new("tmux").args(&argv).output().expect("capture-pane");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut last = capture();
    loop {
        std::thread::sleep(Duration::from_millis(60));
        let next = capture();
        if next == last && !next.trim().is_empty() {
            break;
        }
        last = next;
        if Instant::now() >= deadline {
            break;
        }
    }

    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();
    last
}

/// Assert the emulator's `snapshot` agrees with tmux for one fixture.
fn assert_parity(name: &str, bytes: &[u8], cols: u16, rows: u16) {
    let tmux = tmux_capture(bytes, cols, rows, None);
    let mut emu = Emulator::new(cols, rows);
    emu.feed(bytes);
    let snap = emu.visible_text();
    assert_eq!(
        normalize(&snap),
        normalize(&tmux),
        "snapshot != capture-pane -p -J for fixture `{name}`\n--- snapshot ---\n{snap}\n--- tmux ---\n{tmux}"
    );
}

#[test]
fn snapshot_matches_capture_pane_for_detector_fixtures() {
    if !tmux_available() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }

    // Claude Code's live input box: box-drawing rules the input-box detectors
    // key on, plus the permission-mode footer.
    let input_box = "\
╭──────────────────────────────────────────────────────────────────────────╮\r\n\
│ > Try \"edit the parser\"                                                     │\r\n\
╰──────────────────────────────────────────────────────────────────────────╯\r\n\
  ? for shortcuts                                                             \r\n";
    assert_parity("input-box", input_box.as_bytes(), 80, 10);

    // The live spinner row: glyph + gerund + elapsed timer + streaming tokens,
    // with the interrupt footer below it.
    let spinner = "\
✻ Crunching… (1m 2s · ↓ 39.0k tokens)\r\n\
  esc to interrupt\r\n";
    assert_parity("spinner", spinner.as_bytes(), 80, 6);

    // A blocking dialog (trust-this-folder shape): prose plus a numbered menu.
    let dialog = "\
Do you trust the files in this folder?\r\n\
\r\n\
❯ 1. Yes, proceed\r\n\
  2. No, exit\r\n";
    assert_parity("trust-dialog", dialog.as_bytes(), 80, 8);

    // The usage-limit banner.
    let usage = "\
Claude usage limit reached. Your limit will reset at 3pm (America/New_York).\r\n\
\r\n\
  /upgrade to increase your usage limit.\r\n";
    assert_parity("usage-limit", usage.as_bytes(), 80, 6);

    // Wide (double-width) characters interleaved with ASCII.
    let wide = "你好 world ☃ ok\r\nおはよう CJK\r\n";
    assert_parity("wide-chars", wide.as_bytes(), 40, 5);

    // A line longer than the pane width, so it wraps and `-J` must rejoin it.
    let wrapped =
        "the quick brown fox jumps over the lazy dog and keeps going past the edge\r\n";
    assert_parity("wrapped-line", wrapped.as_bytes(), 20, 8);
}

#[test]
fn snapshot_with_history_matches_capture_pane_scrollback() {
    if !tmux_available() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }

    // Twelve lines on a 4-row pane: eight scroll into history. A history-bearing
    // snapshot must line up with `capture-pane -p -J -S -N`.
    let mut bytes = Vec::new();
    for i in 1..=12 {
        bytes.extend_from_slice(format!("line {i:02}\r\n").as_bytes());
    }
    let cols = 20;
    let rows = 4;
    let history = 8;

    let tmux = tmux_capture(&bytes, cols, rows, Some(history));
    let mut emu = Emulator::new(cols, rows);
    emu.feed(&bytes);
    let snap = emu.screen_with_history(history as usize);
    assert_eq!(
        normalize(&snap),
        normalize(&tmux),
        "history snapshot != capture-pane -S -{history}\n--- snapshot ---\n{snap}\n--- tmux ---\n{tmux}"
    );
}
