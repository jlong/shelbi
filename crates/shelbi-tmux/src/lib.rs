//! tmux abstractions: sessions, windows, send-keys, capture-pane.
//!
//! All operations route through `shelbi-ssh`, so the same code works on a
//! local tmux server or one reached via SSH.

use shelbi_core::{Host, Result, TmuxAddr};

/// Does a tmux session with this name exist on `host`?
pub fn has_session(host: &Host, name: &str) -> Result<bool> {
    let out = shelbi_ssh::run(host, ["tmux", "has-session", "-t", name])
        .map_err(shelbi_core::Error::Io)?;
    Ok(out.status.success())
}

/// Create a detached tmux session with one initial window. Caller is
/// expected to check `has_session` first if they don't want this to fail.
pub fn new_session(
    host: &Host,
    name: &str,
    window_name: &str,
    command: Option<&str>,
) -> Result<()> {
    let mut args: Vec<String> = vec![
        "tmux".into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.into(),
        "-n".into(),
        window_name.into(),
    ];
    if let Some(cmd) = command {
        args.push(cmd.into());
    }
    shelbi_ssh::run_capture(host, &args)?;
    Ok(())
}

/// Create a new window inside an existing session.
pub fn new_window(
    host: &Host,
    session: &str,
    window_name: &str,
    command: Option<&str>,
) -> Result<TmuxAddr> {
    let mut args: Vec<String> = vec![
        "tmux".into(),
        "new-window".into(),
        "-d".into(),
        "-t".into(),
        format!("{session}:"),
        "-n".into(),
        window_name.into(),
    ];
    if let Some(cmd) = command {
        args.push(cmd.into());
    }
    shelbi_ssh::run_capture(host, &args)?;
    Ok(TmuxAddr {
        session: session.into(),
        window: window_name.into(),
    })
}

/// Kill a window in a session.
pub fn kill_window(host: &Host, addr: &TmuxAddr) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "kill-window", "-t", &addr.target()])?;
    Ok(())
}

/// Send a literal string to the target's keyboard input, followed by Enter.
///
/// The string is sent with `-l` so tmux treats it as literal characters,
/// avoiding key-name expansion (e.g. `C-c`). Enter is sent as a separate
/// `Enter` keysym.
pub fn send_line(host: &Host, addr: &TmuxAddr, text: &str) -> Result<()> {
    shelbi_ssh::run_capture(
        host,
        ["tmux", "send-keys", "-t", &addr.target(), "-l", text],
    )?;
    shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &addr.target(), "Enter"])?;
    Ok(())
}

/// Capture the current visible content of the pane as plain text.
///
/// `-p` prints to stdout, `-J` joins wrapped lines.
pub fn capture(host: &Host, addr: &TmuxAddr) -> Result<String> {
    shelbi_ssh::run_capture(
        host,
        ["tmux", "capture-pane", "-p", "-J", "-t", &addr.target()],
    )
}

/// Capture including the scrollback. `-S -<lines>` includes `lines` lines of
/// history before the visible area.
pub fn capture_history(host: &Host, addr: &TmuxAddr, lines: usize) -> Result<String> {
    let start = format!("-{lines}");
    shelbi_ssh::run_capture(
        host,
        [
            "tmux",
            "capture-pane",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            &addr.target(),
        ],
    )
}
