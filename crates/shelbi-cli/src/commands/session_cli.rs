//! `shelbi session …` — the debug surface over the remove-tmux session
//! backend.
//!
//! These subcommands drive `shelbi-session` processes directly through
//! `shelbi-client`, with no tmux and no orchestrator in the loop:
//!
//! - `ls` — enumerate sessions under `~/.shelbi/sessions` with name, short-id,
//!   state (live/dead), size, task, and launch time.
//! - `new` — spawn a detached session running a command.
//! - `kill` — signal a session's child process group.
//! - `send` — deliver text (optionally with a trailing Enter) to a session's
//!   PTY, via a bracketed paste.
//! - `snapshot` — print a session's visible screen as text.
//! - `attach` — a rendered, full-screen, single-session client (see
//!   [`super::session_attach`]).
//!
//! It is a *debug* surface: nothing in the product routes through it yet (the
//! tmux backend stays the default until the remove-tmux cutover). The session
//! selector accepts a short-id, a full session name, a workspace (the last
//! path segment of a name), or any unambiguous prefix/substring of either;
//! when `--project` is set it breaks ties toward that project's sessions.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use shelbi_client::DiscoveredSession;
use shelbi_proto::capability;
use shelbi_session::SpawnSpec;

use super::session_attach;

/// Subcommands of `shelbi session`.
#[derive(Debug, Subcommand)]
pub enum SessionCmd {
    /// List sessions: name, short-id, state, size, task, launch time.
    Ls,
    /// Spawn a detached session running a command (everything after `--`).
    New {
        /// Readable session name recorded in `meta.json`.
        #[arg(long)]
        name: String,
        /// Working directory for the child (default: the current directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Initial screen width in columns.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// Initial screen height in rows.
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Task id this session serves, if any.
        #[arg(long)]
        task: Option<String>,
        /// Keep the full raw output log (off by default).
        #[arg(long = "raw-log")]
        raw_log: bool,
        /// The child program and its arguments — everything after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Kill a session (signal its child's process group).
    Kill {
        /// Session selector (short-id, name, workspace, or unambiguous prefix).
        session: String,
        /// Signal number to send (default: the session's graceful default).
        #[arg(long)]
        signal: Option<i32>,
    },
    /// Send text to a session's PTY, delivered as a bracketed paste.
    Send {
        /// Session selector (short-id, name, workspace, or unambiguous prefix).
        session: String,
        /// The text to deliver.
        text: String,
        /// Also send a carriage return after the text (submit the line).
        #[arg(long)]
        enter: bool,
    },
    /// Print a session's visible screen as text (optionally with history).
    Snapshot {
        /// Session selector (short-id, name, workspace, or unambiguous prefix).
        session: String,
        /// Include this many lines of scrollback above the visible screen.
        #[arg(long)]
        history: Option<u32>,
    },
}

/// Dispatch a `shelbi session` subcommand.
pub fn run(project: Option<String>, cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::Ls => ls(),
        SessionCmd::New {
            name,
            cwd,
            cols,
            rows,
            task,
            raw_log,
            command,
        } => new(name, cwd, cols, rows, task, raw_log, command),
        SessionCmd::Kill { session, signal } => kill(project.as_deref(), &session, signal),
        SessionCmd::Send {
            session,
            text,
            enter,
        } => send(project.as_deref(), &session, &text, enter),
        SessionCmd::Snapshot { session, history } => {
            snapshot(project.as_deref(), &session, history)
        }
    }
}

/// `shelbi attach <workspace>` — attach a rendered, full-screen client to a
/// workspace's session. Detach with `detach_key` (default Ctrl+]); the terminal
/// is restored on exit. The workspace name resolves to its session the same way
/// any session selector does.
pub fn attach_workspace(
    project: Option<String>,
    workspace: String,
    detach_key: String,
) -> Result<()> {
    attach(project.as_deref(), &workspace, &detach_key)
}

/// Enumerate the sessions on disk.
fn discover() -> Result<Vec<DiscoveredSession>> {
    let root = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;
    shelbi_client::list(&root).map_err(|e| anyhow!("listing sessions: {e}"))
}

// --- ls --------------------------------------------------------------------

