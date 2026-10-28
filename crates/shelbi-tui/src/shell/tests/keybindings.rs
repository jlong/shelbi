//! Settings ▸ Keybindings through the shell's real event path
//! (`tui-settings-keybindings`): every key goes in through
//! `ShellState::handle_event`, and `keys.yaml` is an in-memory [`MemKeys`]
//! (no test touches the real `~/.shelbi`).

use super::*;
use crate::shell::keybindings::Stop as KeyStop;
use shelbi_app::settings::keybindings::Change;
use shelbi_state::keymap::{Action, KanbanAction, KeyChord, SidebarAction};

/// `keys.yaml` in memory: what the page reads and saves, a failure to inject
/// into the next saves, and a gate a test holds a save on.
#[derive(Default)]
pub(crate) struct MemKeys {
    text: Mutex<String>,
    fail: Mutex<Option<String>>,
    saves: Mutex<usize>,
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    ensured: Mutex<usize>,
    reads: Mutex<usize>,
}

impl MemKeys {
    fn with(text: &str) -> Arc<MemKeys> {
        let m = MemKeys::default();
        *m.text.lock().unwrap() = text.to_string();
        Arc::new(m)
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    /// Someone else (an editor, the desktop app) writes the file.
    fn write(&self, text: &str) {
        *self.text.lock().unwrap() = text.to_string();
    }

    fn saves(&self) -> usize {
        *self.saves.lock().unwrap()
    }

    fn reads(&self) -> usize {
        *self.reads.lock().unwrap()
    }
}

impl KeysStore for MemKeys {
    fn read(&self, _project: Option<&str>) -> Result<String, String> {
        *self.reads.lock().unwrap() += 1;
        Ok(self.text())
    }

    fn save(&self, change: &Change, project: Option<&str>) -> Result<String, String> {
        if let Some(gate) = self.gate.lock().unwrap().take() {
            let _ = gate.recv();
        }
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        // As the real save does: the change on the file as it is now.
        let mut text = self.text.lock().unwrap();
        let out = change.apply(&text, project).map_err(|e| e.to_string())?;
        *text = out.clone();
        *self.saves.lock().unwrap() += 1;
        Ok(out)
    }

