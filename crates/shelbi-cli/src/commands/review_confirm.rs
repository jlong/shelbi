//! `shelbi __review-confirm --title T --out PATH [--slot NAME --occupant OCC]…`
//! — the "Load for review" dialog, meant to run inside a `tmux display-popup`
//! centered on the terminal (the same surface style as the palette popup).
//!
//! The dialog logic and rendering live in
//! [`shelbi_tui::overlay::review_confirm`] so this tmux popup and the
//! single-process TUI overlay (removing-tmux Phase 4d) share one implementation.
//! This module keeps only the process scaffolding: terminal setup, the event
//! loop, and the temp-file result contract (on Load the chosen slot name is
//! written to `--out` and the process exits 0; on Cancel nothing is written and
//! it exits non-zero).

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

pub use shelbi_tui::overlay::review_confirm::Slot;
use shelbi_tui::overlay::review_confirm::{Dialog, Outcome, Step};

/// Render + drive the dialog, returning whether a slot was loaded. `out`
/// receives the chosen slot name on Load.
pub fn run(title: String, out: String, slots: Vec<Slot>) -> Result<bool> {
    let mut term = setup_terminal()?;
    // Restore the terminal on any early return / panic — the caller reads our
    // exit code, so a stranded raw-mode popup pane would be worse than a normal
    // one that just closes.
    let _guard = TerminalGuard;

    let mut dialog = Dialog::new(title, slots);

    let outcome = loop {
        term.draw(|f| dialog.render(f, f.area()))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if let Step::Done(outcome) = dialog.handle_key(k.code) {
                    break outcome;
                }
            }
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                let area = term.get_frame().area();
                if let Step::Done(outcome) = dialog.handle_click(area, m.column, m.row) {
                    break outcome;
                }
            }
            _ => {}
        }
    };

    restore_terminal(&mut term)?;
    match outcome {
        Outcome::Load(name) => {
            std::fs::write(&out, name)?;
            Ok(true)
        }
        Outcome::Cancel => Ok(false),
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture so the dialog is clickable; tmux forwards mouse events to
    // the popup pane when its `mouse` option is on.
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
/// raw mode. The happy path still calls [`restore_terminal`] explicitly;
/// re-issuing the escapes after a clean restore is harmless.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
