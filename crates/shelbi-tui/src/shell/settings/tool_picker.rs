//! General's Editor and Diff tool pickers (`editor-difftool-tui`), over the
//! shared [`ToolPicker`]: what opens over the page when a picker's field is
//! activated.
//!
//! - The list: Default, the installed command-line tools, the installed
//!   apps (each group under its heading), the current setting when it is
//!   none of those, then Browse… and Other command…. The current one has a
//!   `✓`; the selected one says under the list what picking it does.
//!   Default and a tool save at once.
//! - Browse…: the TUI has no native file panel, so a path prompt over the
//!   page: an `.app` or a program's path, typed with a shell's Tab
//!   completion ([`general::complete_path`]), the folder's matches listed
//!   under it (↑↓ and Enter, or a click, take one), and what the shared
//!   model refuses ([`GeneralForm::browse`]) said in red.
//! - Other command… closes the list and starts the page's command field
//!   ([`GeneralForm::begin_edit_tool`]); the page types it.
//!
//! ```text
//!   ┌ Editor ───────────────────────────────┐
//!   │ Default ($EDITOR or vim)              │
//!   │ Command line                          │
//!   │   Neovim (nvim)                       │
//!   │ Applications                          │
//!   │ ▐  Zed                            ✓ ▌ │
//!   │ Browse…                               │
//!   │ Other command…                        │
//!   │                                       │
//!   │ Opens in the app; the view stays.     │
//!   └───────────────────────────────────────┘
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::settings::general::{self, Change, GeneralForm, LineInput, PathMatch, PickerEntry, ToolPicker, ToolSetting};
use shelbi_state::{ToolChoice, ToolKind};

use super::workflows::{Nav, WorkflowsPage};
use crate::button::{self, Align, Button};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// The most matches the Browse prompt lists at once.
const MATCHES_SHOWN: usize = 8;

/// The list's width, and the Browse prompt's.
const LIST_W: u16 = 48;
const BROWSE_W: u16 = 64;

/// Where Browse starts when the setting isn't an app.
const APPLICATIONS: &str = "/Applications/";

/// What a click on the layer hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// Entry `.0` of the picker's [`ToolPicker::entries`].
    Entry(usize),
    /// Browse's match `.0`.
    Match(usize),
    Cancel,
    Choose,
    /// Inside the layer, on nothing.
    Panel,
}

/// The list open over the page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Menu {
    setting: ToolSetting,
    /// The selected entry's index in [`ToolPicker::entries`].
    selected: usize,
}

/// The Browse path prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Browse {
    setting: ToolSetting,
    input: LineInput,
    /// The typed folder's entries the last part starts.
    matches: Vec<PathMatch>,
    /// The match ↑↓ is on; `None`: the typed path.
    selected: Option<usize>,
    /// Why the path was refused.
    error: Option<String>,
}

impl Browse {
    fn new(setting: ToolSetting, form: &GeneralForm) -> Browse {
        // From the app set now (its folder), else the applications folder.
        let start = form
            .tool(setting)
            .and_then(ToolChoice::app_path)
            .and_then(|p| p.parent())
            .map(|dir| format!("{}/", dir.display()).replace("//", "/"))
            .unwrap_or_else(|| APPLICATIONS.to_string());
        let mut b = Browse { setting, input: LineInput::new(&start), matches: Vec::new(), selected: None, error: None };
        b.typed();
        b
    }

    /// The text changed: list its matches again.
    fn typed(&mut self) {
        self.matches = general::complete_path(&self.input.text()).matches;
        self.selected = None;
        self.error = None;
    }

    /// Tab: complete the text as far as the matches agree.
    fn complete(&mut self) {
        let done = general::complete_path(&self.input.text());
        self.input = LineInput::new(&done.text);
        self.typed();
    }

    /// Take match `i` into the field: a folder goes on into it; an app or
    /// a program is picked.
    fn take(&mut self, form: &mut GeneralForm, i: usize) -> Step {
        let Some(m) = self.matches.get(i).cloned() else {
            return Step::Stay;
        };
        self.input = LineInput::new(&m.text);
        self.typed();
        if m.folder {
            Step::Stay
        } else {
            self.choose(form)
        }
    }

    /// Choose the typed path: saved when the shared model takes it, else
    /// why not.
    fn choose(&mut self, form: &mut GeneralForm) -> Step {
        let text = self.input.text();
        let text = text.trim();
        if text.is_empty() {
            self.error = Some("Type the path of an app or a program.".into());
            return Step::Stay;
        }
        match form.browse(self.setting, &general::expand_home(text)) {
            Ok(change) => Step::Done(change),
            Err(e) => {
                self.error = Some(e);
                Step::Stay
            }
        }
    }