    fn ensure(&self) -> Result<PathBuf, String> {
        *self.ensured.lock().unwrap() += 1;
        Ok(PathBuf::from("/h/.shelbi/keys.yaml"))
    }
}

fn chord(spec: &str) -> KeyChord {
    KeyChord::parse(spec).unwrap()
}

fn key(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

fn ctrl(st: &mut ShellState, c: char) {
    st.handle_event(key(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn type_str(st: &mut ShellState, s: &str) {
    for c in s.chars() {
        type_key(st, c);
    }
}

/// A shell launched with `text` as its `keys.yaml`, Settings open on the
/// Keybindings page with the keyboard on it.
fn keys_shell(text: &str) -> (ShellState, Arc<MemKeys>) {
    keys_shell_on(settings_state(), text)
}

fn keys_shell_on(mut st: ShellState, text: &str) -> (ShellState, Arc<MemKeys>) {
    let mem = MemKeys::with(text);
    st.keys_store = mem.clone();
    // As at launch: the page holds the file's bindings, the shell runs them.
    assert!(st.settings.keys_mut().seed(text.to_string()));
    st.apply_keymaps();
    st.dispatch_effect(Effect::OpenSettings);
    type_key(&mut st, '2');
    assert_eq!(st.settings.page(), shelbi_app::settings::SettingsPage::Keybindings);
    press(&mut st, KeyCode::Tab);
    settle_keys(&mut st);
    (st, mem)
}

/// Drain the page's reads and saves until none is out.
fn settle_keys(st: &mut ShellState) {
    assert!(
        wait_until(Duration::from_secs(5), || {
            st.poll_settings();
            !st.settings.keys().busy()
        }),
        "keys.yaml's read or save lands"
    );
}

/// Search for `query` and land on its first row: `/`, Ctrl+U (clear what
/// an earlier search left), typing, Enter.
fn go_to(st: &mut ShellState, query: &str) {
    type_key(st, '/');
    assert!(st.settings.keys().editing(), "`/` opens the search");
    ctrl(st, 'u');
    type_str(st, query);
    press(st, KeyCode::Enter);
    assert!(!st.settings.keys().editing());
}

fn chords(st: &ShellState, action: Action) -> Vec<KeyChord> {
    st.keymaps.chords(action)
}

fn screen(st: &mut ShellState, w: u16, h: u16) -> String {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    draw_frame(&mut term, st).unwrap();
    screen_text(&term)
}

const PALETTE: Action = Action::Global(GlobalAction::OpenPalette);
const REFRESH: Action = Action::Sidebar(SidebarAction::Refresh);
const NAV_UP: Action = Action::Sidebar(SidebarAction::NavUp);
const NAV_DOWN: Action = Action::Sidebar(SidebarAction::NavDown);

#[test]
fn a_rebind_saves_and_the_whole_shell_takes_it_at_once() {
    let (mut st, mem) = keys_shell("");
    go_to(&mut st, "command palette");
    assert_eq!(st.settings.keys().stop(), KeyStop::Row(PALETTE));
    press(&mut st, KeyCode::Enter);
    assert!(st.settings.keys().state().capture().is_some(), "Enter captures");
    ctrl(&mut st, 'k');
    assert!(st.overlay.is_none(), "nothing opened while capturing");
    // Not until it is written.
    assert_eq!(chords(&st, PALETTE), [chord("ctrl-p")]);
    settle_keys(&mut st);
    assert_eq!(chords(&st, PALETTE), [chord("ctrl-k")]);
    assert_eq!(mem.text(), "defaults:\n  global:\n    open_palette: ctrl-k\n");
    assert_eq!(st.palette_chord, Some(chord("ctrl-k")));
    // The sidebar footer's palette hint, the board's and the feed's keymaps.
    assert!(st.sidebar_chrome().palette().contains('K'), "{}", st.sidebar_chrome().palette());
    assert_eq!(st.kanban.keymaps.chords(PALETTE), [chord("ctrl-k")]);
    assert_eq!(st.activity.keymaps().chords(PALETTE), [chord("ctrl-k")]);

    // Out of Settings, the new chord opens the palette and the old one doesn't.
    press(&mut st, KeyCode::Esc);
    assert!(!st.settings_open());
    ctrl(&mut st, 'p');
    assert!(st.overlay.is_none(), "Ctrl+P is free now");
    ctrl(&mut st, 'k');
    assert!(matches!(st.overlay, Some(ActiveOverlay::Palette(_))), "Ctrl+K opens it");
}

#[test]
fn a_capture_takes_ctrl_p_ctrl_c_and_the_focus_chords_without_running_them() {
    let (mut st, mem) = keys_shell("");
    go_to(&mut st, "Refresh");
    assert_eq!(st.settings.keys().stop(), KeyStop::Row(REFRESH));
    press(&mut st, KeyCode::Enter);
    ctrl(&mut st, 'p');
    assert!(st.overlay.is_none(), "Ctrl+P didn't open the palette");
    settle_keys(&mut st);
    assert_eq!(chords(&st, REFRESH), [chord("ctrl-p")]);

    // Ctrl+C: Quit's in the sidebar, so it is held as a conflict, and
    // nothing quits.
    type_key(&mut st, 'a');
    ctrl(&mut st, 'c');
    assert!(!st.should_quit);
    let text = screen(&mut st, 100, 40);
    assert!(text.contains("is already Quit"), "{text}");
    press(&mut st, KeyCode::Esc);
    assert!(st.settings_open(), "Esc only ended the capture");
    assert_eq!(mem.saves(), 1, "nothing written for the cancelled capture");

    // Ctrl+H, the focus move, is the capture's too.
    type_key(&mut st, 'a');
    ctrl(&mut st, 'h');
    assert_eq!(st.settings.region(), settings::Region::Page, "focus didn't move");
    settle_keys(&mut st);
    assert_eq!(chords(&st, REFRESH), [chord("ctrl-p"), chord("ctrl-h")]);
}

#[test]
fn add_remove_reset_and_reset_all_from_the_keyboard() {
    let (mut st, mem) = keys_shell("# mine\n");
    go_to(&mut st, "Move selection up");
    assert_eq!(st.settings.keys().stop(), KeyStop::Row(NAV_UP));
    // `a` adds a chord after the others.
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    settle_keys(&mut st);
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up"), chord("w")]);
    // ← / → pick a chord (shown ↑, k, w, then +); Backspace removes the
    // picked one.
    for _ in 0..3 {
        press(&mut st, KeyCode::Left);
    }
    press(&mut st, KeyCode::Right);
    press(&mut st, KeyCode::Backspace);
    settle_keys(&mut st);
    assert_eq!(chords(&st, NAV_UP), [chord("up"), chord("w")]);
    // A capture on the picked chord replaces it; Delete in a capture removes.
    press(&mut st, KeyCode::Left);
    press(&mut st, KeyCode::Enter);
    type_key(&mut st, 'i');
    settle_keys(&mut st);
    assert_eq!(chords(&st, NAV_UP), [chord("i"), chord("w")]);
    press(&mut st, KeyCode::Enter);
    press(&mut st, KeyCode::Delete);
    settle_keys(&mut st);
    assert_eq!(chords(&st, NAV_UP).len(), 1);
    // `u` resets the row.
    type_key(&mut st, 'u');
    settle_keys(&mut st);
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up")]);

    // Reset all, from its button.
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    settle_keys(&mut st);
    assert!(mem.text().contains("nav_up"));
    // Home is Open keys.yaml; Reset all ends the filter row after it.
    press(&mut st, KeyCode::Home);
    for _ in 0..3 {
        press(&mut st, KeyCode::Down);
    }
    assert_eq!(st.settings.keys().stop(), KeyStop::ResetAll);
    press(&mut st, KeyCode::Enter);
    settle_keys(&mut st);
    assert_eq!(mem.text(), "# mine\n", "comments survive, the overrides go");
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up")]);
}

#[test]
fn a_conflict_names_the_other_action_and_needs_a_second_press() {
    let (mut st, mem) = keys_shell("");
    go_to(&mut st, "Move selection up");
    press(&mut st, KeyCode::Enter);
    type_key(&mut st, 'r');
    let text = screen(&mut st, 80, 24);
    assert!(text.contains("is already Refresh"), "{text}");
    // Esc keeps both, nothing written.
    press(&mut st, KeyCode::Esc);
    assert_eq!(mem.saves(), 0);
    assert_eq!(chords(&st, REFRESH), [chord("r")]);
    assert!(!screen(&mut st, 80, 24).contains("is already Refresh"));
    // Again, twice: it moves.
    press(&mut st, KeyCode::Enter);
    type_key(&mut st, 'r');
    type_key(&mut st, 'r');
    settle_keys(&mut st);
    assert!(chords(&st, REFRESH).is_empty(), "moved off Refresh");
    assert_eq!(st.keymaps.sidebar.dispatch(chord("r")), Some(SidebarAction::NavUp));
    assert_eq!(mem.saves(), 1);
}

#[test]
fn an_edit_goes_to_the_projects_block_when_it_owns_the_action() {
    let text = "projects:\n  proj:\n    sidebar:\n      refresh: f5\n  other:\n    sidebar:\n      refresh: f6\n";
    let (mut st, mem) = keys_shell_on(test_state_with(Arc::new(RecordingLifecycle::default())), text);
    assert_eq!(chords(&st, REFRESH), [chord("f5")], "the project's override runs");
    go_to(&mut st, "Refresh");
    press(&mut st, KeyCode::Enter);
    st.handle_event(key(KeyCode::F(7), KeyModifiers::NONE));
    settle_keys(&mut st);
    assert_eq!(
        mem.text(),
        "projects:\n  proj:\n    sidebar:\n      refresh: f7\n  other:\n    sidebar:\n      refresh: f6\n"
    );
    // One the project doesn't set goes to the defaults; other projects stay.
    go_to(&mut st, "Move selection down");
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    settle_keys(&mut st);
    assert!(mem.text().contains("defaults:\n  sidebar:\n    nav_down: [j, down, w]\n"), "{}", mem.text());
    assert!(mem.text().contains("refresh: f6"));

    // Switching project runs the other project's bindings, from the same file.
    st.dispatch_effect(Effect::SwitchProject { project: "other".into() });
    assert_eq!(chords(&st, REFRESH), [chord("f6")]);
    assert_eq!(st.kanban.keymaps.chords(REFRESH), [chord("f6")]);
}

#[test]
fn a_malformed_keys_yaml_is_refused_and_the_bindings_stay() {
    let (mut st, mem) = keys_shell("defaults:\n  sidebar:\n    refresh: f5\n");
    // Broken on disk behind the page's back: the save refuses it.
    mem.write("defaults: [unclosed\n");
    go_to(&mut st, "Move selection up");
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    settle_keys(&mut st);
    assert_eq!(mem.text(), "defaults: [unclosed\n", "never rewritten");
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up")], "shown and running stay as they were");
    assert_eq!(chords(&st, REFRESH), [chord("f5")]);
    let text = screen(&mut st, 100, 40);
    assert!(text.contains("doesn't parse"), "{text}");

    // Reread (as after the editor): still refused, still the old bindings.
    st.load_settings(true);
    settle_keys(&mut st);
    assert_eq!(chords(&st, REFRESH), [chord("f5")]);
    assert!(st.settings.keys().error().unwrap().contains("doesn't parse"));
    // Fixed: the next read takes it.
    mem.write("defaults:\n  sidebar:\n    refresh: f6\n");
    st.load_settings(true);
    settle_keys(&mut st);
    assert_eq!(chords(&st, REFRESH), [chord("f6")]);
    assert_eq!(st.settings.keys().error(), None);
}

#[test]
fn a_failed_save_says_so_and_changes_nothing() {
    let (mut st, mem) = keys_shell("");
    *mem.fail.lock().unwrap() = Some("couldn't write keys.yaml: disk full".into());
    go_to(&mut st, "command palette");
    press(&mut st, KeyCode::Enter);
    ctrl(&mut st, 'k');
    settle_keys(&mut st);
    assert_eq!(chords(&st, PALETTE), [chord("ctrl-p")]);
    assert_eq!(st.settings.keys().state().chords(PALETTE), [chord("ctrl-p")], "shown = running");
    assert_eq!(st.palette_chord, Some(chord("ctrl-p")));
    let text = screen(&mut st, 160, 40);
    assert!(text.contains("disk full"), "{text}");
    // The next one goes through.
    *mem.fail.lock().unwrap() = None;
    press(&mut st, KeyCode::Enter);
    ctrl(&mut st, 'k');
    settle_keys(&mut st);
    assert_eq!(chords(&st, PALETTE), [chord("ctrl-k")]);
    assert_eq!(st.settings.keys().error(), None);
}

#[test]
fn a_save_lands_on_top_of_an_edit_made_elsewhere_meanwhile() {
    let (mut st, mem) = keys_shell("");
    // The desktop app (or an editor) writes the file while the page shows.
    mem.write("# from the desktop app\ndefaults:\n  kanban:\n    refresh: f5\n");
    go_to(&mut st, "Move selection up");
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    settle_keys(&mut st);
    let text = mem.text();
    assert!(text.starts_with("# from the desktop app\n"), "{text}");
    assert!(text.contains("refresh: f5") && text.contains("nav_up"), "{text}");
    // The shell runs the whole file it wrote, the other edit included.
    let kanban_refresh = Action::Kanban(KanbanAction::Refresh);
    assert_eq!(chords(&st, kanban_refresh), [chord("f5")]);
}

#[test]
fn open_keys_yaml_hands_off_to_the_editor_and_rereads_after() {
    let (mut st, mem) = keys_shell("");
    // Open keys.yaml is the header's button, the first stop.
    press(&mut st, KeyCode::Home);
    assert_eq!(st.settings.keys().stop(), KeyStop::OpenKeys);
    press(&mut st, KeyCode::Enter);
    assert_eq!(st.pending_editor, Some(PathBuf::from("/h/.shelbi/keys.yaml")));
    assert_eq!(*mem.ensured.lock().unwrap(), 1, "the stub is made before the editor opens");
    // The editor writes; the run loop rereads Settings when it returns.
    st.pending_editor = None;
    mem.write("defaults:\n  global:\n    open_palette: ctrl-o\n");
    st.load_settings(true);
    settle_keys(&mut st);
    assert_eq!(st.palette_chord, Some(chord("ctrl-o")));
    assert_eq!(st.settings.keys().state().chords(PALETTE), [chord("ctrl-o")]);
}

#[test]
fn hints_follow_a_rebound_nav_key_at_once() {
    let (mut st, _mem) = keys_shell("");
    go_to(&mut st, "Move selection down");
    assert_eq!(st.settings.keys().stop(), KeyStop::Row(NAV_DOWN));
    // Picked `j` (shown ↓, j): replace it with `n`.
    press(&mut st, KeyCode::Right);
    press(&mut st, KeyCode::Enter);
    type_key(&mut st, 'n');
    settle_keys(&mut st);
    // Back on Settings' sidebar, its hint names the new key and `n` moves.
    press(&mut st, KeyCode::Tab);
    let text = screen(&mut st, 100, 30);
    assert!(text.contains("↑↓/kn move"), "{text}");
    assert_eq!(st.settings.stop(), settings::Stop::Page(shelbi_app::settings::SettingsPage::Keybindings));
    type_key(&mut st, 'n');
    assert_eq!(
        st.settings.stop(),
        settings::Stop::Page(shelbi_app::settings::SettingsPage::Terminal),
        "`n` moves down now"
    );
}

#[test]
fn settings_navigation_survives_unbinding_its_keys() {
    let text = "defaults:\n  sidebar:\n    nav_up: []\n    nav_down: []\n    activate: []\n";
    let (mut st, _mem) = keys_shell(text);
    // The arrows and Enter still move and open; Esc still leaves.
    press(&mut st, KeyCode::Down);
    assert_eq!(st.settings.keys().stop(), KeyStop::Context);
    press(&mut st, KeyCode::Tab);
    press(&mut st, KeyCode::Up);
    assert_eq!(st.settings.stop(), settings::Stop::Page(shelbi_app::settings::SettingsPage::General));
    press(&mut st, KeyCode::Enter);
    assert_eq!(st.settings.page(), shelbi_app::settings::SettingsPage::General);
    press(&mut st, KeyCode::Esc);
    assert!(!st.settings_open());
}

#[test]
fn saves_run_one_at_a_time() {
    let (mut st, mem) = keys_shell("");
    let (release, gate) = std::sync::mpsc::channel::<()>();
    *mem.gate.lock().unwrap() = Some(gate);
    go_to(&mut st, "Move selection up");
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    assert!(st.settings.keys().state().saving());
    assert!(screen(&mut st, 80, 24).contains("Saving…"));
    // While it is out nothing else starts.
    type_key(&mut st, 'a');
    assert!(st.settings.keys().state().capture().is_none(), "no capture while saving");
    type_key(&mut st, 'u');
    release.send(()).unwrap();
    settle_keys(&mut st);
    assert_eq!(mem.saves(), 1);
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up"), chord("w")]);
}

#[test]
fn a_read_asked_for_mid_save_runs_after_it() {
    let (mut st, mem) = keys_shell("");
    let (release, gate) = std::sync::mpsc::channel::<()>();
    *mem.gate.lock().unwrap() = Some(gate);
    go_to(&mut st, "Move selection up");
    type_key(&mut st, 'a');
    type_key(&mut st, 'w');
    // Back from an editor while the save is out: the read waits for it.
    let reads = mem.reads();
    st.load_settings(true);
    assert_eq!(mem.reads(), reads, "no read while the save is out");
    release.send(()).unwrap();
    settle_keys(&mut st);
    assert_eq!(mem.reads(), reads + 1, "the read ran once the save landed");
    assert_eq!(chords(&st, NAV_UP), [chord("k"), chord("up"), chord("w")]);
}

#[test]
fn a_paste_goes_into_the_search() {
    let (mut st, _mem) = keys_shell("");
    type_key(&mut st, '/');
    st.handle_event(Event::Paste("toggle zen".into()));
    assert_eq!(st.settings.keys().state().query, "toggle zen");
    let zen = Action::Global(GlobalAction::ZenToggle);
    let rows: Vec<Action> = st
        .settings
        .keys()
        .state()
        .groups()
        .iter()
        .flat_map(|g| g.rows.iter().map(|r| r.action))
        .collect();
    assert!(rows.contains(&zen), "{rows:?}");
    assert!(rows.len() < 4, "narrowed: {rows:?}");
}

#[test]
fn every_state_is_usable_at_80x24() {
    let (mut st, _mem) = keys_shell("defaults:\n  sidebar:\n    refresh: []\n");
    let text = screen(&mut st, 80, 24);
    for want in ["Keybindings", "Search", "All contexts", "Reset all", "Open keys.yaml", "Global", "Open command palette"] {
        assert!(text.contains(want), "{want:?}:\n{text}");
    }
    // A capture, on screen with its keys.
    go_to(&mut st, "Move selection up");
    press(&mut st, KeyCode::Enter);
    let text = screen(&mut st, 80, 24);
    assert!(text.contains("Press keys…") && text.contains("Esc cancel"), "{text}");
    // The conflict line, wrapped inside the page.
    type_key(&mut st, 'j');
    let text = screen(&mut st, 80, 24);
    assert!(text.contains("is already"), "{text}");
    press(&mut st, KeyCode::Esc);
    // The search, and an empty result.
    type_key(&mut st, '/');
    ctrl(&mut st, 'u');
    type_str(&mut st, "zzzz");
    let text = screen(&mut st, 80, 24);
    assert!(text.contains("No actions or shortcuts match."), "{text}");
    assert!(text.contains("zzzz"), "{text}");
    // The context filter, from its control.
    press(&mut st, KeyCode::Esc);
    press(&mut st, KeyCode::Esc);
    press(&mut st, KeyCode::Down);
    assert_eq!(st.settings.keys().stop(), KeyStop::Context);
    press(&mut st, KeyCode::Right);
    press(&mut st, KeyCode::Right);
    let text = screen(&mut st, 80, 24);
    assert!(text.contains(" Sidebar ") && text.contains('▾'), "the dropdown shows the context:\n{text}");
    assert!(!text.contains("Open command palette"), "{text}");
    // Unbound and customized rows say so.
    assert!(text.contains("None") && text.contains('●'), "{text}");
}

#[test]
fn chords_this_terminal_cant_send_are_marked() {
    let (mut st, _mem) = keys_shell("defaults:\n  global:\n    open_palette: [ctrl-p, super-k]\n");
    st.set_caps(Caps { kitty: false, truecolor: true, nested: None, emoji: true });
    go_to(&mut st, "command palette");
    press(&mut st, KeyCode::Right);
    let text = screen(&mut st, 100, 30);
    assert!(text.contains('⚠'), "{text}");
    assert!(text.contains("doesn't send ⌘"), "{text}");
}

