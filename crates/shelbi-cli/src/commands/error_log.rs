//! `shelbi __error-log PROJECT` — the persistent error-log viewer, meant to run
//! inside a `tmux display-popup` launched from the sidebar's unread-errors
//! button (or the palette's "Open error log" entry, which runs it inline in the
//! palette's own popup pane).
//!
//! The viewer logic and rendering live in [`shelbi_tui::overlay::error_log`] so
//! this tmux popup and the single-process TUI overlay (removing-tmux Phase 4d)
//! share one implementation. This module keeps only the process scaffolding:
//! the terminal setup, the event loop, and the mark-read-on-open contract.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use shelbi_tui::overlay::error_log::{render, ErrorLogOutcome, Viewer};

/// Render + drive the error-log popup. Marks the log read on open, then loops
/// until the user closes it (`Esc`/`q`) or clears it (`c`). Best-effort: a
/// read/mark failure degrades to an empty view rather than aborting.
pub fn run(project: String) -> Result<()> {
    // Snapshot the unread state BEFORE marking read, so the `●` markers reflect
    // what was new when the log was opened; then mark everything read so the
    // sidebar button clears once this popup closes.
    let rows = shelbi_state::read_errors_with_unread(&project).unwrap_or_default();
    let _ = shelbi_state::mark_errors_read(&project);

    let mut term = setup_terminal()?;
    let _guard = TerminalGuard;
    let mut viewer = Viewer::new(rows);

    loop {
        term.draw(|f| render(f, f.area(), &mut viewer))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match viewer.handle_key(k) {
                ErrorLogOutcome::Continue => {}
                ErrorLogOutcome::Close => break,
                ErrorLogOutcome::Clear => {
                    // Clear resets both the log and the read marker; reflect it
                    // in the live view so the popup shows the empty state at once.
                    let _ = shelbi_state::clear_errors(&project);
                    viewer.clear();
                }
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => viewer.scroll_up(1),
                MouseEventKind::ScrollDown => viewer.scroll_down(1),
                MouseEventKind::Down(MouseButton::Left) => {}
                _ => {}
            },
            _ => {}
        }
    }

    restore_terminal(&mut term)?;
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// RAII backstop that restores the terminal on any early return / panic, so a
/// bail-out can't strand the popup pane in raw mode / the alt-screen.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