    fn key(&mut self, form: &mut GeneralForm, k: KeyEvent) -> Step {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.matches.len().min(MATCHES_SHOWN);
        match k.code {
            KeyCode::Esc => return Step::Done(None),
            KeyCode::Enter => {
                return match self.selected {
                    Some(i) => self.take(form, i),
                    None => self.choose(form),
                };
            }
            KeyCode::Tab => self.complete(),
            KeyCode::Down if n > 0 => self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(n - 1))),
            KeyCode::Up => self.selected = self.selected.and_then(|i| i.checked_sub(1)),
            KeyCode::Backspace => {
                self.input.backspace();
                self.typed();
            }
            KeyCode::Delete => {
                self.input.delete();
                self.typed();
            }
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.typed();
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.input.insert(c);
                self.typed();
            }
            _ => {}
        }
        Step::Stay
    }
}

/// Where a key or click leaves the layer.
enum Step {
    Stay,
    /// Close the layer, writing this change (if any).
    Done(Option<Change>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Layer {
    Menu(Menu),
    Browse(Browse),
}

/// The pickers' layer over the General page: the list or the Browse
/// prompt, while one shows.
#[derive(Debug, Default)]
pub(crate) struct ToolLayer {
    layer: Option<Layer>,
    /// Click targets from the last draw, the layer's rect first.
    hits: Vec<(Rect, Hit)>,
}

impl ToolLayer {
    /// Whether the list or the prompt shows (it takes every key).
    pub(crate) fn is_open(&self) -> bool {
        self.layer.is_some()
    }

    /// The setting the open layer is for.
    #[cfg(test)]
    pub(crate) fn setting(&self) -> Option<ToolSetting> {
        match self.layer.as_ref()? {
            Layer::Menu(m) => Some(m.setting),
            Layer::Browse(b) => Some(b.setting),
        }
    }

    /// Whether the Browse prompt shows.
    #[cfg(test)]
    pub(crate) fn browsing(&self) -> bool {
        matches!(self.layer, Some(Layer::Browse(_)))
    }

    /// The Browse prompt's text and error.
    #[cfg(test)]
    pub(crate) fn browse_text(&self) -> Option<(String, Option<String>)> {
        match self.layer.as_ref()? {
            Layer::Browse(b) => Some((b.input.text(), b.error.clone())),
            Layer::Menu(_) => None,
        }
    }

    /// Open `setting`'s list on its current entry. Not while the settings
    /// can't be saved (unread, or a save in flight).
    pub(crate) fn open(&mut self, form: &GeneralForm, setting: ToolSetting) -> bool {
        if !form.editable() {
            return false;
        }
        let selected = form.picker(setting).selected();
        self.layer = Some(Layer::Menu(Menu { setting, selected }));
        true
    }

    pub(crate) fn close(&mut self) {
        self.layer = None;
        self.hits.clear();
    }

    /// The installed tools arrived: the list's selection stays in it.
    pub(crate) fn detected(&mut self, form: &GeneralForm) {
        if let Some(Layer::Menu(m)) = self.layer.as_mut() {
            m.selected = m.selected.min(form.picker(m.setting).entries().len() - 1);
        }
    }

    /// A paste into the Browse prompt. Whether it took it.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.layer.as_mut() {
            Some(Layer::Browse(b)) => {
                b.input.paste(text.trim_end_matches(['\n', '\r']));
                b.typed();
                true
            }
            _ => false,
        }
    }

    /// Pick entry `i` of `setting`'s list.
    fn pick(&mut self, form: &mut GeneralForm, setting: ToolSetting, i: usize) -> Option<Change> {
        let entry = form.picker(setting).entries().get(i).cloned()?;
        self.close();
        match entry {
            PickerEntry::Browse => {
                self.layer = Some(Layer::Browse(Browse::new(setting, form)));
                None
            }
            PickerEntry::OtherCommand => {
                form.begin_edit_tool(setting);
                None
            }
            entry => form.choose_entry(setting, &entry),
        }
    }

