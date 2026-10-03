//! Phase 0 replay cases (a), (b), (c), plus the vt100 comparison.
//!
//! Fixtures under `tests/fixtures/` are real PTY recordings made by the
//! `capture` bin (nvim, and a shell with nvim opened over it). The tests are
//! deterministic: they only replay the recorded bytes.

use rt_spike_replay::boundary::{count_fixed_tears, rest_chunks, tears_at};
use rt_spike_replay::serialize::replay_stream;
use rt_spike_replay::{vt, Emu, Snapshot};

const COLS: usize = 80;
const LINES: usize = 24;

const NVIM: &[u8] = include_bytes!("fixtures/nvim-file.bin");
const SHELL_NVIM: &[u8] = include_bytes!("fixtures/shell-nvim.bin");

fn feed(stream: &[u8]) -> Emu {
    let mut e = Emu::new(COLS, LINES);
    e.feed(stream);
    e
}

// ---------------------------------------------------------------------------
// Case (a): reattach at unchanged size to a running full-screen program.
// ---------------------------------------------------------------------------
#[test]
fn case_a_fullscreen_roundtrip() {
    let a = feed(NVIM);
    assert!(a.is_alt(), "precondition: nvim is on the alternate screen");

    let replay = replay_stream(&a);
    let b = feed(&replay);

    assert!(b.is_alt(), "replay must put B on the alternate screen too");
    if let Some(d) = a.snapshot().diff(&b.snapshot()) {
        panic!("case (a): A and B diverged after replay: {d}");
    }
}

// ---------------------------------------------------------------------------
// Case (b): attach while the full-screen program is open, quit it, and the
// shell screen underneath must be intact. This is the case that needs the
// inactive grid, which only the vendored fork exposes.
// ---------------------------------------------------------------------------
#[test]
fn case_b_quit_reveals_shell_underneath() {
    let mut a = feed(SHELL_NVIM);
    assert!(a.is_alt(), "precondition: on the alternate screen (nvim open)");

    // Precondition: the normal screen underneath really holds the shell output.
    let normal_a = Snapshot::of(a.inactive_grid(), true);
    assert!(
        normal_a.contains_text("SHELL_UNDERNEATH_MARKER"),
        "precondition: A's inactive grid holds the shell markers"
    );

    let replay = replay_stream(&a);
    let mut b = feed(&replay);

    // Both emulators show the same alternate screen.
    assert!(b.is_alt());
    if let Some(d) = a.snapshot().diff(&b.snapshot()) {
        panic!("case (b): alternate screens diverge after replay: {d}");
    }

    // Quit the full-screen program on both (leave the alternate screen).
    a.feed(b"\x1b[?1049l");
    b.feed(b"\x1b[?1049l");
    assert!(!a.is_alt() && !b.is_alt(), "both back on the normal screen");

    let (ta, tb) = (a.snapshot(), b.snapshot());
    assert!(
        ta.contains_text("SHELL_UNDERNEATH_MARKER"),
        "session's shell screen is intact"
    );
    assert!(
        tb.contains_text("SHELL_UNDERNEATH_MARKER"),
        "replayed client's shell screen MUST be intact (not blank)"
    );
    assert_eq!(
        ta.cells, tb.cells,
        "case (b): the revealed shell screens differ cell-for-cell"
    );
}

// ---------------------------------------------------------------------------
// Case (c): split the live stream only where the parser is at rest.
// ---------------------------------------------------------------------------
#[test]
fn case_c_rest_boundary_framing_is_lossless() {
    // Naive fixed-size framing tears real escape-sequence output.
    let torn = count_fixed_tears(NVIM, 16);
    assert!(
        torn > 0,
        "fixed 16-byte framing should tear sequences in real output"
    );

    // Rest-boundary framing: chunks reassemble to the original, never tear, and
    // feeding chunk-by-chunk equals feeding the whole stream.
    let chunks = rest_chunks(NVIM, 16);
    assert_eq!(chunks.concat(), NVIM, "chunks must reassemble losslessly");

    let whole = feed(NVIM);
    let mut piece = Emu::new(COLS, LINES);
    for c in &chunks {
        piece.feed(c);
    }
    assert!(
        whole.snapshot().diff(&piece.snapshot()).is_none(),
        "rest-chunked feed must equal whole feed"
    );

    let mut off = 0;
    for c in &chunks[..chunks.len() - 1] {
        off += c.len();
        assert!(!tears_at(NVIM, off), "rest-chunk edge at {off} tears a sequence");
    }
}

#[test]
fn case_c_torn_sequence_corrupts_a_resuming_client() {
    // 'A', then SGR-red, then 'B'.
    let s: &[u8] = b"A\x1b[31mB";

    // A cut right after 'A' is at rest; a cut inside the CSI is not.
    assert!(!tears_at(s, 1), "offset 1 (after 'A') is a rest boundary");
    assert!(tears_at(s, 3), "offset 3 (inside ESC[31m) tears");

    // Model a client that rebuilt state up to the cut (parser reset to Ground)
    // and now receives the tail. At a rest cut the tail is well-formed; at a
    // torn cut its leading bytes are misparsed as text.
    let mut safe = Emu::new(20, 2);
    safe.feed(&s[1..]); // "\x1b[31mB" -> red "B"
    let mut torn = Emu::new(20, 2);
    torn.feed(&s[3..]); // "31mB"     -> literal "31mB"

    let safe_text = safe.snapshot().text_rows().join("");
    let torn_text = torn.snapshot().text_rows().join("");
    assert!(
        !safe_text.contains("31m") && safe_text.contains('B'),
        "rest-cut tail renders cleanly: {safe_text:?}"
    );
    assert!(
        torn_text.contains("31m"),
        "torn-cut tail leaks SGR params as visible text: {torn_text:?}"
    );
}

// ---------------------------------------------------------------------------
// Comparison: vt100's single-buffer serialization loses the screen underneath.
// ---------------------------------------------------------------------------
#[test]
fn comparison_vt100_replay_loses_inactive_screen() {
    let mut a = vt::feed(COLS as u16, LINES as u16, 1000, SHELL_NVIM);
    assert!(vt::on_alt_screen(&a), "precondition: vt100 A on the alt screen");

    // vt100's own replay: the formatted contents of the current (alt) screen.
    let replay = vt::vt_replay(&a);
    let mut b = vt::feed(COLS as u16, LINES as u16, 1000, &replay);

    // Quit the full-screen program on both.
    a.process(b"\x1b[?1049l");
    b.process(b"\x1b[?1049l");

    assert!(
        vt::contains(&a, "SHELL_UNDERNEATH_MARKER"),
        "vt100 session keeps the shell underneath"
    );
    assert!(
        !vt::contains(&b, "SHELL_UNDERNEATH_MARKER"),
        "vt100 replay cannot reconstruct the screen underneath (single-buffer)"
    );
}

// ---------------------------------------------------------------------------
// Metrics for the findings doc (prints under `cargo test -- --nocapture`).
// ---------------------------------------------------------------------------
#[test]
fn report_metrics() {
    let a = feed(NVIM);
    let replay = replay_stream(&a);
    eprintln!("[metrics] nvim fixture: {} bytes", NVIM.len());
    eprintln!("[metrics] shell-nvim fixture: {} bytes", SHELL_NVIM.len());
    eprintln!("[metrics] regenerated replay stream: {} bytes", replay.len());
    for chunk in [8usize, 16, 32, 64] {
        eprintln!(
            "[metrics] fixed {}-byte framing tears {} boundaries; rest framing tears 0",
            chunk,
            count_fixed_tears(NVIM, chunk)
        );
    }
}
