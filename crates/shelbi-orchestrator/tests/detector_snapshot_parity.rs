//! The `ready.rs` / `submit.rs` screen detectors read a session snapshot the
//! same way they read a `tmux capture-pane -p -J`.
//!
//! The orchestrator's ~80 detector and baseline tests feed the detectors literal
//! screen fixtures. When the session-process backend serves `snapshot` out of
//! the headless emulator, those fixtures travel a new path: child bytes → the
//! emulator → `snapshot`. This test runs each detector fixture through that path
//! — feeding the fixture's bytes into a real [`Emulator`](shelbi_session::emulator::Emulator)
//! and rendering a snapshot — and asserts every detector reads the rendered
//! snapshot **identically** to the raw fixture string. If the emulator's render
//! ever drifts from the shape the detectors expect, one of these fails.
//!
//! No tmux here (that parity is covered in `shelbi-session`'s `capture_parity`);
//! this pins the detectors to the emulator render directly, so it runs
//! everywhere.

use shelbi_orchestrator::{ready, submit};
use shelbi_session::emulator::Emulator;

/// Render `fixture` through the session emulator and return its snapshot.
///
/// The fixture is the screen text the detector tests use; the emulator is fed
/// the same text with `\r\n` line endings (a terminal needs the carriage return
/// to return to column 0) at a width wide enough that no fixture line wraps, so
/// the snapshot carries the same logical lines the raw fixture does.
fn render(fixture: &str) -> String {
    let cols = 140;
    let rows = 40;
    let mut emu = Emulator::new(cols, rows);
    let bytes = fixture.replace('\n', "\r\n");
    emu.feed(bytes.as_bytes());
    emu.visible_text()
}

/// Assert `detector` returns the same verdict on the emulator snapshot as on the
/// raw fixture, and (as a sanity anchor) that the verdict is `expected`.
fn assert_detector<T: std::fmt::Debug + PartialEq>(
    name: &str,
    fixture: &str,
    detector: impl Fn(&str) -> T,
    expected: T,
) {
    let snap = render(fixture);
    let on_raw = detector(fixture);
    let on_snap = detector(&snap);
    assert_eq!(
        on_raw, expected,
        "detector `{name}` verdict on the RAW fixture changed unexpectedly"
    );
    assert_eq!(
        on_snap, expected,
        "detector `{name}` read the emulator snapshot differently from the raw fixture\n\
         raw => {on_raw:?}, snapshot => {on_snap:?}\n--- snapshot ---\n{snap}"
    );
}

#[test]
fn input_box_and_ready_footer_survive_the_emulator() {
    let fixture = "\
╭────────────────────────────────────────────────────────────╮
│ > implement the snapshot parser                              │
╰────────────────────────────────────────────────────────────╯
  ? for shortcuts";
    assert_detector("is_input_ready", fixture, ready::is_input_ready, true);
    assert_detector(
        "is_claude_working",
        fixture,
        ready::is_claude_working,
        false,
    );
    assert_detector(
        "input_holds_unsubmitted_prompt",
        fixture,
        |s| submit::input_holds_unsubmitted_prompt(s, "implement the snapshot parser"),
        true,
    );
}

#[test]
fn live_spinner_row_survives_the_emulator() {
    // `has_live_spinner` scans upward from the input box to the spinner row, so
    // the fixture carries the spinner *above* a drawn input box.
    let fixture = "\
✻ Crunching… (1m 2s · ↓ 39.0k tokens)
╭────────────────────────────────────────────────────────────╮
│ >                                                            │
╰────────────────────────────────────────────────────────────╯
  esc to interrupt";
    assert_detector("has_live_spinner", fixture, ready::has_live_spinner, true);
    assert_detector("is_claude_working", fixture, ready::is_claude_working, true);
    assert_detector("is_input_ready", fixture, ready::is_input_ready, false);
}

#[test]
fn trust_dialog_survives_the_emulator() {
    let fixture = "\
Do you trust the files in this folder?

❯ 1. Yes, proceed
  2. No, exit";
    assert_detector("is_trust_dialog", fixture, ready::is_trust_dialog, true);
    assert_detector(
        "is_unknown_selection_dialog",
        fixture,
        ready::is_unknown_selection_dialog,
        true,
    );
}

#[test]
fn resume_summary_dialog_survives_the_emulator() {
    let fixture = "\
This session is 2h 49m old and 239.8k tokens.
Resuming the full session will consume a substantial portion of your usage limits.
❯ 1. Resume from summary (recommended)
  2. Resume full session as-is";
    assert_detector(
        "is_resume_summary_dialog",
        fixture,
        ready::is_resume_summary_dialog,
        true,
    );
}

#[test]
fn usage_limit_banner_survives_the_emulator() {
    let fixture = "\
⏱ You've hit your usage limit · resets 3pm (America/New_York)

 ❯ 1. Stop and wait for limit to reset
   2. Upgrade your plan";
    // The reset hint must scrape out of the rendered snapshot exactly as it does
    // from the raw banner.
    assert_detector(
        "detect_usage_limit.reset",
        fixture,
        |s| ready::detect_usage_limit(s).and_then(|u| u.reset),
        Some("3pm (America/New_York)".to_string()),
    );
}

#[test]
fn agent_exited_chrome_survives_the_emulator() {
    let fixture = "\
Resume this session with:
claude --resume 56d67514-c708-4810-b420-143a1b42d9d0
[agent exited — press enter to close]";
    assert_detector(
        "detect_agent_exited",
        fixture,
        ready::detect_agent_exited,
        true,
    );
}

#[test]
fn wide_characters_do_not_shift_the_ready_footer() {
    // A pane whose conversation carries double-width glyphs still reads as ready
    // once its input-box footer is drawn — the wide-char columns must not smear
    // the footer text the detector keys on.
    let fixture = "\
你好 — reviewing the diff おはよう
  ⏵⏵ accept edits on (shift+tab to cycle)";
    assert_detector("is_input_ready", fixture, ready::is_input_ready, true);
}