    /// A key while the layer shows: the change to write, if one was picked.
    pub(crate) fn key(&mut self, form: &mut GeneralForm, k: KeyEvent, nav: Nav) -> Option<Change> {
        match self.layer.as_mut()? {
            Layer::Menu(m) => {
                let n = form.picker(m.setting).entries().len();
                let (setting, selected) = (m.setting, m.selected);
                m.selected = match k.code {
                    KeyCode::Esc => {
                        self.close();
                        return None;
                    }
                    KeyCode::Up => selected.saturating_sub(1),
                    KeyCode::Down => (selected + 1).min(n - 1),
                    KeyCode::Home => 0,
                    KeyCode::End => n - 1,
                    _ if nav.activate || k.code == KeyCode::Char(' ') => return self.pick(form, setting, selected),
                    _ if nav.up => selected.saturating_sub(1),
                    _ if nav.down => (selected + 1).min(n - 1),
                    _ => selected,
                };
                None
            }
            Layer::Browse(b) => {
                let step = b.key(form, k);
                self.step(step)
            }
        }
    }

    /// Close the layer when `step` is done with it.
    fn step(&mut self, step: Step) -> Option<Change> {
        match step {
            Step::Stay => None,
            Step::Done(change) => {
                self.close();
                change
            }
        }
    }

