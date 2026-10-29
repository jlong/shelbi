//! A remote session in the TUI: the other machine's own Shelbi for the
//! project, run over `ssh -t` with this terminal handed over to it
//! (`shelbi_app::launch::terminal`), the way `edit:*` hands it to the editor.
//!
//! While it runs, the window title names the host and project (pushed and
//! popped, so the user's title comes back). When `ssh` drops, the terminal
//! shows the shared "Reconnecting to <host>… (attempt N)" notice and backs
//! off until the host answers; Esc (or Ctrl+C) stops. Quitting the far
//! Shelbi comes back here with a status line.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use shelbi_app::launch::remote::{RemoteConnector, RemoteSession};
use shelbi_app::launch::terminal::{self, ReconnectWait, TerminalRunner};
use shelbi_app::session::ReconnectPolicy;

/// The hint after each reconnect notice.
pub(crate) const STOP_HINT: &str = "Press Esc to stop.";

/// Run `session` in this terminal until the user quits it or stops
/// reconnecting. The shell's terminal modes must already be restored (raw
/// mode off, main screen); the caller re-enters them after. Returns the
/// status line to show.
pub fn run(session: &RemoteSession, connector: &dyn RemoteConnector) -> String {
    let mut out = io::stdout();
    // Push the title, then name the session (OSC 2); popped below.
    let _ = write!(out, "\x1b[22;0t\x1b]2;{}\x07", terminal::window_title(session));
    let _ = writeln!(out, "{}…\r", terminal::connected_label(session));
    let _ = out.flush();
    let end = terminal::run(
        session,
        connector,
        &mut Foreground,
        &mut TerminalWait,
        ReconnectPolicy::default(),
    );
    let _ = write!(out, "\x1b[23;0t");
    let _ = out.flush();
    end.status(session)
}

/// Runs `ssh` with this process's terminal.
struct Foreground;

impl TerminalRunner for Foreground {
    fn run(&mut self, argv: &[String]) -> io::Result<Option<i32>> {
        let (program, args) = argv.split_first().expect("a terminal command");
        let status = std::process::Command::new(program).args(args).status()?;
        Ok(status.code())
    }
}

/// Prints the notice on its own line and waits, in raw mode so Esc reaches
/// us (and Ctrl+C doesn't kill the TUI).
struct TerminalWait;

impl ReconnectWait for TerminalWait {
    fn wait(&mut self, notice: &str, pause: Duration) -> bool {
        let mut out = io::stdout();
        let _ = write!(out, "\r\n\x1b[2K{}\r\n", notice_line(notice));
        let _ = out.flush();
        if enable_raw_mode().is_err() {
            std::thread::sleep(pause);
            return true;
        }
        let keep_going = wait_for_stop(pause);
        let _ = disable_raw_mode();
        keep_going
    }
}

/// The notice with how to stop.
pub(crate) fn notice_line(notice: &str) -> String {
    format!("{notice} {STOP_HINT}")
}

/// Wait `pause`, `false` as soon as the user presses Esc or Ctrl+C.
fn wait_for_stop(pause: Duration) -> bool {
    let until = Instant::now() + pause;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        if !event::poll(left).unwrap_or(false) {
            return true;
        }
        if let Ok(ev) = event::read() {
            if is_stop(&ev) {
                return false;
            }
        }
    }
}

/// Esc or Ctrl+C.
pub(crate) fn is_stop(ev: &Event) -> bool {
    let Event::Key(key) = ev else {
        return false;
    };
    key.kind != KeyEventKind::Release
        && (key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn esc_and_ctrl_c_stop_reconnecting() {
        assert!(is_stop(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))));
        assert!(is_stop(&Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))));
        assert!(!is_stop(&Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))));
        assert!(!is_stop(&Event::FocusGained));
    }

    #[test]
    fn the_notice_says_how_to_stop() {
        assert_eq!(
            notice_line("Reconnecting to devbox… (attempt 2) Network is unreachable"),
            "Reconnecting to devbox… (attempt 2) Network is unreachable Press Esc to stop."
        );
    }
}