fn ls() -> Result<()> {
    let mut sessions = discover()?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    sessions.sort_by(|a, b| a.meta.name.cmp(&b.meta.name).then(a.short_id.cmp(&b.short_id)));

    let mut rows: Vec<[String; 6]> = vec![[
        "NAME".into(),
        "ID".into(),
        "STATE".into(),
        "SIZE".into(),
        "TASK".into(),
        "LAUNCHED".into(),
    ]];
    for s in &sessions {
        let state = if s.alive {
            "live".to_string()
        } else {
            dead_state(s)
        };
        let size = if s.alive {
            live_size(s).unwrap_or_else(|| "?".into())
        } else {
            "-".into()
        };
        let task = s.meta.task.clone().unwrap_or_else(|| "-".into());
        rows.push([
            s.meta.name.clone(),
            s.short_id.clone(),
            state,
            size,
            task,
            short_time(&s.meta.launched_at),
        ]);
    }
    print!("{}", format_table(&rows));
    Ok(())
}

/// The `STATE` cell for a dead session, enriched with its exit record when one
/// is on disk (`dead(code=0)` / `dead(sig=15)`).
fn dead_state(s: &DiscoveredSession) -> String {
    let exit_path = s.dir.join("exit.json");
    let Ok(text) = std::fs::read_to_string(&exit_path) else {
        return "dead".to_string();
    };
    match serde_json::from_str::<shelbi_session::ExitRecord>(&text) {
        Ok(rec) => match (rec.code, rec.signal) {
            (Some(c), _) => format!("dead(code={c})"),
            (_, Some(sig)) => format!("dead(sig={sig})"),
            _ => "dead".to_string(),
        },
        Err(_) => "dead".to_string(),
    }
}

/// Best-effort live size: connect and ask the session for its current
/// emulator size. Any failure (a session that just died, a slow socket)
/// collapses to `None` so the listing never blocks on one bad session.
fn live_size(s: &DiscoveredSession) -> Option<String> {
    let (conn, _events) = shelbi_client::Connection::open(&s.sock, None, capability::ALL).ok()?;
    let info = conn.info().ok()?;
    Some(format!("{}x{}", info.cols, info.rows))
}

/// Render an RFC3339 launch timestamp as a compact local `MM-DD HH:MM`, or the
/// raw string if it does not parse.
fn short_time(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%m-%d %H:%M")
            .to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Left-align every column to the widest cell in it, with a two-space gutter.
fn format_table(rows: &[[String; 6]]) -> String {
    let mut widths = [0usize; 6];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                out.push_str(cell);
            } else {
                let pad = widths[i] - cell.chars().count();
                out.push_str(cell);
                out.push_str(&" ".repeat(pad + 2));
            }
        }
        out.push('\n');
    }
    out
}

// --- new -------------------------------------------------------------------

fn new(
    name: String,
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
    task: Option<String>,
    raw_log: bool,
    command: Vec<String>,
) -> Result<()> {
    let cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir().map_err(|e| anyhow!("resolving cwd: {e}"))?,
    };
    let spec = SpawnSpec {
        name: name.clone(),
        cwd,
        cols,
        rows,
        task,
        raw_output_log: raw_log,
        child_argv: command,
    };
    let spawned = shelbi_client::spawn(&spec).map_err(|e| anyhow!("spawning session: {e}"))?;
    println!("started session {} ({name})", spawned.id);
    println!("  socket: {}", spawned.sock.display());
    Ok(())
}

// --- kill / send / snapshot / attach ---------------------------------------

fn kill(project: Option<&str>, selector: &str, signal: Option<i32>) -> Result<()> {
    let sessions = discover()?;
    let idx = resolve_index(&sessions, project, selector)?;
    let s = &sessions[idx];
    if !s.alive {
        println!("session {} ({}) is already dead", s.short_id, s.meta.name);
        return Ok(());
    }
    let (conn, _events) = shelbi_client::Connection::open(&s.sock, None, capability::ALL)
        .map_err(|e| anyhow!("connecting to session: {e}"))?;
    conn.kill(signal).map_err(|e| anyhow!("sending kill: {e}"))?;
    println!("sent kill to {} ({})", s.short_id, s.meta.name);
    Ok(())
}

