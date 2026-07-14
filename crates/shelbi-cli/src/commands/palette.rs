//! `shelbi __palette PROJECT` — full-screen ratatui picker meant to run
//! inside a `tmux display-popup`. Lists every destination the sidebar can
//! reach (Chat, Tasks, each declared workspace, each review-ready task, each
//! legacy spawned agent) plus the global actions; on Enter, performs the
//! action and exits.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use shelbi_palette::{Entry, EntryKind};
use shelbi_state::keymap::{
    load_keymaps, GlobalAction, KeyChord, KeymapDiagnostic, Keymaps, PaletteAction,
};
use shelbi_state::{load_user_config, ProjectSummary, ZenModeState, ZenToggleChord};
use shelbi_tui::{decoration_to_color, App, Row, View, WorkspaceOverview};

use super::zen_intro::{render_intro, step_intro, IntroOutcome, IntroState};

pub fn run(project: String) -> Result<()> {
    // Load the merged keymaps before entering the alt-screen so any
    // parse / collision diagnostics land on the terminal the user can
    // still see. Out-of-process palette → no shared Keymaps with the
    // sidebar; we load our own copy.
    let (keymaps, diags) = load_keymaps(Some(&project));
    for d in &diags {
        eprintln!("{}", format_diag(d));
    }

    let mut term = setup_terminal()?;
    // Backstop: any `?` bail-out (a sub-picker error, a failed `State::new`)
    // or panic between here and the explicit `restore_terminal` below would
    // otherwise leave the user's terminal stuck in raw mode / the alt-screen.
    // The guard restores it on drop no matter how we leave the scope
    // (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F7).
    let _guard = TerminalGuard;
    let mut state = State::new(&project, &keymaps)?;

    // Sub-screens share the palette's alt-screen so the follow-up
    // surface is a re-render rather than a popup-pane flash:
    //
    // - Switch Project — sub-picker. Esc inside it exits the palette
    //   (preserves the prior single-shot UX so the popup feels like a
    //   one-key chord).
    // - Quit Project — confirmation popover. Cancel bounces back to
    //   the main picker so a fat-fingered Enter on the destructive
    //   action doesn't kick the user out of the palette entirely —
    //   they can pick a different option without re-summoning it.
    // - Quit Shelbi — confirmation popover. Handled below the loop
    //   because its cancel-path exits the palette (matches the prior
    //   single-shot UX for that entry); quit-project's bounce-back
    //   loop here intentionally diverges.
    // - Toggle Zen (off → on, first time only) — intro popover. The
    //   gate lives inside the loop body because the popover *may*
    //   suppress the toggle entirely (Cancel) and we still want to
    //   handle the don't-show-again checkbox uniformly.
    let (chosen, switch_target, quit_project_confirmed) = loop {
        let chosen = picker_loop(&mut term, &mut state, &keymaps);
        match &chosen {
            Ok(Some(entry)) if entry.id == "action:switch-project" => {
                let target = run_project_picker(&mut term, &project, &keymaps)?;
                break (chosen, target, false);
            }
            Ok(Some(entry)) if entry.id == "action:quit-project" => {
                if run_quit_project_confirm(&mut term, &project, &keymaps)? {
                    break (chosen, None, true);
                }
                // Cancel: re-enter the main picker with state preserved.
                continue;
            }
            Ok(Some(entry))
                if entry.id == "action:toggle-zen" && should_show_zen_intro(&project) =>
            {
                handle_zen_intro_then_toggle(&mut term, &project, &keymaps)?;
                // Toggle (or its suppression) was handled inline — we
                // don't want the post-loop dispatch to fire a second
                // `toggle_zen_mode` call.
                break (Ok(None), None, false);
            }
            _ => break (chosen, None, false),
        }
    };
    // Propagate a picker/sub-picker error instead of silently discarding it in
    // the `if let Ok(..)` below — an event-read failure would otherwise make
    // the palette exit `Ok(())` as if nothing was chosen (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F7).
    // The terminal is restored by `_guard` on this early return.
    let chosen = chosen?;
    let quit_shelbi_confirmed = match &chosen {
        Some(entry) if entry.id == "action:quit-shelbi" => {
            run_quit_shelbi_confirm(&mut term, &keymaps)?
        }
        _ => false,
    };

    restore_terminal(&mut term)?;

    if let Some(target) = switch_target {
        switch_to_project(&target)?;
    } else if quit_project_confirmed {
        super::quit_project::run(&project)?;
    } else if quit_shelbi_confirmed {
        super::quit_shelbi::run()?;
    } else if let Some(entry) = chosen {
        if entry.id != "action:switch-project"
            && entry.id != "action:quit-project"
            && entry.id != "action:quit-shelbi"
        {
            dispatch(&project, &entry)?;
        }
    }
    Ok(())
}

struct State {
    query: String,
    selected: usize,
    all_entries: Vec<Entry>,
    project: String,
}

impl State {
    fn new(project: &str, keymaps: &Keymaps) -> Result<Self> {
        // Lean on the sidebar's `App` for the row list — same icons,
        // same status decorations, same data source. Anything that
        // changes how the sidebar paints a destination automatically
        // shows up in the palette on the next open.
        let mut app = App::new_sidebar(project);
        app.refresh().ok();
        // Resolve the Zen toggle chord. Prefer the keys.yaml binding
        // (canonical source of truth after the legacy migration); fall
        // back to the legacy `config.yaml` field for non-preset chords
        // the four-value enum can't represent. Defaults to Alt+Z on a
        // missing config — same fallback the probe uses on cooperative
        // terminals.
        let legacy = load_user_config()
            .map(|c| c.keymap.zen_toggle)
            .unwrap_or(ZenToggleChord::AltZ);
        let chord = keymaps.zen_toggle_chord(legacy);
        let all_entries = build_entries(&app, app.zen_mode, chord);
        Ok(Self {
            query: String::new(),
            selected: 0,
            all_entries,
            project: project.to_string(),
        })
    }

