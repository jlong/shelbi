//! Nothing lands on the shell's screen from outside its frame
//! (`tui-stderr-corrupts-screen`): a GitHub retry notice shows in the status
//! band and never reaches stderr, and Ctrl+L / a resize / focus regained paint
//! every cell again, wiping text written over the frame.

use super::*;
use crate::shell::stray_output::{install_retry_notices, uninstall_retry_notices};
use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Cell;
use shelbi_state::{RetryKind, RetryNotice};

/// What the read policy's notifier hands over on a transient `gh` failure.
fn transient_notice() -> RetryNotice {
    RetryNotice {
        attempt: 1,
        max_attempts: 2,
        wait: Duration::from_secs(1),
        kind: RetryKind::Transient,
    }
}

const NOTICE_LINE: &str = "transient GitHub error; retrying in 1s (attempt 1/2)";

/// Set in the child run of [`retry_notice_child`]: `sink` installs the
/// shell's notice sink first (as the run loop does), `stderr` doesn't.
const CHILD_ENV: &str = "SHELBI_TUI_STRAY_OUTPUT_CHILD";

/// The half that runs in a child test process, whose stderr the parent reads
/// (the harness's capture would hide a print here). A no-op in a normal run.
#[test]
fn retry_notice_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    if mode != "sink" {
        // The control: no sink, so the production notifier prints.
        shelbi_state::notify_retry_for_test(&transient_notice());
        return;
    }
    let mut st = test_state();
    st.retry_notices = Some(install_retry_notices());
    // From a refresher thread, where a board or activity read retries.
    std::thread::spawn(|| shelbi_state::notify_retry_for_test(&transient_notice()))
        .join()
        .unwrap();
    let now = Instant::now();
    assert!(st.poll_retry_notices(now), "the notice arrives");
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    draw_frame(&mut term, &mut st).unwrap();
    let (_, row) = find_on_screen(&term, NOTICE_LINE).expect("the notice is in the status band");
    assert_eq!(row, 29, "on the bottom row");
    uninstall_retry_notices();
}

/// Run [`retry_notice_child`] in a child process in `mode`; its stderr.
fn run_child(mode: &str) -> String {
    let test = format!(
        "{}::retry_notice_child",
        module_path!().split_once("::").unwrap().1
    );
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "the {mode} child passes:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

#[test]
fn a_github_retry_shows_in_the_status_band_and_writes_nothing_to_the_terminal() {
    // The control proves the child's stderr is where a leak would show.
    assert!(run_child("stderr").contains(NOTICE_LINE), "without the sink it prints");
    let stderr = run_child("sink");
    assert!(!stderr.contains("GitHub"), "nothing reaches stderr:\n{stderr}");
}

#[test]
fn a_retry_notice_shows_over_the_status_and_then_gives_it_back() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut st = test_state();
    st.retry_notices = Some(rx);
    st.status = Some("review loaded; opening…".into());
    tx.send((NOTICE_LINE.to_string(), Duration::from_secs(1))).unwrap();
    let now = Instant::now();
    assert!(st.poll_retry_notices(now));
    assert_eq!(st.status_text().as_deref(), Some(NOTICE_LINE));
    // Shown for at least the notice time, even for a one-second wait.
    assert!(!st.poll_retry_notices(now + Duration::from_secs(NOTICE_SECS - 1)));
    assert!(st.poll_retry_notices(now + Duration::from_secs(NOTICE_SECS)));
    assert_eq!(st.status_text().as_deref(), Some("review loaded; opening…"));
    // A long rate-limit wait stays up for the whole wait.
    tx.send(("GitHub rate limit hit; resuming in 90s (attempt 1/10)".into(), Duration::from_secs(90)))
        .unwrap();
    assert!(st.poll_retry_notices(now));
    assert!(!st.poll_retry_notices(now + Duration::from_secs(89)));
    assert!(st.poll_retry_notices(now + Duration::from_secs(90)));
}

/// Write `text` straight into the terminal at row `y`, behind ratatui's back:
/// what a stray stderr line does to the real screen.
fn scribble(term: &mut Terminal<TestBackend>, y: u16, text: &str) {
    let cells: Vec<(u16, u16, Cell)> = text
        .chars()
        .enumerate()
        .map(|(x, ch)| {
            let mut cell = Cell::default();
            cell.set_char(ch);
            (x as u16, y, cell)
        })
        .collect();
    term.backend_mut()
        .draw(cells.iter().map(|(x, y, c)| (*x, *y, c)))
        .unwrap();
}

#[test]
fn ctrl_l_resize_and_focus_gained_paint_every_cell_again() {
    let mut st = test_state();
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    draw_frame(&mut term, &mut st).unwrap();
    let stray = "s (attempt 1/2) daemon 0.11.0";

    // A plain redraw only repaints cells ratatui thinks changed: the stray
    // text stays.
    scribble(&mut term, 3, stray);
    draw_frame(&mut term, &mut st).unwrap();
    assert!(screen_text(&term).contains(stray), "an ordinary frame leaves it");

    let ctrl_l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    for (what, ev) in [
        ("Ctrl+L", ctrl_l.clone()),
        ("a resize", Event::Resize(120, 30)),
        ("focus regained", Event::FocusGained),
    ] {
        st.client.focus_sidebar();
        st.dirty = false;
        st.handle_event(ev);
        assert!(st.dirty, "{what} asks for a frame");
        draw_frame(&mut term, &mut st).unwrap();
        assert!(!screen_text(&term).contains(stray), "{what} wipes it");
        assert!(!st.full_redraw, "{what}'s repaint is done once");
        scribble(&mut term, 3, stray);
    }

    // Ctrl+L still moves focus to the main pane, and repaints under an
    // overlay too.
    st.client.focus_sidebar();
    st.handle_event(ctrl_l.clone());
    assert!(st.focus_is_main(), "Ctrl+L focuses the main pane");
    st.full_redraw = false;
    st.open_palette();
    st.handle_event(ctrl_l);
    assert!(st.full_redraw, "Ctrl+L repaints over the palette");
}