    /// A mouse event while the layer shows (it is modal): a click picks an
    /// entry or a match, or presses a button; a click outside the list
    /// closes it.
    pub(crate) fn mouse(&mut self, form: &mut GeneralForm, m: MouseEvent) -> Option<Change> {
        let hit = self.hits.iter().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|(_, h)| *h);
        let click = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        match self.layer.as_mut()? {
            Layer::Menu(menu) => {
                let (setting, selected) = (menu.setting, menu.selected);
                let last = form.picker(setting).entries().len() - 1;
                match (m.kind, hit) {
                    (_, Some(Hit::Entry(i))) if click => return self.pick(form, setting, i),
                    (_, None) if click => self.close(),
                    (MouseEventKind::ScrollUp, _) => menu.selected = selected.saturating_sub(1),
                    (MouseEventKind::ScrollDown, _) => menu.selected = (selected + 1).min(last),
                    _ => {}
                }
                None
            }
            Layer::Browse(b) => {
                let step = match hit {
                    Some(Hit::Match(i)) if click => b.take(form, i),
                    Some(Hit::Choose) if click => b.choose(form),
                    Some(Hit::Cancel) if click => Step::Done(None),
                    _ => Step::Stay,
                };
                self.step(step)
            }
        }
    }

    /// The key hints while the layer shows.
    pub(crate) fn hints(&self, moves: &str) -> String {
        match self.layer {
            Some(Layer::Browse(_)) => "type a path · Tab complete · ↑↓ matches · ⏎ choose · Esc cancel".to_string(),
            _ => format!("{moves} choose · ⏎ pick · Esc close"),
        }
    }

    /// Draw the layer over `area` (the main area).
    pub(crate) fn render(&mut self, form: &GeneralForm, f: &mut Frame, area: Rect) {
        self.hits.clear();
        match self.layer.clone() {
            None => {}
            Some(Layer::Menu(m)) => self.render_menu(form, f, area, &m),
            Some(Layer::Browse(b)) => self.render_browse(f, area, &b),
        }
    }

    fn render_menu(&mut self, form: &GeneralForm, f: &mut Frame, area: Rect, menu: &Menu) {
        let picker = form.picker(menu.setting);
        let rows = menu_rows(&picker);
        let help = entry_help(&picker, menu.selected);
        let help_lines = shelbi_app::overlay::error_log::wrap(&help, LIST_W as usize - 4);
        let extra = help_lines.len() as u16 + 1;
        let Some(rect) = WorkflowsPage::popup(area, LIST_W, rows.len() as u16 + 2 + extra) else {
            return;
        };
        let inner = WorkflowsPage::frame(f, rect, &format!(" {} ", menu.setting.label()));
        self.hits.push((rect, Hit::Panel));
        let w = inner.width as usize;
        let room = inner.height.saturating_sub(extra) as usize;
        // Keep the selected entry's row in view.
        let at = rows.iter().position(|r| r.entry == Some(menu.selected)).unwrap_or(0);
        let skip = (at + 1).saturating_sub(room.max(1));
        for (n, row) in rows.iter().skip(skip).take(room).enumerate() {
            let r = Rect { y: inner.y + n as u16, height: 1, ..inner };
            let line = match row.entry {
                // A heading lines up with the entries' text.
                None => Line::from(Span::styled(format!("  {}", row.label), Style::default().fg(palette().muted))),
                Some(i) => {
                    let note = if row.current { "✓" } else { "" };
                    super::menu_line(&row.label, note, w, i == menu.selected, false)
                }
            };
            f.render_widget(Paragraph::new(line), r);
            if let Some(i) = row.entry {
                self.hits.insert(0, (r, Hit::Entry(i)));
            }
        }
        let first = inner.y + room as u16 + 1;
        for (y, line) in (first..inner.bottom()).zip(help_lines) {
            f.render_widget(
                Paragraph::new(Span::styled(format!(" {line}"), Style::default().fg(palette().muted))),
                Rect { y, height: 1, ..inner },
            );
        }
    }

    fn render_browse(&mut self, f: &mut Frame, area: Rect, b: &Browse) {
        let width = BROWSE_W.min(area.width.saturating_sub(2));
        let text_w = width.saturating_sub(4) as usize;
        let muted = Style::default().fg(palette().muted);
        let mut lines: Vec<(Line<'static>, Option<Hit>)> = Vec::new();
        for l in shelbi_app::overlay::error_log::wrap(
            "An app's path (it opens in the app) or a program's (it opens in a pane). Tab completes.",
            text_w.max(1),
        ) {
            lines.push((Line::from(Span::styled(l, muted)), None));
        }
        lines.push((Line::raw(""), None));
        let text = b.input.text();
        let field = Textbox::new(&text)
            .caret(Some(b.input.caret()))
            .placeholder("/Applications/Zed.app")
            .clip_left(true)
            .focused(b.selected.is_none())
            .invalid(b.error.is_some());
        for line in field.lines(text_w) {
            lines.push((line, Some(Hit::Panel)));
        }
        if let Some(e) = &b.error {
            for l in shelbi_app::overlay::error_log::wrap(e, text_w.max(1)) {
                lines.push((Line::from(Span::styled(l, Style::default().fg(palette().red))), None));
            }
        }
        // The folder's matches, the one ↑↓ is on lit.
        let start = b.selected.map_or(0, |s| (s + 1).saturating_sub(MATCHES_SHOWN));
        for (i, m) in b.matches.iter().enumerate().skip(start).take(MATCHES_SHOWN) {
            let label = if m.folder { format!("{}/", m.name) } else { m.name.clone() };
            lines.push((super::menu_line(&label, "", text_w, b.selected == Some(i), false), Some(Hit::Match(i))));
        }
        let more = b.matches.len().saturating_sub(start + MATCHES_SHOWN);
        if more > 0 {
            lines.push((Line::from(Span::styled(format!(" … {more} more: type to narrow them"), muted)), None));
        }
        lines.push((Line::raw(""), None));
        let buttons = [Button::secondary("Cancel"), Button::primary("Choose")];
        let mut at = Vec::new();
        for (band, band_at) in button::bands(&buttons, text_w, Align::Right) {
            let y = lines.len() as u16;
            lines.extend(band.into_iter().map(|l| (l, None)));
            at.extend(band_at.into_iter().map(|(x, w, i)| (x, y, w, i)));
        }
        let height = (lines.len() as u16 + 2).min(area.height);
        let Some(rect) = WorkflowsPage::popup(area, width, height) else {
            return;
        };
        let title = format!(" Browse for {} ", match b.setting {
            ToolSetting::Editor => "an editor",
            ToolSetting::DiffTool => "a diff tool",
        });
        let inner = WorkflowsPage::frame(f, rect, &title);
        let content = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        self.hits.push((rect, Hit::Panel));
        for (n, (line, hit)) in lines.into_iter().take(content.height as usize).enumerate() {
            let r = Rect { y: content.y + n as u16, height: 1, ..content };
            f.render_widget(Paragraph::new(line), r);
            if let Some(hit @ Hit::Match(_)) = hit {
                self.hits.insert(0, (r, hit));
            }
        }
        for (x, y, w, i) in at {
            let r = Rect { x: content.x + x, y: content.y + y, width: w, height: 3 }.intersection(content);
            if !r.is_empty() {
                self.hits.insert(0, (r, if i == 0 { Hit::Cancel } else { Hit::Choose }));
            }
        }
    }
}

/// One row of a picker's list: a group's heading, or an entry.
struct MenuRow {
    label: String,
    /// The entry's index in [`ToolPicker::entries`]; `None` for a heading.
    entry: Option<usize>,
    current: bool,
}

