//! shelbi's two top-level entry points:
//!
//! - `run_main(project)` — set up the project's tmux session with the
//!   dashboard layout (sidebar + orchestrator) and `exec tmux attach`.
//!   This is what `shelbi` (no subcommand) invokes.
//! - `run_sidebar(project)` — the minimal ratatui process that lives in
//!   the dashboard's left pane: agent list, status footer, Ctrl+Space
//!   palette.
//!   Selecting an agent switches the tmux window. This is what
//!   `shelbi __sidebar PROJECT` invokes.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

mod app;
mod palette;
mod sidebar;

pub use app::{App, View};

/// Set up the project's tmux session and attach to it. If we're already
/// inside a tmux client, use `switch-client` instead of `attach` (tmux
/// refuses to nest, modern tmux supports switching).
pub fn run_main(project_name: &str) -> Result<()> {
    shelbi_orchestrator::ensure_dashboard(project_name)
        .with_context(|| format!("setting up dashboard for `{project_name}`"))?;

    let session = format!("shelbi-{project_name}");
    let inside_tmux = std::env::var("TMUX").is_ok();

    let args: &[&str] = if inside_tmux {
        &["switch-client", "-t"]
    } else {
        &["attach", "-t"]
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .exec();
        Err(err.into())
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux exited with {status}");
        }
        Ok(())
    }
}

/// Run the minimal ratatui sidebar in the current pane.
pub fn run_sidebar(project_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = App::new_sidebar(project_name);
    let mut pal = palette::PaletteState::new();
    app.refresh().ok();

    let result = sidebar_loop(&mut term, &mut app, &mut pal);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

fn sidebar_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    pal: &mut palette::PaletteState,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh().ok();

        let pal_entries = if pal.open {
            palette::entries(app)
        } else {
            Vec::new()
        };
        let pal_results = if pal.open {
            shelbi_palette::search(&pal_entries, &pal.query)
        } else {
            Vec::new()
        };

        term.draw(|f| {
            let area = f.area();
            sidebar::render_full(f, app, area);
            palette::render(f, pal, &pal_results);
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if pal.open {
                    handle_palette_key(app, pal, &pal_results, k.code, k.modifiers);
                } else {
                    handle_key(app, pal, k.code, k.modifiers);
                }
            }
        }
    }
    Ok(())
}

fn handle_palette_key(
    app: &mut App,
    pal: &mut palette::PaletteState,
    results: &[(shelbi_palette::Entry, u16)],
    code: KeyCode,
    mods: KeyModifiers,
) {
    if is_palette_chord(code, mods) {
        pal.close();
        return;
    }
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        pal.close();
        return;
    }
    match code {
        KeyCode::Esc => pal.close(),
        KeyCode::Up => {
            if pal.selected > 0 {
                pal.selected -= 1;
            }
        }
        KeyCode::Down => {
            if pal.selected + 1 < results.len() {
                pal.selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Some((entry, _)) = results.get(pal.selected) {
                let keep_open = palette::activate(app, entry);
                if !keep_open {
                    pal.close();
                }
            }
        }
        KeyCode::Backspace => pal.backspace(),
        KeyCode::Char(c) => pal.type_char(c),
        _ => {}
    }
}

/// Is this keypress the palette-toggle chord? Ctrl+Space arrives differently
/// depending on the terminal: crossterm reports it as Char(' ') with CONTROL
/// on most modern terminals, but some send the legacy NUL byte.
fn is_palette_chord(code: KeyCode, mods: KeyModifiers) -> bool {
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char(' ')) {
        return true;
    }
    if matches!(code, KeyCode::Null) {
        return true;
    }
    false
}

fn handle_key(
    app: &mut App,
    pal: &mut palette::PaletteState,
    code: KeyCode,
    mods: KeyModifiers,
) {
    if is_palette_chord(code, mods) {
        pal.toggle();
        return;
    }
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.should_quit = true;
        return;
    }
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
        KeyCode::Enter => app.activate_selection(),
        KeyCode::Char('r') => {
            app.refresh().ok();
        }
        _ => {}
    }
}