fn send(project: Option<&str>, selector: &str, text: &str, enter: bool) -> Result<()> {
    let sessions = discover()?;
    let idx = resolve_index(&sessions, project, selector)?;
    let s = &sessions[idx];
    if !s.alive {
        bail!("session {} ({}) is dead; cannot send to it", s.short_id, s.meta.name);
    }
    let (conn, _events) = shelbi_client::Connection::open(&s.sock, None, capability::ALL)
        .map_err(|e| anyhow!("connecting to session: {e}"))?;
    // Deliver as a paste so the session brackets it when the program asked for
    // bracketed paste; Enter is a separate keystroke so it submits the line
    // rather than landing inside the bracketed block.
    conn.paste(text).map_err(|e| anyhow!("sending text: {e}"))?;
    if enter {
        conn.input(b"\r").map_err(|e| anyhow!("sending enter: {e}"))?;
    }
    println!("sent {} bytes to {} ({})", text.len(), s.short_id, s.meta.name);
    Ok(())
}

fn snapshot(project: Option<&str>, selector: &str, history: Option<u32>) -> Result<()> {
    let sessions = discover()?;
    let idx = resolve_index(&sessions, project, selector)?;
    let snap = shelbi_client::snapshot(&sessions[idx], history)
        .map_err(|e| anyhow!("snapshotting session: {e}"))?;
    print!("{}", snap.text);
    if !snap.text.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn attach(project: Option<&str>, selector: &str, detach_key: &str) -> Result<()> {
    let detach = session_attach::parse_detach_key(detach_key)?;
    let mut sessions = discover()?;
    let idx = resolve_index(&sessions, project, selector)?;
    let session = sessions.swap_remove(idx);
    if !session.alive {
        bail!(
            "session {} ({}) is dead; `shelbi session snapshot` shows its final screen",
            session.short_id,
            session.meta.name
        );
    }
    session_attach::attach(session, detach)
}

// --- selector resolution ---------------------------------------------------

/// Resolve a user-typed selector to an index into `sessions`.
///
/// Tried in order, each stage short-circuiting on a unique hit: exact short-id,
/// exact name, workspace (the last `/`-segment of a name), unique short-id
/// prefix, unique name substring. When a stage finds several candidates and a
/// `project` is set, ties are broken toward names under `"<project>/"`; a tie
/// that still has several candidates is an error that lists them.
pub(crate) fn resolve_index(
    sessions: &[DiscoveredSession],
    project: Option<&str>,
    selector: &str,
) -> Result<usize> {
    if sessions.is_empty() {
        bail!("no sessions on this machine (try `shelbi session ls`)");
    }
    // Exact short-id wins outright — ids are unique by construction.
    if let Some(i) = sessions.iter().position(|s| s.short_id == selector) {
        return Ok(i);
    }

    for stage in 0..4 {
        let hits: Vec<usize> = (0..sessions.len())
            .filter(|&i| stage_match(stage, &sessions[i], selector))
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0]),
            _ => {
                if let Some(proj) = project {
                    let prefix = format!("{proj}/");
                    let narrowed: Vec<usize> = hits
                        .iter()
                        .copied()
                        .filter(|&i| sessions[i].meta.name.starts_with(&prefix))
                        .collect();
                    if narrowed.len() == 1 {
                        return Ok(narrowed[0]);
                    }
                }
                let names: Vec<String> = hits
                    .iter()
                    .map(|&i| format!("{} ({})", sessions[i].short_id, sessions[i].meta.name))
                    .collect();
                bail!(
                    "`{selector}` matches several sessions; be more specific:\n  {}",
                    names.join("\n  ")
                );
            }
        }
    }
    bail!("no session matching `{selector}` (try `shelbi session ls`)")
}