    fn results(&self) -> Vec<(Entry, u16)> {
        shelbi_palette::search(&self.all_entries, &self.query)
    }
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

/// RAII backstop that leaves raw mode and the alternate screen on drop, so
/// an error/panic path in [`run`] can't strand the terminal in the palette's
/// full-screen state. The happy path still calls [`restore_terminal`]
/// explicitly (it also shows the cursor via the `Terminal` handle); this
/// guard covers every other exit. Re-issuing the escapes after a clean
/// restore is harmless.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Format a keymap diagnostic for stderr. The foundation crate doesn't
/// implement Display for KeymapDiagnostic, so we tag the severity and
/// location inline.
fn format_diag(d: &KeymapDiagnostic) -> String {
    match d {
        KeymapDiagnostic::Error {
            message, location, ..
        } => match location {
            Some(loc) => format!("keys.yaml error [{loc}]: {message}"),
            None => format!("keys.yaml error: {message}"),
        },
        KeymapDiagnostic::Warning {
            message, location, ..
        } => match location {
            Some(loc) => format!("keys.yaml warning [{loc}]: {message}"),
            None => format!("keys.yaml warning: {message}"),
        },
    }
}

fn picker_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    state: &mut State,
    keymaps: &Keymaps,
) -> Result<Option<Entry>> {
    // Opener-as-close: whatever the user configured for opening the
    // palette also closes it. Resolved at startup so a future runtime
    // reload would need a re-entry — fine here, the palette is a
    // single-shot process.
    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();
    loop {
        let results = state.results();
        term.draw(|f| render(f, state, &results))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(None);
                    }
                }
                match keymaps.palette.dispatch(k) {
                    Some(PaletteAction::Close) => return Ok(None),
                    Some(PaletteAction::Activate) => {
                        if let Some((entry, _)) = results.get(state.selected) {
                            return Ok(Some(entry.clone()));
                        }
                    }
                    Some(PaletteAction::NavUp) => {
                        if state.selected > 0 {
                            state.selected -= 1;
                        }
                    }
                    Some(PaletteAction::NavDown) => {
                        if state.selected + 1 < results.len() {
                            state.selected += 1;
                        }
                    }
                    Some(PaletteAction::Backspace) => {
                        state.query.pop();
                        state.selected = 0;
                    }
                    None => {
                        // Unbound printable Char appends to the query.
                        // Binding a printable character (e.g. `space`) to
                        // a palette action makes it un-typeable in the
                        // query — that's the documented trade-off.
                        if let KeyCode::Char(c) = k.code {
                            if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT {
                                state.query.push(c);
                                state.selected = 0;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render(f: &mut Frame, state: &State, results: &[(Entry, u16)]) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Title.
    let title = Paragraph::new(Line::from(Span::styled(
        format!("shelbi · {}", state.project),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    // Search input.
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[1]);

    // Result list. List row width matches the list pane so we can pad
    // out to the right edge and tuck the shortcut hint flush against
    // it; falls back to no padding when the area is narrower than the
    // content (the shortcut still appears, just not right-aligned).
    let row_width = layout[2].width as usize;
    let items: Vec<ListItem> = results
        .iter()
        .map(|(e, _)| {
            let (glyph, glyph_color) = match &e.decoration {
                Some(d) => (d.glyph.as_str(), decoration_to_color(d.color)),
                None => (e.kind.icon(), Color::DarkGray),
            };
            let prefix = format!(" {glyph} ");
            let label = format!("{:<22}", e.label);
            let mut content_width = prefix.chars().count() + label.chars().count();
            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(glyph_color)),
                Span::raw(label),
            ];
            if let Some(sub) = &e.subtitle {
                let s = format!("  {sub}");
                content_width += s.chars().count();
                spans.push(Span::styled(s, Style::default().fg(Color::DarkGray)));
            }
            if let Some(short) = &e.shortcut {
                let sw = short.chars().count();
                // 1-col right margin keeps the glyph off the pane edge.
                let pad = row_width
                    .saturating_sub(content_width)
                    .saturating_sub(sw)
                    .saturating_sub(1);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                } else {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    short.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(shelbi_tui::theme::SELECTION_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut s = ListState::default();
    if !results.is_empty() {
        s.select(Some(state.selected.min(results.len().saturating_sub(1))));
    }
    f.render_stateful_widget(list, layout[2], &mut s);

    // Footer.
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "↑↓ navigate · Enter activate · Esc / Ctrl+P close",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[3]);
}

// ---------------------------------------------------------------------------
// Entry building + dispatch
//
// Order mirrors `App::rows()` in shelbi-tui: Chat, Tasks, Activity,
// then workspaces, then review-ready tasks, then legacy spawned agents,
// then the two global actions. An empty-query palette should read
// top-to-bottom like the sidebar. We literally walk `app.rows()` to
// build the entry list so the palette inherits whatever icons and
// status decorations the sidebar paints — no parallel lookup to drift.

fn build_entries(app: &App, zen_mode: ZenModeState, zen_chord: ZenToggleChord) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    // First pass: every sidebar row becomes an entry with the row's
    // own decoration attached. Section headers and blanks have no
    // decoration and are skipped.
    for row in app.rows() {
        if let Some(entry) = entry_from_row(&row, &app.workspaces) {
            out.push(entry);
        }
        // The Zen-Mode toggle sits inline with the top nav block so users
        // discover it where they already look for the dashboard.
        if matches!(
            &row,
            Row::Nav {
                view: View::Builtin("activity"),
                ..
            }
        ) {
            out.push(zen_toggle_entry(zen_mode, zen_chord));
        }
    }
    // Hidden-until-queried config openers. They carry the `hidden_until_query`
    // flag so an empty-query palette reads exactly as it did before; typing
    // `E` / `Edit` / `settings` surfaces them. Placed ahead of the global
    // action trail so "Quit Shelbi" stays the structurally-last entry.
    out.extend(edit_entries(&app.project_name));
    out.push(Entry {
        id: "action:switch-project".into(),
        label: "Switch Project".into(),
        kind: EntryKind::Action,
        subtitle: Some("fuzzy-pick another project and swap the dashboard".into()),
        shortcut: None,
        decoration: None,
        hidden_until_query: false,
    });
    out.push(Entry {
        id: "action:quit-project".into(),
        label: "Quit Project".into(),
        kind: EntryKind::Action,
        subtitle: Some(
            "close every pane (workspaces + stash + main) and switch to the next project".into(),
        ),
        shortcut: None,
        decoration: None,
        hidden_until_query: false,
    });
    // Most-destructive action lands last so fuzzy search doesn't
    // surface it ahead of the per-project quit, and so users who
    // scroll the list bottom-up have to step over the project-level
    // option before they hit the global one.
    out.push(Entry {
        id: "action:quit-shelbi".into(),
        label: "Quit Shelbi".into(),
        kind: EntryKind::Action,
        subtitle: Some(
            "close every Shelbi session on this host (all projects + workspaces + stash)".into(),
        ),
        shortcut: None,
        decoration: None,
        hidden_until_query: false,
    });
    out
}

// ---------------------------------------------------------------------------
// "Edit …" config openers
//
// Hidden-until-queried shortcuts that open a project's config files in the
// user's `$EDITOR`. Each is skipped when its target file/dir doesn't exist
// so the palette never offers a dead entry, and the per-agent openers are
// enumerated from the agents dir rather than hardcoded. Paths come from the
// `shelbi-state` helpers so the palette can't drift from where the rest of
// the codebase reads/writes these files.

/// Build the hidden `edit:*` entries for `project`. Every entry sets
/// `hidden_until_query` and only appears once its target exists on disk.
fn edit_entries(project: &str) -> Vec<Entry> {
    let mut candidates: Vec<(String, String, String)> = vec![(
        "edit:project".into(),
        "Edit Project Settings".into(),
        "opens project.yaml".into(),
    )];

    // Per-agent openers, enumerated from `agents/` (orchestrator, developer,
    // review, and any custom agents). Sorted so the list is stable across
    // runs regardless of readdir order.
    if let Ok(dir) = shelbi_state::agents_dir(project) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut agents: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            agents.sort();
            for agent in agents {
                candidates.push((
                    format!("edit:agent:{agent}"),
                    format!("Edit {agent} Settings"),
                    format!("opens agents/{agent}/instructions.md"),
                ));
            }
        }
    }

    candidates.push((
        "edit:zenmode".into(),
        "Edit Zen Mode".into(),
        "opens zenmode.md".into(),
    ));
    candidates.push((
        "edit:workflows".into(),
        "Edit Workflows".into(),
        "opens the workflows/ config".into(),
    ));

    candidates
        .into_iter()
        .filter_map(|(id, label, subtitle)| {
            let path = edit_target_path(project, &id)?;
            // A genuinely-missing target is skipped rather than offered as a
            // dead entry the editor would open on nothing.
            if !path.exists() {
                return None;
            }
            Some(Entry {
                id,
                label,
                kind: EntryKind::Action,
                subtitle: Some(subtitle),
                shortcut: None,
                decoration: None,
                hidden_until_query: true,
            })
        })
        .collect()
}

/// Resolve an `edit:*` id to the config file/dir it opens. Pure mapping —
/// no `$EDITOR`, no side effects — so both [`edit_entries`] (to attach a
/// real path and existence-check it) and [`dispatch`] (to know what to hand
/// the editor) share one source of truth. Returns `None` for an id that
/// isn't a recognized `edit:*` target.
fn edit_target_path(project: &str, id: &str) -> Option<PathBuf> {
    match id.strip_prefix("edit:")? {
        "project" => project_settings_path(project),
        "zenmode" => shelbi_state::zenmode_path(project).ok(),
        "workflows" => shelbi_state::workflows_dir(project).ok(),
        other => {
            let agent = other.strip_prefix("agent:")?;
            Some(
                shelbi_state::agents_dir(project)
                    .ok()?
                    .join(agent)
                    .join("instructions.md"),
            )
        }
    }
}

/// Path to the editable `project.yaml` for `project`, resolving the two
/// config layouts: in-repo projects keep it at `<repo>/.shelbi/project.yaml`
/// (via the config-mode-aware `config_project_dir`), global projects at
/// `~/.shelbi/projects/<name>.yaml`. Prefers the in-repo file when present
/// and otherwise returns the global path so a missing file still yields a
/// stable (and thus skippable) target.
fn project_settings_path(project: &str) -> Option<PathBuf> {
    if let Ok(dir) = shelbi_state::config_project_dir(project) {
        let in_repo = dir.join("project.yaml");
        if in_repo.exists() {
            return Some(in_repo);
        }
    }
    Some(
        shelbi_state::projects_dir()
            .ok()?
            .join(format!("{project}.yaml")),
    )
}

/// Open an `edit:*` entry's target in the user's editor. By the time
/// `dispatch` runs, `run` has already torn the palette's alt-screen down via
/// [`restore_terminal`], so the child inherits a clean normal-mode terminal;
/// we block until it exits and let the palette process close afterward.
/// Editor precedence: `$VISUAL`, then `$EDITOR`, falling back to `vi`. A
/// multi-word editor command (e.g. `code -w`) is split so leading args are
/// preserved before the file path.
fn open_edit_target(project: &str, id: &str) -> Result<()> {
    let path = edit_target_path(project, id)
        .ok_or_else(|| anyhow::anyhow!("unknown edit target: {id}"))?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let prog = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(prog)
        .args(parts)
        .arg(&path)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to launch editor `{editor}`: {e}"))?;
    if !status.success() {
        // A non-zero editor exit isn't fatal to shelbi — surface it but
        // don't fail the palette out.
        eprintln!("shelbi: editor `{editor}` exited with {status}");
    }
    Ok(())
}

/// Translate one sidebar [`Row`] into a palette [`Entry`]. The row's
/// `decoration()` is the source of truth for icon + status tint — this
/// just copies it across and stitches a subtitle suited to the
/// palette's wider layout. Returns `None` for section headers / blanks
/// since they have no destination to activate.
fn entry_from_row(row: &Row, workspaces: &[WorkspaceOverview]) -> Option<Entry> {
    let decoration = row.decoration();
    match row {
        Row::Nav { label, view, .. } => {
            let id = match view {
                View::Builtin(name) => format!("view:{name}"),
                _ => return None,
            };
            Some(Entry {
                id,
                label: label.to_string(),
                kind: EntryKind::View,
                subtitle: nav_subtitle(view),
                shortcut: None,
                decoration,
                hidden_until_query: false,
            })
        }
        Row::Workspace { name, .. } => {
            let machine = workspaces
                .iter()
                .find(|w| w.name == *name)
                .map(|w| w.machine.clone());
            // A workspace row's palette id is keyed purely on its name — the
            // `view` variant doesn't change it, so there's no branch to make.
            Some(Entry {
                id: format!("workspace:{name}"),
                label: name.clone(),
                kind: EntryKind::Agent,
                subtitle: Some(match machine {
                    Some(m) => format!("workspace · {m}"),
                    None => "workspace".into(),
                }),
                shortcut: None,
                decoration,
                hidden_until_query: false,
            })
        }
        Row::Review {
            title,
            location,
            view,
            ..
        } => {
            let id = match view {
                View::ReviewTask(task_id) => format!("review:{task_id}"),
                _ => return None,
            };
            Some(Entry {
                id,
                label: title.clone(),
                kind: EntryKind::Action,
                // Loaded tasks carry their `machine:port` URL; queued ones
                // have no location yet.
                subtitle: Some(match location.as_deref() {
                    Some(loc) => format!("review · {loc}"),
                    None => "review · queued".into(),
                }),
                shortcut: None,
                decoration,
                hidden_until_query: false,
            })
        }
        Row::LegacyAgent {
            id,
            machine,
            status,
            ..
        } => Some(Entry {
            id: format!("agent:{id}"),
            label: id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("{machine} · {status:?}")),
            shortcut: None,
            decoration,
            hidden_until_query: false,
        }),
        Row::Section { .. } | Row::Blank | Row::MachineGroup { .. } => None,
    }
}

/// Per-destination palette subtitle. Sidebar nav rows render label-only,
/// so the subtitle is palette-specific copy that explains what each
/// destination is for first-time users.
fn nav_subtitle(view: &View) -> Option<String> {
    match view {
        View::Builtin("orch") => Some("the orchestrator pane you talk to".into()),
        View::Builtin("tasks") => Some("live `shelbi list`".into()),
        View::Builtin("activity") => Some("human-readable events feed".into()),
        _ => None,
    }
}

/// Build the "Toggle Zen Mode" entry. Label flips with current state
/// so users can see what's on before activating, and the right-aligned
/// shortcut hint mirrors the user's configured chord (defaults to
/// `⌥Z`). When the chord is explicitly disabled we omit the hint
/// rather than render an empty column.
fn zen_toggle_entry(current: ZenModeState, chord: ZenToggleChord) -> Entry {
    let (label, subtitle) = match current {
        ZenModeState::On => ("Turn Zen Mode off", "currently on"),
        ZenModeState::Off => ("Turn Zen Mode on", "currently off"),
        ZenModeState::Paused => ("Turn Zen Mode on", "currently paused"),
    };
    let hint = chord.hint();
    let shortcut = if hint.is_empty() {
        None
    } else {
        Some(hint.to_string())
    };
    Entry {
        id: "action:toggle-zen".into(),
        label: label.into(),
        kind: EntryKind::Action,
        subtitle: Some(subtitle.into()),
        shortcut,
        decoration: None,
        hidden_until_query: false,
    }
}

fn dispatch(project: &str, entry: &Entry) -> Result<()> {
    if let Some(view) = entry.id.strip_prefix("view:") {
        shelbi_orchestrator::show_view(project, view).map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if let Some(workspace) = entry.id.strip_prefix("workspace:") {
        shelbi_orchestrator::focus_workspace(project, workspace).map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if let Some(task_id) = entry.id.strip_prefix("review:") {
        let target = shelbi_orchestrator::load::load_task_by_id(project, task_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        super::run_tmux(["select-window", "-t", &exact_window_target(&target)]);
        return Ok(());
    }
    if let Some(id) = entry.id.strip_prefix("agent:") {
        super::run_tmux(["select-window", "-t", &format!("shelbi-{project}:={id}")]);
        return Ok(());
    }
    if entry.id.starts_with("edit:") {
        // Config opener. `run` has already restored the terminal before
        // dispatch, so the editor inherits a clean normal-mode terminal and
        // the palette process closes once it returns.
        return open_edit_target(project, &entry.id);
    }
    if entry.id == "action:toggle-zen" {
        // Shares the read/write/log path with the TUI's Alt+Z handler
        // and the CLI's `shelbi zen on|off` — only the source tag
        // differs (`user:palette`) so the activity feed can attribute
        // the toggle back to the palette.
        shelbi_state::toggle_zen_mode(project, "user:palette").map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    // `action:quit-project` is intentionally not handled here — the
    // outer `run` flow gates it behind a confirmation popover and
    // invokes `super::quit_project::run` directly on confirm. Routing
    // it through `dispatch` would bypass the popover.
    Ok(())
}

/// Anchor the window-name half of a `session:window` tmux target with `=`
/// so `select-window` matches it exactly rather than by prefix (a bare
/// `w-foo` would otherwise also match `w-foobar`). Session part is left
/// untouched. Leaves a target with no `:` (shouldn't happen for the
/// review addr, but be defensive) as-is.
fn exact_window_target(target: &str) -> String {
    match target.split_once(':') {
        Some((session, window)) => format!("{session}:={window}"),
        None => target.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Zen Mode first-enable intro popover
//
// The popover gates the off → on transition when the user-level
// `zen_intro_seen` flag is unset. Other transitions (paused → on, on →
// off, on → paused) toggle immediately — once the user has *been* in
// Zen, the explanation is redundant.

/// Decide whether the user should see the intro popover for an
/// `action:toggle-zen` invocation. True only when (a) the current
/// project's Zen mode is Off — paused → on means the user already
/// knew about Zen when they paused it — and (b) the user-level
/// `zen_intro_seen` flag is still unset. A missing `state.json` (or
/// `~/.shelbi/state.json`) defaults to "show": the gate degrades
/// permissively so we never silently swallow the first-enable UI.
fn should_show_zen_intro(project: &str) -> bool {
    let project_off = shelbi_state::read_state(project)
        .map(|s| s.zen_mode == ZenModeState::Off)
        .unwrap_or(true);
    if !project_off {
        return false;
    }
    let intro_seen = shelbi_state::read_global_state()
        .map(|s| s.zen_intro_seen)
        .unwrap_or(false);
    !intro_seen
}

/// Run the intro popover, then apply the user's choice. The render +
/// IO step is wrapped here; the project-state side effects (toggle Zen,
/// persist the flag) live in [`apply_zen_intro_result`] so they can be
/// unit-tested without spinning up a terminal.
fn handle_zen_intro_then_toggle<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    project: &str,
    keymaps: &Keymaps,
) -> Result<()> {
    let outcome = run_zen_intro_popover(term, keymaps)?;
    apply_zen_intro_result(project, outcome)
}

/// Apply the popover's exit signal to disk. On Confirm we toggle Zen
/// (same canonical `toggle_zen_mode("user:palette")` event the inline
/// dispatch fires); on Cancel we drop the toggle entirely. In both
/// branches, if the "Don't show this again" checkbox was set we persist
/// `zen_intro_seen = true` so the popover never re-fires — either
/// choice acknowledges that the user has read the explanation.
fn apply_zen_intro_result(project: &str, outcome: ZenIntroResult) -> Result<()> {
    // Confirming will mutate project state. Gate before even the optional
    // "don't show again" preference write so a rejected toggle is entirely
    // side-effect free and can be retried after the daemon restart.
    if outcome.confirmed {
        shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| anyhow::anyhow!(e))?;
    }
    if outcome.dont_show_again {
        // Best-effort — a write failure shouldn't block the toggle (the
        // popover re-firing once more is much milder than swallowing
        // the user's confirm action).
        let _ = shelbi_state::mark_zen_intro_seen();
    }
    if outcome.confirmed {
        shelbi_state::toggle_zen_mode(project, "user:palette").map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

/// Captures the popover's exit signal. Separate struct so the caller
/// can treat the "did the user check the box" question independently
/// of the confirm/cancel decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ZenIntroResult {
    confirmed: bool,
    dont_show_again: bool,
}

fn run_zen_intro_popover<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    keymaps: &Keymaps,
) -> Result<ZenIntroResult> {
    // The palette opener-as-close convention extends to the popover —
    // re-pressing the palette chord while the intro is up exits as if
    // the user cancelled. Matches the Quit popovers' behavior.
    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();

    let mut state = IntroState::default();
    loop {
        let snapshot = state;
        term.draw(|f| render_intro(f, f.area(), &snapshot))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(ZenIntroResult {
                            confirmed: false,
                            dont_show_again: state.dont_show_again,
                        });
                    }
                }
                match step_intro(&mut state, k) {
                    IntroOutcome::Continue => {}
                    IntroOutcome::Cancelled => {
                        return Ok(ZenIntroResult {
                            confirmed: false,
                            dont_show_again: state.dont_show_again,
                        });
                    }
                    IntroOutcome::Confirmed => {
                        return Ok(ZenIntroResult {
                            confirmed: true,
                            dont_show_again: state.dont_show_again,
                        });
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// "Switch project" sub-picker
//
// A second ratatui loop sharing the palette's alt-screen. Lists every
// project except the current one, fuzzy-filtered the same way the main
// palette filters its entries (nucleo via `shelbi_palette::score`).

fn run_project_picker<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    current: &str,
    keymaps: &Keymaps,
) -> Result<Option<String>> {
    let projects: Vec<ProjectSummary> = shelbi_state::list_projects()
        .map_err(|e| anyhow::anyhow!(e))?
        .into_iter()
        .filter(|p| p.name != current)
        .collect();
    if projects.is_empty() {
        // Nothing to switch to — degrade silently; the user lands back
        // on the regular palette flow which exits cleanly.
        return Ok(None);
    }

    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();
    let mut query = String::new();
    let mut selected = 0usize;

    loop {
        let results = filter_projects(&projects, &query);
        term.draw(|f| render_project_picker(f, current, &query, &results, selected))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(None);
                    }
                }
                match keymaps.palette.dispatch(k) {
                    Some(PaletteAction::Close) => return Ok(None),
                    Some(PaletteAction::Activate) => {
                        if let Some(p) = results.get(selected) {
                            return Ok(Some(p.name.clone()));
                        }
                    }
                    Some(PaletteAction::NavUp) => {
                        selected = selected.saturating_sub(1);
                    }
                    Some(PaletteAction::NavDown) => {
                        if selected + 1 < results.len() {
                            selected += 1;
                        }
                    }
                    Some(PaletteAction::Backspace) => {
                        query.pop();
                        selected = 0;
                    }
                    None => {
                        if let KeyCode::Char(c) = k.code {
                            if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT {
                                query.push(c);
                                selected = 0;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn filter_projects(projects: &[ProjectSummary], query: &str) -> Vec<ProjectSummary> {
    if query.is_empty() {
        return projects.to_vec();
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = shelbi_palette::parse_pattern(query);
    let mut hits: Vec<(ProjectSummary, u16)> = projects
        .iter()
        .filter_map(|p| {
            shelbi_palette::score_pattern(&mut matcher, &pattern, &p.name).and_then(|s| {
                if s == 0 {
                    None
                } else {
                    Some((p.clone(), s))
                }
            })
        })
        .collect();
    hits.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    hits.into_iter().map(|(p, _)| p).collect()
}

fn render_project_picker(
    f: &mut Frame,
    current: &str,
    query: &str,
    results: &[ProjectSummary],
    selected: usize,
) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "switch project",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  (current: {current})"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    f.render_widget(title, layout[0]);

    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(query.to_string()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[1]);

    let items: Vec<ListItem> = results
        .iter()
        .map(|p| {
            let m = if p.machine_count == 1 {
                "machine"
            } else {
                "machines"
            };
            let w = if p.workspace_count == 1 {
                "workspace"
            } else {
                "workspaces"
            };
            let spans = vec![
                Span::styled(" ◉ ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{:<22}", p.name)),
                Span::styled(
                    format!("  {} {m} · {} {w}", p.machine_count, p.workspace_count),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(shelbi_tui::theme::SELECTION_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut s = ListState::default();
    if !results.is_empty() {
        s.select(Some(selected.min(results.len().saturating_sub(1))));
    }
    f.render_stateful_widget(list, layout[2], &mut s);

    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "↑↓ navigate · Enter switch · Esc cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[3]);
}

// ---------------------------------------------------------------------------
// "Quit Shelbi" confirmation popover
//
// A second ratatui screen sharing the palette's alt-screen, same
// pattern as `run_project_picker`. Lists every project shelbi is
// currently managing (live `shelbi-<name>` tmux sessions) with a
// count of each one's active workspace panes so users see exactly what
// they're about to tear down. Cancel is default focus — the popover
// is a guard against accidental confirmation, so the safe button is
// the one Enter activates by default.

fn run_quit_shelbi_confirm<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    keymaps: &Keymaps,
) -> Result<bool> {
    let projects = super::quit_shelbi::list_managed_projects();
    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();
    let mut focus_quit = false;

    loop {
        term.draw(|f| render_quit_shelbi_confirm(f, &projects, focus_quit))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(false);
                    }
                }
                // Button-focus toggle (Left/Right/Tab/BackTab) stays
                // hardcoded — it's a confirmation-modal specific that
                // isn't part of the customizable palette action set.
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        focus_quit = !focus_quit;
                        continue;
                    }
                    _ => {}
                }
                match keymaps.palette.dispatch(k) {
                    Some(PaletteAction::Close) => return Ok(false),
                    Some(PaletteAction::Activate) => return Ok(focus_quit),
                    // NavUp / NavDown / Backspace have no meaningful
                    // effect on a confirmation popover; swallow them so
                    // a stray j/k doesn't fall through to printable
                    // input (there is none here, but keeps parity).
                    Some(_) | None => {}
                }
            }
        }
    }
}

// "Quit Project" confirmation popover
//
// A second ratatui screen sharing the palette's alt-screen, same
// pattern as `run_project_picker`. Lists each declared workspace whose
// tmux pane is currently live, with its state + assigned task, so
// users see exactly what's about to be torn down. Cancel is default
// focus — this is a guard against accidental confirmation, so the
// safe button is the one Enter activates by default.

fn run_quit_project_confirm<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    project: &str,
    keymaps: &Keymaps,
) -> Result<bool> {
    let workspaces = super::quit_project::list_active_workspaces(project);
    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();
    let mut focus_quit = false;

    loop {
        term.draw(|f| render_quit_project_confirm(f, project, &workspaces, focus_quit))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(false);
                    }
                }
                // Button-focus toggle (Left/Right/Tab/BackTab) stays
                // hardcoded — see `run_quit_shelbi_confirm` for the
                // reasoning.
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        focus_quit = !focus_quit;
                        continue;
                    }
                    _ => {}
                }
                match keymaps.palette.dispatch(k) {
                    Some(PaletteAction::Close) => return Ok(false),
                    Some(PaletteAction::Activate) => return Ok(focus_quit),
                    Some(_) | None => {}
                }
            }
        }
    }
}

fn render_quit_shelbi_confirm(
    f: &mut Frame,
    projects: &[super::quit_shelbi::ManagedProject],
    focus_quit: bool,
) {
    let area = f.area();
    let dirty_summary = format_dirty_summary(projects);
    // The dirty-workspaces summary line is reserved unconditionally so
    // the buttons / footer don't reflow when one shows up — bordering
    // a red warning on shifting geometry would feel worse than the
    // single line of dim spacing we get when there are no dirty
    // worktrees.
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // blank
            Constraint::Length(4), // warning body (wraps)
            Constraint::Length(1), // blank
            Constraint::Min(1),    // project list
            Constraint::Length(1), // dirty-workspaces warning (blank if none)
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        "Quit Shelbi?",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    let body = Paragraph::new(
        "This will close all open Shelbi sessions across every project, including any \
         active workspaces. In-flight task changes will remain in workspace worktrees but \
         won't be merged.",
    )
    .style(Style::default().fg(Color::Gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[2]);

    let items: Vec<ListItem> = projects
        .iter()
        .map(|p| {
            let count_text = format_active_workspaces(p.active_workspaces);
            ListItem::new(Line::from(vec![
                Span::raw(format!("  {:<22}  ", p.name)),
                Span::styled(count_text, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), layout[4]);

    if let Some(summary) = dirty_summary {
        let warning = Paragraph::new(Line::from(Span::styled(
            summary,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        f.render_widget(warning, layout[5]);
    }

    render_confirm_buttons(f, layout[7], "Quit Shelbi", focus_quit);
    render_confirm_footer(f, layout[8]);
}

/// Compose the popover's "⚠ uncommitted changes in …" line, or `None`
/// when every project's worktrees are clean (the warning row stays
/// blank in that case).
///
/// Workspaces are namespaced by project so two projects with a
/// same-named workspace don't collide in the message — pulled
/// straight from [`super::quit_shelbi::ManagedProject::dirty_workspaces`]
/// which already excludes shelbi's `.claude/` deploy footprint.
fn format_dirty_summary(projects: &[super::quit_shelbi::ManagedProject]) -> Option<String> {
    let mut labels: Vec<String> = projects
        .iter()
        .flat_map(|p| {
            p.dirty_workspaces
                .iter()
                .map(move |w| format!("{}/{}", p.name, w))
        })
        .collect();
    if labels.is_empty() {
        return None;
    }
    labels.sort();
    Some(format!("⚠ uncommitted changes in: {}", labels.join(", ")))
}

fn render_quit_project_confirm(
    f: &mut Frame,
    project: &str,
    workspaces: &[super::quit_project::ActiveWorkspace],
    focus_quit: bool,
) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // blank
            Constraint::Length(3), // warning body (wraps)
            Constraint::Length(1), // blank
            Constraint::Min(1),    // workspace list / empty state
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        format!("Quit project: {project}?"),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    let body = Paragraph::new(
        "This will close the project session and any workspace panes. In-flight \
         task changes will remain in workspace worktrees but won't be merged.",
    )
    .style(Style::default().fg(Color::Gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[2]);

    if workspaces.is_empty() {
        let empty =
            Paragraph::new("No active workspaces.").style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, layout[4]);
    } else {
        let items: Vec<ListItem> = workspaces
            .iter()
            .map(|w| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("  {:<14}  ", w.name)),
                    Span::styled(
                        format!("{:<16}", w.state),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("  {}", w.task),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();
        f.render_widget(List::new(items), layout[4]);
    }

    render_confirm_buttons(f, layout[6], "Quit project", focus_quit);
    render_confirm_footer(f, layout[7]);
}

// Destructive button gets the red tint when focused; the cancel
// button gets the shared selection-gray highlight. Unfocused buttons
// render dim so the focused option is unambiguous at a glance.
fn render_confirm_buttons(f: &mut Frame, area: Rect, quit_label: &str, focus_quit: bool) {
    let cancel_style = if focus_quit {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
            .bg(shelbi_tui::theme::SELECTION_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let quit_style = if focus_quit {
        Style::default()
            .bg(Color::Red)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let buttons = Paragraph::new(Line::from(vec![
        Span::styled("  [ Cancel ]  ", cancel_style),
        Span::raw("  "),
        Span::styled(format!("  [ {quit_label} ]  "), quit_style),
    ]));
    f.render_widget(buttons, area);
}

fn render_confirm_footer(f: &mut Frame, area: Rect) {
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "←→/Tab switch · Enter activate · Esc cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, area);
}

/// Per-project workspace-count suffix shown in the Quit Shelbi popover.
/// Singular/plural distinction keeps `1 active workspace` natural.
fn format_active_workspaces(n: usize) -> String {
    match n {
        0 => "(no active workspaces)".to_string(),
        1 => "(1 active workspace)".to_string(),
        n => format!("({n} active workspaces)"),
    }
}

/// Launch (or focus) `target`'s dashboard and move the attached client to it.
/// The `attach`/`switch-client` call must inherit the terminal's stdio (it's
/// interactive), so it deliberately does NOT go through the capturing
/// [`super::run_tmux`] helper.
fn switch_to_project(target: &str) -> Result<()> {
    shelbi_state::touch_project_launched(target).map_err(|e| anyhow::anyhow!(e))?;
    shelbi_orchestrator::ensure_dashboard(target).map_err(|e| anyhow::anyhow!(e))?;

    let session = format!("shelbi-{target}");
    let inside_tmux = std::env::var("TMUX").is_ok();
    let args: &[&str] = if inside_tmux {
        &["switch-client", "-t"]
    } else {
        &["attach", "-t"]
    };
    let _ = std::process::Command::new("tmux")
        .args(args)
        .arg(&session)
        .status();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_window_target_anchors_only_the_window_half() {
        assert_eq!(
            exact_window_target("shelbi-proj:w-fix-login"),
            "shelbi-proj:=w-fix-login"
        );
        assert_eq!(
            exact_window_target("shelbi-proj:agent-1"),
            "shelbi-proj:=agent-1"
        );
        // Defensive: a target without a `:` is passed through untouched.
        assert_eq!(exact_window_target("no-colon"), "no-colon");
    }

    #[test]
    fn zen_toggle_entry_label_and_subtitle_flip_with_current_state() {
        let on = zen_toggle_entry(ZenModeState::On, ZenToggleChord::AltZ);
        assert_eq!(on.label, "Turn Zen Mode off");
        assert_eq!(on.subtitle.as_deref(), Some("currently on"));

        let off = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(off.label, "Turn Zen Mode on");
        assert_eq!(off.subtitle.as_deref(), Some("currently off"));

        let paused = zen_toggle_entry(ZenModeState::Paused, ZenToggleChord::AltZ);
        assert_eq!(paused.label, "Turn Zen Mode on");
        assert_eq!(paused.subtitle.as_deref(), Some("currently paused"));
    }

    #[test]
    fn zen_toggle_entry_uses_configured_chord_hint() {
        let alt = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(alt.shortcut.as_deref(), Some("⌥Z"));

        let bs = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::CtrlBackslash);
        assert_eq!(bs.shortcut.as_deref(), Some("⌃\\"));
    }

    #[test]
    fn zen_toggle_entry_omits_shortcut_when_chord_disabled() {
        let none = zen_toggle_entry(ZenModeState::On, ZenToggleChord::None);
        assert!(none.shortcut.is_none());
    }

    #[test]
    fn zen_toggle_entry_id_is_stable_dispatch_key() {
        // The dispatch() match keys off this exact id; renaming the entry
        // breaks the toggle silently. Pin it.
        let e = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(e.id, "action:toggle-zen");
    }

    #[test]
    fn build_entries_places_zen_toggle_directly_after_view_block() {
        // A fresh `App` (no `refresh`) carries empty workspace / review / agent
        // lists, so `rows()` is just the three nav entries — perfect for
        // pinning the inline-actions order without touching the disk.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        // View block first (Chat/Tasks/Activity), then the mode toggle,
        // then the global actions trail. Workspaces/reviews/agents are
        // empty in this fixture so they don't appear.
        assert_eq!(
            ids,
            vec![
                "view:orch",
                "view:tasks",
                "view:activity",
                "action:toggle-zen",
                "action:switch-project",
                "action:quit-project",
                "action:quit-shelbi",
            ]
        );
    }

    #[test]
    fn format_active_workspaces_handles_zero_one_and_many() {
        assert_eq!(format_active_workspaces(0), "(no active workspaces)");
        assert_eq!(format_active_workspaces(1), "(1 active workspace)");
        assert_eq!(format_active_workspaces(2), "(2 active workspaces)");
        assert_eq!(format_active_workspaces(11), "(11 active workspaces)");
    }

    fn dirty_project(
        name: &str,
        active: usize,
        dirty: &[&str],
    ) -> super::super::quit_shelbi::ManagedProject {
        super::super::quit_shelbi::ManagedProject {
            name: name.to_string(),
            active_workspaces: active,
            dirty_workspaces: dirty.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn dirty_summary_is_none_when_every_workspace_is_clean() {
        // No warning row when every active workspace's worktree is
        // clean — the popover's reserved warning line stays blank.
        let projects = vec![
            dirty_project("alpha", 2, &[]),
            dirty_project("bravo", 0, &[]),
        ];
        assert!(format_dirty_summary(&projects).is_none());
    }

    #[test]
    fn dirty_summary_namespaces_workspaces_with_their_project() {
        // Two projects each have a workspace named `alice`; without
        // the `project/workspace` prefix the user can't tell them
        // apart from the warning line.
        let projects = vec![
            dirty_project("alpha", 2, &["alice"]),
            dirty_project("bravo", 1, &["alice"]),
        ];
        let summary = format_dirty_summary(&projects).expect("expected warning");
        assert!(summary.starts_with("⚠ uncommitted changes in: "));
        assert!(summary.contains("alpha/alice"));
        assert!(summary.contains("bravo/alice"));
    }

    #[test]
    fn dirty_summary_sorts_labels_for_stable_ordering() {
        // The warning is destructive-confirmation copy: the order
        // shouldn't change between re-renders or runs, otherwise the
        // user's eye can't anchor on a stable list.
        let projects = vec![
            dirty_project("zeta", 2, &["zoe", "carol"]),
            dirty_project("alpha", 1, &["bob"]),
        ];
        let summary = format_dirty_summary(&projects).expect("expected warning");
        assert_eq!(
            summary,
            "⚠ uncommitted changes in: alpha/bob, zeta/carol, zeta/zoe"
        );
    }

    #[test]
    fn quit_shelbi_is_the_absolute_last_entry_in_the_palette() {
        // Destructive global action — fuzzy search shouldn't surface
        // it ahead of per-project quit, and a user scrolling bottom-up
        // should have to step over the project-level option before
        // hitting the host-wide one. Pinned because the dispatch
        // assumes this exact id and a rename would silently break it.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(
            entries.last().map(|e| e.id.as_str()),
            Some("action:quit-shelbi")
        );
    }

    #[test]
    fn nav_entries_carry_the_sidebar_icon_as_their_decoration() {
        // The palette must paint each sidebar destination with the same
        // glyph the sidebar uses, sourced from `Row::decoration` so the
        // two surfaces can't drift.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let by_id = |id: &str| {
            entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"))
        };
        assert_eq!(by_id("view:orch").decoration.as_ref().unwrap().glyph, "💬");
        assert_eq!(by_id("view:tasks").decoration.as_ref().unwrap().glyph, "📋");
        assert_eq!(
            by_id("view:activity").decoration.as_ref().unwrap().glyph,
            "⚡"
        );
    }

    #[test]
    fn global_actions_without_a_sidebar_twin_keep_their_default_icon() {
        // The task scope explicitly excludes the global actions — they
        // have no sidebar row to mirror, so their decoration stays None
        // and the renderer falls back to the dim `EntryKind::icon()`.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        for id in [
            "action:toggle-zen",
            "action:switch-project",
            "action:quit-project",
            "action:quit-shelbi",
        ] {
            let e = entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"));
            assert!(
                e.decoration.is_none(),
                "{id} should fall back to the dim default icon, got {:?}",
                e.decoration
            );
        }
    }

    // ---- "Edit …" config openers ------------------------------------------
    //
    // Exercise the pure id→path resolver and the disk-enumerated build path
    // without launching `$EDITOR`. A distinct project name (`editproj`) keeps
    // these fixtures from colliding with the `demo` project the ordering
    // tests above assume has no scaffolded files. (`ENV_LOCK`, `fresh_home`,
    // and `PathBuf` are declared in the Zen-intro test section below and are
    // module-scoped, so they're reused here.)

    /// Scaffold a global-mode project on disk under `home`: the global
    /// `project.yaml`, an `agents/<id>/instructions.md` per named agent, a
    /// `zenmode.md`, and a `workflows/` dir — everything the edit openers
    /// resolve to. Returns the per-project config dir.
    fn scaffold_edit_project(home: &std::path::Path, name: &str, agents: &[&str]) -> PathBuf {
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        // Global-mode project config lives at `projects/<name>.yaml`.
        std::fs::write(projects.join(format!("{name}.yaml")), format!("name: {name}\n")).unwrap();
        // Config half (agents/, zenmode.md, workflows/) sits under the
        // per-project dir in global mode.
        let cfg = projects.join(name);
        for a in agents {
            let ad = cfg.join("agents").join(a);
            std::fs::create_dir_all(&ad).unwrap();
            std::fs::write(ad.join("instructions.md"), "# instructions\n").unwrap();
        }
        std::fs::write(cfg.join("zenmode.md"), "zen summary\n").unwrap();
        std::fs::create_dir_all(cfg.join("workflows")).unwrap();
        cfg
    }

    #[test]
    fn edit_target_path_resolves_each_id_to_its_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let cfg = scaffold_edit_project(&home, "editproj", &["developer"]);

        assert_eq!(
            edit_target_path("editproj", "edit:project"),
            Some(home.join("projects").join("editproj.yaml"))
        );
        assert_eq!(
            edit_target_path("editproj", "edit:agent:developer"),
            Some(cfg.join("agents").join("developer").join("instructions.md"))
        );
        assert_eq!(
            edit_target_path("editproj", "edit:zenmode"),
            Some(cfg.join("zenmode.md"))
        );
        assert_eq!(
            edit_target_path("editproj", "edit:workflows"),
            Some(cfg.join("workflows"))
        );
        // Not a recognized edit target.
        assert_eq!(edit_target_path("editproj", "edit:bogus"), None);
        assert_eq!(edit_target_path("editproj", "view:orch"), None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn build_entries_emits_edit_entries_enumerated_from_agents_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        scaffold_edit_project(&home, "editproj", &["orchestrator", "developer", "review"]);

        let app = App::new_sidebar("editproj");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();

        for expected in [
            "edit:project",
            "edit:agent:developer",
            "edit:agent:orchestrator",
            "edit:agent:review",
            "edit:zenmode",
            "edit:workflows",
        ] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
        // Every edit entry is hidden-until-query so the default list is
        // unchanged, and "Quit Shelbi" is still structurally last.
        for e in entries.iter().filter(|e| e.id.starts_with("edit:")) {
            assert!(e.hidden_until_query, "{} must be hidden until query", e.id);
        }
        assert_eq!(
            entries.last().map(|e| e.id.as_str()),
            Some("action:quit-shelbi")
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn build_entries_skips_edit_entries_whose_target_is_missing() {
        // Only scaffold project.yaml + one agent — no zenmode.md, no
        // workflows/ dir — and confirm the missing-target openers are
        // dropped rather than offered as dead entries.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("sparse.yaml"), "name: sparse\n").unwrap();
        let ad = projects.join("sparse").join("agents").join("developer");
        std::fs::create_dir_all(&ad).unwrap();
        std::fs::write(ad.join("instructions.md"), "x\n").unwrap();

        let app = App::new_sidebar("sparse");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"edit:project"));
        assert!(ids.contains(&"edit:agent:developer"));
        assert!(!ids.contains(&"edit:zenmode"), "zenmode.md is absent");
        assert!(!ids.contains(&"edit:workflows"), "workflows/ is absent");

        std::env::remove_var("SHELBI_HOME");
    }

    // ---- Zen intro popover gate + apply -----------------------------------
    //
    // These exercise the two project-state helpers wrapping the popover:
    //  - `should_show_zen_intro` — gating decision (acceptance criteria f, g).
    //  - `apply_zen_intro_result` — Confirm/Cancel side effects + flag
    //    persistence (acceptance criteria b, c, d, e).
    //
    // The popover state machine itself lives in `commands::zen_intro` and is
    // covered by the pure-data tests in that module.

    use super::super::test_support::ENV_LOCK;
    use shelbi_state::{
        mark_zen_intro_seen, read_global_state, read_state, write_state, ZenModeState,
    };
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-palette-zen-intro-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Seed the project dir so `read_state`/`write_state` have somewhere
    /// to land. Mirrors how the real palette would be invoked — a
    /// `state.json` may or may not exist yet but the project itself does.
    fn seed_project(home: &std::path::Path, name: &str) {
        let p = home.join("projects").join(name);
        std::fs::create_dir_all(&p).unwrap();
    }

    #[test]
    fn intro_shows_on_fresh_state_with_zen_off() {
        // Acceptance (a): first enable on a fresh `~/.shelbi/state.json` —
        // zen_mode is Off (no `state.json` yet) AND zen_intro_seen is
        // unset, so the popover gate fires.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        assert!(
            should_show_zen_intro("demo"),
            "fresh state must trigger the intro popover"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn intro_is_skipped_once_zen_intro_seen_is_set() {
        // Acceptance (f): subsequent enables with the flag set skip the
        // popover entirely — the toggle should fall through to the
        // normal dispatch path.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        mark_zen_intro_seen().unwrap();
        assert!(
            !should_show_zen_intro("demo"),
            "with zen_intro_seen=true the gate must say skip the popover"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn intro_is_skipped_when_zen_is_paused_or_already_on() {
        // Acceptance (g): paused → on (and on → off / on → paused)
        // never trigger the popover. The user knew about Zen when they
        // paused / turned it on the first time, so re-explaining is
        // noise. We check both non-Off states to pin the gate.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        for non_off in [ZenModeState::Paused, ZenModeState::On] {
            let mut s = read_state("demo").unwrap();
            s.zen_mode = non_off;
            write_state("demo", &s).unwrap();
            assert!(
                !should_show_zen_intro("demo"),
                "zen_mode={non_off:?} must not trigger the popover"
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn apply_cancel_does_not_toggle_zen() {
        // Acceptance (b): Cancel closes the popover without enabling Zen.
        // No `mode=zen` event line should land in the activity log either.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        apply_zen_intro_result(
            "demo",
            ZenIntroResult {
                confirmed: false,
                dont_show_again: false,
            },
        )
        .unwrap();

        // Zen mode untouched.
        let state = read_state("demo").unwrap_or_default();
        assert_eq!(state.zen_mode, ZenModeState::Off);
        // No event line was logged. The events log may or may not exist
        // — either way it must not mention `mode=zen`.
        let evt = shelbi_state::events_log_path().unwrap();
        if evt.exists() {
            let log = std::fs::read_to_string(&evt).unwrap();
            assert!(
                !log.contains("mode=zen"),
                "Cancel must not emit a mode=zen event; got {log}"
            );
        }
        // And the flag is still unset — cancel without checkbox doesn't
        // mark seen.
        assert!(!read_global_state().unwrap().zen_intro_seen);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn apply_confirm_toggles_zen_on_and_emits_event() {
        // Acceptance (c): Confirm enables Zen and emits the canonical
        // event line tagged with `user:palette` so the activity feed
        // can attribute the toggle to the palette source.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        apply_zen_intro_result(
            "demo",
            ZenIntroResult {
                confirmed: true,
                dont_show_again: false,
            },
        )
        .unwrap();

        assert_eq!(read_state("demo").unwrap().zen_mode, ZenModeState::On);
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            log.lines()
                .any(|l| l.contains("project=demo mode=zen off -> on reason=user:palette")),
            "missing canonical palette-toggle event in: {log}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn apply_confirm_with_checkbox_persists_the_flag() {
        // Acceptance (d): checkbox-then-Confirm both enables Zen and
        // persists the flag so the popover never re-fires.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        apply_zen_intro_result(
            "demo",
            ZenIntroResult {
                confirmed: true,
                dont_show_again: true,
            },
        )
        .unwrap();

        assert_eq!(read_state("demo").unwrap().zen_mode, ZenModeState::On);
        assert!(
            read_global_state().unwrap().zen_intro_seen,
            "checkbox + Confirm must persist zen_intro_seen=true"
        );
        // Gate now says skip — second enable from off goes straight
        // through.
        let mut s = read_state("demo").unwrap();
        s.zen_mode = ZenModeState::Off;
        write_state("demo", &s).unwrap();
        assert!(!should_show_zen_intro("demo"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn apply_cancel_with_checkbox_persists_the_flag_but_does_not_toggle() {
        // Acceptance (e): checkbox-then-Cancel persists the flag — the
        // user has read the explanation and explicitly opted out of
        // seeing it again, even though they didn't enable Zen this time.
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        seed_project(&home, "demo");

        apply_zen_intro_result(
            "demo",
            ZenIntroResult {
                confirmed: false,
                dont_show_again: true,
            },
        )
        .unwrap();

        // Zen stays off.
        let state = read_state("demo").unwrap_or_default();
        assert_eq!(state.zen_mode, ZenModeState::Off);
        // But the flag is now set.
        assert!(read_global_state().unwrap().zen_intro_seen);
        // No event line either.
        let evt = shelbi_state::events_log_path().unwrap();
        if evt.exists() {
            let log = std::fs::read_to_string(&evt).unwrap();
            assert!(
                !log.contains("mode=zen"),
                "checkbox-then-Cancel must not emit a mode=zen event; got {log}"
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }
}
