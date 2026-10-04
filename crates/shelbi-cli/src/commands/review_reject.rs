//! `shelbi __review-reject-reason --out PATH` — the reject-reason prompt, meant
//! to run inside a `tmux display-popup` (centered, `-B`-borderless so the
//! widget's own frame is the only border).
//!
//! The prompt logic and rendering live in
//! [`shelbi_tui::overlay::review_reject`] so this tmux popup and the
//! single-process TUI overlay (removing-tmux Phase 4d) share one implementation.
//! This module keeps only the process scaffolding: terminal setup, the event
//! loop, and the temp-file result contract — on submit it writes the typed
//! reason (newlines preserved verbatim) to `PATH` and exits 0; on cancel it
//! writes nothing and exits non-zero.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use shelbi_tui::overlay::review_reject::{RejectPrompt, Step};

/// Render + drive the reject-reason popup. On submit, writes the reason
/// (newlines preserved) to `out` and returns `true`; on cancel, writes nothing
/// and returns `false`.
pub fn run(out: String) -> Result<bool> {
    let mut term = setup_terminal()?;
    // Restore the terminal on any early return / panic — the caller reads our
    // process exit code, so a stranded raw-mode popup pane would be worse than
    // one that just closes.
    let _guard = TerminalGuard;
    let mut prompt = RejectPrompt::new();

    let submitted = loop {
        term.draw(|f| prompt.render(f, f.area()))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match prompt.handle_key(k) {
                Step::Submit => break true,
                Step::Cancel => break false,
                Step::Continue => {}
            },
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                let area = term.get_frame().area();
                match prompt.handle_click(area, m.column, m.row) {
                    Step::Submit => break true,
                    Step::Cancel => break false,
                    Step::Continue => {}
                }
            }
            _ => {}
        }
    };

    restore_terminal(&mut term)?;
    if submitted {
        // Write the reason verbatim (internal newlines intact); the launcher
        // trims the outer whitespace on read-back.
        std::fs::write(&out, prompt.reason())
            .with_context(|| format!("writing reject reason to {out}"))?;
    }
    Ok(submitted)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture so the `[ Reject ]` / `[ Cancel ]` buttons are clickable;
    // tmux forwards mouse events to the popup pane when its `mouse` option is on.
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

/// RAII backstop that leaves raw mode, mouse capture, and the alternate screen
/// on drop, so an error/panic path can't strand the popup pane in full-screen
/// raw mode (or leak the mouse-capture escape sequences).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