/// The match predicate for resolution stage `stage` (ascending specificity):
/// 0 = exact name, 1 = workspace (last `/`-segment), 2 = short-id prefix,
/// 3 = name substring.
fn stage_match(stage: u8, s: &DiscoveredSession, selector: &str) -> bool {
    match stage {
        0 => s.meta.name == selector,
        1 => s.meta.name.rsplit('/').next() == Some(selector),
        2 => s.short_id.starts_with(selector),
        _ => s.meta.name.contains(selector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_session::Meta;
    use std::path::PathBuf;

    fn session(short_id: &str, name: &str, alive: bool) -> DiscoveredSession {
        DiscoveredSession {
            short_id: short_id.to_string(),
            dir: PathBuf::from(format!("/tmp/{short_id}")),
            sock: PathBuf::from(format!("/tmp/{short_id}/sock")),
            meta: Meta {
                id: short_id.to_string(),
                name: name.to_string(),
                argv: vec!["/bin/sh".into()],
                cwd: PathBuf::from("/tmp"),
                task: None,
                launched_at: "2026-10-04T12:00:00Z".into(),
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
            },
            alive,
        }
    }

    #[test]
    fn resolves_by_exact_id_name_segment_prefix_and_substring() {
        let sessions = vec![
            session("aabbccdd", "demo/orch", true),
            session("11223344", "demo/ws/alpha", true),
            session("99887766", "other/ws/beta", true),
        ];
        // exact short-id
        assert_eq!(resolve_index(&sessions, None, "11223344").unwrap(), 1);
        // exact name
        assert_eq!(resolve_index(&sessions, None, "demo/orch").unwrap(), 0);
        // workspace (last segment)
        assert_eq!(resolve_index(&sessions, None, "alpha").unwrap(), 1);
        assert_eq!(resolve_index(&sessions, None, "beta").unwrap(), 2);
        // unique short-id prefix
        assert_eq!(resolve_index(&sessions, None, "aabb").unwrap(), 0);
        // unique name substring
        assert_eq!(resolve_index(&sessions, None, "other").unwrap(), 2);
    }

    #[test]
    fn ambiguous_selector_is_an_error_listing_candidates() {
        let sessions = vec![
            session("aaaa1111", "demo/ws/shared", true),
            session("bbbb2222", "other/ws/shared", true),
        ];
        let err = resolve_index(&sessions, None, "shared").unwrap_err().to_string();
        assert!(err.contains("matches several"), "{err}");
        assert!(err.contains("demo/ws/shared") && err.contains("other/ws/shared"), "{err}");
    }

    #[test]
    fn project_breaks_a_tie_toward_its_own_sessions() {
        let sessions = vec![
            session("aaaa1111", "demo/ws/shared", true),
            session("bbbb2222", "other/ws/shared", true),
        ];
        // With `--project demo`, the ambiguous workspace selector resolves to
        // the demo session.
        assert_eq!(resolve_index(&sessions, Some("demo"), "shared").unwrap(), 0);
        assert_eq!(resolve_index(&sessions, Some("other"), "shared").unwrap(), 1);
    }

    #[test]
    fn no_match_is_an_error() {
        let sessions = vec![session("aaaa1111", "demo/orch", true)];
        assert!(resolve_index(&sessions, None, "nope").is_err());
        assert!(resolve_index(&[], None, "anything").is_err());
    }

    #[test]
    fn table_aligns_columns_with_a_two_space_gutter() {
        let rows = vec![
            ["NAME".into(), "ID".into(), "STATE".into(), "SIZE".into(), "TASK".into(), "LAUNCHED".into()],
            ["demo/orch".into(), "aabb".into(), "live".into(), "80x24".into(), "-".into(), "10-04 12:00".into()],
        ];
        let out = format_table(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        // Header "NAME" is padded to the width of "demo/orch" (9) + 2 gutter.
        assert!(lines[0].starts_with("NAME       ID"), "{:?}", lines[0]);
        assert!(lines[1].starts_with("demo/orch  aabb"), "{:?}", lines[1]);
        // The last column is not trailing-padded.
        assert!(lines[0].ends_with("LAUNCHED"));
    }

    #[test]
    fn short_time_formats_or_passes_through() {
        // A well-formed RFC3339 renders as MM-DD HH:MM in local time; we only
        // assert the shape (local offset varies by host).
        let t = short_time("2026-10-04T12:00:00Z");
        assert_eq!(t.len(), "10-04 12:00".len());
        assert_eq!(&t[2..3], "-");
        // A bad timestamp passes through verbatim.
        assert_eq!(short_time("not-a-time"), "not-a-time");
    }
}