/// A picker's list as drawn: Default, the command-line tools and the apps
/// each under a heading (indented), the current custom setting, Browse and
/// Other command. A CLI tool says its command when its name isn't it
/// (`Neovim (nvim)`).
fn menu_rows(picker: &ToolPicker) -> Vec<MenuRow> {
    let current = picker.selected();
    let mut rows = Vec::new();
    let mut entry = 0;
    let mut push = |rows: &mut Vec<MenuRow>, label: String| {
        rows.push(MenuRow { label, entry: Some(entry), current: entry == current });
        entry += 1;
    };
    push(&mut rows, picker.setting.default_label().to_string());
    let groups = [("Command line", &picker.cli), ("Applications", &picker.apps)];
    for (heading, tools) in groups {
        if !tools.is_empty() {
            rows.push(MenuRow { label: heading.to_string(), entry: None, current: false });
        }
        for t in tools.iter() {
            push(&mut rows, format!("  {}", tool_label(t)));
        }
    }
    if let Some(custom) = &picker.custom {
        rows.push(MenuRow { label: "Your setting".to_string(), entry: None, current: false });
        push(&mut rows, format!("  {}", tool_label(custom)));
    }
    push(&mut rows, general::BROWSE_LABEL.to_string());
    push(&mut rows, general::OTHER_COMMAND_LABEL.to_string());
    rows
}

/// A tool's entry: its name, and a command-line tool's command when the
/// name isn't it.
pub(super) fn tool_label(t: &shelbi_state::tools::ToolOption) -> String {
    match t.choice.command() {
        Some(command) if t.kind == ToolKind::Cli && command != t.name => format!("{} ({command})", t.name),
        _ => t.name.clone(),
    }
}

/// What picking entry `i` does, said under the list.
fn entry_help(picker: &ToolPicker, i: usize) -> String {
    match picker.entries().get(i) {
        Some(PickerEntry::Default) => match (&picker.default_detail, picker.setting) {
            (Some(detail), _) => format!("No setting: runs {detail}."),
            (None, ToolSetting::DiffTool) => "No setting: runs git's difftool (none configured).".to_string(),
            (None, ToolSetting::Editor) => "No setting: $EDITOR, else vim.".to_string(),
        },
        Some(PickerEntry::Tool(t)) if t.kind == ToolKind::App => {
            "Opens in the app; the view in Shelbi stays as it is.".to_string()
        }
        Some(PickerEntry::Tool(_)) => "Opens in a pane in the worktree.".to_string(),
        Some(PickerEntry::Browse) => "Pick an app or a program by its path.".to_string(),
        Some(PickerEntry::OtherCommand) => "Type a command line, with its flags (code --wait).".to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::settings::general::Preferences;
    use shelbi_state::tools::ToolOption;
    use shelbi_state::{ThemePreference, UserConfig};

    fn form() -> GeneralForm {
        let mut f = GeneralForm::default();
        f.load(Ok(Preferences { theme: ThemePreference::Dark, editor: None, diff_tool: None }), Some("nvim".into()));
        f.set_detected(super::super::fixture_detected());
        f
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn nav(activate: bool) -> Nav {
        Nav { up: false, down: false, activate }
    }

    fn labels(picker: &ToolPicker) -> Vec<String> {
        menu_rows(picker).into_iter().map(|r| r.label).collect()
    }

    #[test]
    fn the_list_groups_cli_tools_and_apps_under_headings() {
        let f = form();
        assert_eq!(
            labels(&f.picker(ToolSetting::Editor)),
            [
                "Default ($EDITOR or vim)",
                "Command line",
                "  Neovim (nvim)",
                "  Helix (hx)",
                "Applications",
                "  Visual Studio Code",
                "  Zed",
                "Browse…",
                "Other command…"
            ]
        );
        let rows = menu_rows(&f.picker(ToolSetting::Editor));
        // Headings pick nothing; entries are numbered as entries() lists them.
        let entries: Vec<usize> = rows.iter().filter_map(|r| r.entry).collect();
        assert_eq!(entries, (0..7).collect::<Vec<_>>());
        assert!(rows[0].current, "unset: Default is current");
        assert_eq!(entry_help(&f.picker(ToolSetting::Editor), 0), "No setting: runs nvim, $EDITOR.");
        assert_eq!(entry_help(&f.picker(ToolSetting::Editor), 4), "Opens in the app; the view in Shelbi stays as it is.");
    }

    #[test]
    fn keys_pick_a_tool_and_esc_closes() {
        let mut f = form();
        let mut layer = ToolLayer::default();
        assert!(layer.open(&f, ToolSetting::Editor));
        for _ in 0..4 {
            assert_eq!(layer.key(&mut f, key(KeyCode::Down), nav(false)), None);
        }
        let change = layer.key(&mut f, key(KeyCode::Enter), nav(true));
        assert_eq!(change, Some(Change::Editor(Some(ToolChoice::app("/Applications/Zed.app")))));
        assert!(!layer.is_open());
        assert!(!layer.open(&f, ToolSetting::DiffTool), "not while the save is in flight");
        f.saved_result(Ok(UserConfig { editor: Some(ToolChoice::app("/Applications/Zed.app")), ..UserConfig::default() }));

        assert!(layer.open(&f, ToolSetting::Editor));
        assert_eq!(layer.key(&mut f, key(KeyCode::Esc), nav(false)), None);
        assert!(!layer.is_open());
    }

    #[test]
    fn other_command_starts_the_field_and_browse_opens_the_prompt() {
        let mut f = form();
        let mut layer = ToolLayer::default();
        layer.open(&f, ToolSetting::DiffTool);
        assert_eq!(layer.key(&mut f, key(KeyCode::End), nav(false)), None);
        assert_eq!(layer.key(&mut f, key(KeyCode::Enter), nav(true)), None);
        assert_eq!(f.draft_setting(), Some(ToolSetting::DiffTool), "the page types the command");
        assert!(!layer.is_open());
        f.cancel_edit();

        layer.open(&f, ToolSetting::DiffTool);
        layer.key(&mut f, key(KeyCode::End), nav(false));
        layer.key(&mut f, key(KeyCode::Up), nav(false));
        layer.key(&mut f, key(KeyCode::Enter), nav(true));
        assert!(layer.browsing());
        assert_eq!(layer.browse_text().unwrap().0, APPLICATIONS);
    }

    #[test]
    fn browse_completes_and_saves_a_path_or_says_why_not() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shelbi-tui-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Fancy.app/Contents")).unwrap();
        std::fs::write(dir.join("Fancy.app/Contents/Info.plist"), "").unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin/mydiff");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut f = form();
        let mut layer = ToolLayer { layer: Some(Layer::Browse(Browse::new(ToolSetting::Editor, &f))), ..ToolLayer::default() };
        // Type the folder, Tab into `bin/`, then Tab the program's name.
        layer.key(&mut f, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), nav(false));
        assert!(layer.paste(&format!("{}/b", dir.display())));
        layer.key(&mut f, key(KeyCode::Tab), nav(false));
        assert_eq!(layer.browse_text().unwrap().0, format!("{}/bin/", dir.display()));
        layer.key(&mut f, key(KeyCode::Tab), nav(false));
        assert_eq!(layer.browse_text().unwrap().0, exe.display().to_string());
        let change = layer.key(&mut f, key(KeyCode::Enter), nav(true));
        assert_eq!(change, Some(Change::Editor(Some(ToolChoice::Command(exe.display().to_string())))));
        assert!(!layer.is_open());
        f.saved_result(Err("nope".into()));

        // A folder is refused, and says why; ↓ Enter takes the app match.
        layer.layer = Some(Layer::Browse(Browse::new(ToolSetting::DiffTool, &f)));
        layer.key(&mut f, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), nav(false));
        layer.paste(&format!("{}/bin", dir.display()));
        assert_eq!(layer.key(&mut f, key(KeyCode::Enter), nav(true)), None);
        let (_, error) = layer.browse_text().unwrap();
        assert!(error.unwrap().contains("is a folder"));
        layer.key(&mut f, key(KeyCode::Backspace), nav(false));
        layer.key(&mut f, key(KeyCode::Backspace), nav(false));
        layer.key(&mut f, key(KeyCode::Backspace), nav(false));
        layer.key(&mut f, key(KeyCode::Char('F')), nav(false));
        layer.key(&mut f, key(KeyCode::Down), nav(false));
        let change = layer.key(&mut f, key(KeyCode::Enter), nav(true));
        assert_eq!(change, Some(Change::DiffTool(Some(ToolChoice::app(dir.join("Fancy.app"))))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cli_tool_says_its_command() {
        let t = ToolOption::custom(&ToolChoice::Command("code --wait".into()));
        assert_eq!(tool_label(&t), "Visual Studio Code (code --wait)");
        let t = ToolOption::custom(&ToolChoice::app("/Applications/Zed.app"));
        assert_eq!(tool_label(&t), "Zed");
    }
}
