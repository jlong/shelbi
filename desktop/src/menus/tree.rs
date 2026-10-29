//! The menu bar as plain data: a pure function from the window's
//! [`CommandModel`] and the user's [`Keymaps`] to a [`MenuTree`].
//!
//! Every command the registry offers for the current state lands in a menu,
//! placed by its [`CommandKind`] family ([`place`], an exhaustive match with
//! no wildcard, so a new kind fails to compile here until it has a home).
//! The registry's fixed commands (Zen, the error log, add / switch project,
//! detach, the quit pair) keep a slot even when the registry leaves them out,
//! drawn disabled. Families whose items come and go (workspaces, reviews,
//! projects, edit targets) list what exists, with a disabled placeholder when
//! there's nothing.
//!
//! Shortcuts come from `keys.yaml` (the global mode's palette, focus and Zen
//! chords) or are the macOS standards (⌘, ⌘W ⌘Q ⌘C ...). A `keys.yaml` chord
//! becomes a key equivalent only when it is one a menu can carry (it holds
//! Ctrl, Option or Cmd, or is a function key, so a bare letter never steals
//! typing) and macOS doesn't already own it ([`reserved_by`]); those that
//! macOS owns are reported in the settings view ([`reserved_conflicts`]).
//!
//! gpui never appears here; `super::install` converts the tree.

use std::collections::HashSet;

use shelbi_app::command::{Command, CommandModel};
use shelbi_app::{CommandKind, CommandRegistry};
use shelbi_state::keymap::{
    format_chord, Action, DisplayStyle, GlobalAction, Key, KeyChord, Keymaps, Mods,
};

/// The menu bar: top-level menus, left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuTree {
    pub menus: Vec<MenuSpec>,
}

/// One top-level menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSpec {
    pub title: String,
    pub items: Vec<MenuEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item(MenuItemSpec),
    Submenu { title: String, items: Vec<MenuEntry> },
    Separator,
    /// The system's Services submenu.
    Services,
}

/// A clickable item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemSpec {
    pub title: String,
    pub action: MenuAction,
    /// Disabled items are drawn grayed out and do nothing.
    pub enabled: bool,
    pub checked: bool,
    pub shortcut: Option<Shortcut>,
}

/// What an item does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MenuAction {
    /// Run the registry command with this id (`CommandKind::from_id`), the
    /// same path the palette takes.
    Command(String),
    /// A client action outside the registry.
    App(AppCommand),
}

/// The client actions the menus offer besides registry commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppCommand {
    Settings,
    Hide,
    HideOthers,
    ShowAll,
    /// Close this client: every window, nothing stops (⌘Q).
    QuitClient,
    CloseWindow,
    Minimize,
    Zoom,
    ToggleSidebar,
    CommandPalette,
    FocusSidebar,
    FocusMain,
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// View ▸ Bigger (⌘+): the terminal font, a point larger.
    FontBigger,
    /// View ▸ Smaller (⌘−): the terminal font, a point smaller.
    FontSmaller,
    /// View ▸ Actual Size (⌘0): the terminal font back to the default.
    FontActualSize,
    Documentation,
    ReportIssue,
}

/// A key equivalent: the gpui keystroke it is bound as and how it reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shortcut {
    /// gpui's keystroke syntax (`ctrl-p`, `alt-z`, `cmd-,`).
    pub keystroke: String,
    /// The macOS menu form (`⌃P`, `⌥Z`, `⌘,`).
    pub display: String,
}

/// The macOS-standard shortcuts, as `keys.yaml` chord strings. These are the
/// app's own bindings (`super::bind_fixed_keys` binds the same table), never
/// read from `keys.yaml`.
pub const FIXED_SHORTCUTS: &[(AppCommand, &str)] = &[
    (AppCommand::Settings, "super-,"),
    (AppCommand::Hide, "super-h"),
    (AppCommand::HideOthers, "alt-super-h"),
    (AppCommand::QuitClient, "super-q"),
    (AppCommand::CloseWindow, "super-w"),
    (AppCommand::Minimize, "super-m"),
    (AppCommand::ToggleSidebar, "ctrl-super-s"),
    (AppCommand::Cut, "super-x"),
    (AppCommand::Copy, "super-c"),
    (AppCommand::Paste, "super-v"),
    (AppCommand::SelectAll, "super-a"),
];

/// Chords macOS (or the standard app menus) already own, with what they do.
/// A `keys.yaml` binding on one of these never reaches Shelbi reliably.
const RESERVED: &[(&str, &str)] = &[
    ("super-q", "Quit"),
    ("super-w", "Close Window"),
    ("super-h", "Hide Shelbi"),
    ("alt-super-h", "Hide Others"),
    ("super-m", "Minimize"),
    ("super-,", "Settings"),
    ("super-x", "Cut"),
    ("super-c", "Copy"),
    ("super-v", "Paste"),
    ("super-a", "Select All"),
    ("super-z", "Undo"),
    ("shift-super-z", "Redo"),
    ("super-tab", "the app switcher"),
    ("super-space", "Spotlight"),
    ("super-`", "cycling windows"),
    ("alt-super-esc", "Force Quit"),
    ("shift-super-3", "screenshots"),
    ("shift-super-4", "screenshots"),
    ("shift-super-5", "screenshots"),
    ("ctrl-super-q", "Lock Screen"),
    ("ctrl-super-f", "Full Screen"),
    ("ctrl-super-s", "Show Sidebar"),
    ("ctrl-super-space", "Emoji & Symbols"),
    ("ctrl-space", "switching input sources"),
    ("ctrl-up", "Mission Control"),
    ("ctrl-down", "App Exposé"),
    ("ctrl-left", "switching Spaces"),
    ("ctrl-right", "switching Spaces"),
];

/// What macOS uses `chord` for, when it is reserved.
pub fn reserved_by(chord: &KeyChord) -> Option<&'static str> {
    let canonical = chord.canonical();
    RESERVED.iter().find_map(|(spec, what)| {
        let reserved = KeyChord::parse(spec).ok()?;
        (reserved.canonical() == canonical).then_some(*what)
    })
}

/// A `keys.yaml` binding macOS already owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutConflict {
    /// The action's mode in `keys.yaml` (`global`, `kanban`, ...).
    pub mode: &'static str,
    /// The action's `keys.yaml` key (`open_palette`).
    pub action: &'static str,
    /// The chord as a menu shows it (`⌘W`).
    pub chord: String,
    /// What macOS uses it for.
    pub reserved_by: &'static str,
}

impl ShortcutConflict {
    /// One line for the settings view.
    pub fn message(&self) -> String {
        format!(
            "{}.{} is bound to {}, which macOS uses for {}: it won't reach Shelbi.",
            self.mode, self.action, self.chord, self.reserved_by
        )
    }
}

/// Every binding in `keymaps` that a reserved macOS chord shadows, in
/// `keys.yaml` order (mode, then action).
pub fn reserved_conflicts(keymaps: &Keymaps) -> Vec<ShortcutConflict> {
    let mut out = Vec::new();
    for action in Action::all() {
        for chord in keymaps.chords(action) {
            if let Some(what) = reserved_by(&chord) {
                out.push(ShortcutConflict {
                    mode: action.mode(),
                    action: action.key_name(),
                    chord: format_chord(&chord, DisplayStyle::Mac),
                    reserved_by: what,
                });
            }
        }
    }
    out
}

/// `chord` as a menu key equivalent, or `None` when a menu can't carry it
/// (no Ctrl / Option / Cmd and not a function key: a bare `q` would steal
/// typing) or macOS already owns it.
pub fn menu_shortcut(chord: &KeyChord) -> Option<Shortcut> {
    let modified = chord
        .mods
        .intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER);
    if !modified && !matches!(chord.code, Key::F(_)) {
        return None;
    }
    if reserved_by(chord).is_some() {
        return None;
    }
    shortcut(chord)
}

/// The first chord bound to a global `action` that a menu can carry.
fn global_shortcut(keymaps: &Keymaps, action: GlobalAction) -> Option<Shortcut> {
    keymaps
        .global
        .chords_for(action)
        .into_iter()
        .find_map(menu_shortcut)
}

/// The terminal font zoom items' shortcuts (⌘+ ⌘− ⌘0). The window takes
/// the chords itself (`crate::keys::font_zoom`); here they label the items.
pub fn font_zoom_shortcut(command: AppCommand) -> Option<Shortcut> {
    let spec = match command {
        AppCommand::FontBigger => crate::keys::FONT_BIGGER,
        AppCommand::FontSmaller => crate::keys::FONT_SMALLER,
        AppCommand::FontActualSize => crate::keys::FONT_ACTUAL_SIZE,
        _ => return None,
    };
    shortcut(&KeyChord::parse(spec).ok()?)
}

/// A [`FIXED_SHORTCUTS`] entry's shortcut.
pub fn fixed_shortcut(command: AppCommand) -> Option<Shortcut> {
    let (_, spec) = FIXED_SHORTCUTS.iter().find(|(c, _)| *c == command)?;
    shortcut(&KeyChord::parse(spec).ok()?)
}

/// Render `chord` both ways, or `None` for a key with no gpui name.
fn shortcut(chord: &KeyChord) -> Option<Shortcut> {
    Some(Shortcut {
        keystroke: keystroke(chord)?,
        display: format_chord(chord, DisplayStyle::Mac),
    })
}

/// `chord` in gpui's keystroke syntax: `ctrl-alt-shift-cmd-<key>`, with
/// gpui's key names (`space`, `escape`, `pageup`, `f5`).
pub fn keystroke(chord: &KeyChord) -> Option<String> {
    let mut mods = chord.mods;
    let key = match chord.code {
        Key::Char(' ') => "space".to_string(),
        Key::Char(c) if c.is_ascii_uppercase() => {
            mods |= Mods::SHIFT;
            c.to_ascii_lowercase().to_string()
        }
        Key::Char(c) => c.to_string(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Enter => "enter".into(),
        Key::Esc => "escape".into(),
        Key::Tab => "tab".into(),
        Key::BackTab => {
            mods |= Mods::SHIFT;
            "tab".into()
        }
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Insert => "insert".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::F(n) => format!("f{n}"),
    };
    let mut out = String::new();
    for (bit, name) in [
        (Mods::CONTROL, "ctrl-"),
        (Mods::ALT, "alt-"),
        (Mods::SHIFT, "shift-"),
        (Mods::SUPER, "cmd-"),
    ] {
        if mods.contains(bit) {
            out.push_str(name);
        }
    }
    out.push_str(&key);
    Some(out)
}

// ---------------------------------------------------------------------------
// Placement: where each command family lives.
// ---------------------------------------------------------------------------

/// Where a registry command lives in the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Go: the nav views.
    GoView,
    /// Go ▸ Workspaces.
    GoWorkspace,
    /// Go ▸ Reviews ▸ <task>: open, approve, reject.
    GoReview,
    /// View: the Zen toggle and the error log.
    View,
    /// File: add / switch project, open project, edit configuration.
    File,
    /// The app menu: detach and the quit pair.
    App,
    /// Help: Send Feedback…, Acknowledgements.
    Help,
    /// Board-only mutations the registry never lists (they act on a card).
    Board,
}

/// The menu family of `kind`. No wildcard arm: a new kind must be placed.
pub fn place(kind: &CommandKind) -> Placement {
    match kind {
        CommandKind::ShowView(_) => Placement::GoView,
        CommandKind::FocusWorkspace { .. } => Placement::GoWorkspace,
        CommandKind::LoadReview { .. } => Placement::GoReview,
        CommandKind::ApproveReview { .. } => Placement::GoReview,
        CommandKind::RejectReview { .. } => Placement::GoReview,
        CommandKind::ToggleZen => Placement::View,
        CommandKind::OpenErrorLog => Placement::View,
        CommandKind::OpenSettings => Placement::App,
        CommandKind::SendFeedback => Placement::Help,
        CommandKind::OpenAbout => Placement::App,
        CommandKind::OpenAcknowledgements => Placement::Help,
        CommandKind::SwitchProject { .. } => Placement::File,
        CommandKind::AddProject => Placement::File,
        CommandKind::NewWindow => Placement::File,
        CommandKind::OpenEditor { .. } => Placement::File,
        CommandKind::Detach => Placement::App,
        CommandKind::DetachOtherClients => Placement::App,
        CommandKind::QuitProject => Placement::App,
        CommandKind::QuitShelbi => Placement::App,
        CommandKind::MoveIssue { .. }
        | CommandKind::StartIssue { .. }
        | CommandKind::AssignIssue { .. }
        | CommandKind::EditIssue { .. }
        | CommandKind::AddIssue { .. }
        | CommandKind::PrioritizeIssue { .. } => Placement::Board,
    }
}

// ---------------------------------------------------------------------------
// The builder.
// ---------------------------------------------------------------------------

/// What the menus are built from besides the command model.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowFacts {
    /// The sidebar is hidden (the toggle reads "Show Sidebar").
    pub sidebar_collapsed: bool,
}

/// The menu bar for `model` with `keymaps`' shortcuts.
pub fn build(model: &CommandModel, keymaps: &Keymaps, facts: WindowFacts) -> MenuTree {
    let commands = CommandRegistry::new().commands(model);
    let available: HashSet<&str> = commands.iter().map(Command::id).collect();
    let by_place = |p: Placement| commands.iter().filter(move |c| place(c.kind()) == p);

    // A fixed slot: the registry's command when it is offered, else the same
    // item disabled.
    let slot = |id: &str, title: &str, shortcut: Option<Shortcut>| -> MenuEntry {
        let enabled = available.contains(id);
        MenuEntry::Item(MenuItemSpec {
            title: title.to_string(),
            action: MenuAction::Command(id.to_string()),
            enabled,
            checked: false,
            // A disabled item can't fire, so it shows no key equivalent.
            shortcut: if enabled { shortcut } else { None },
        })
    };

    MenuTree {
        menus: vec![
            app_menu(&slot),
            file_menu(model, &by_place, &slot),
            edit_menu(),
            view_menu(model, keymaps, facts, &slot),
            go_menu(model, &by_place),
            window_menu(),
            help_menu(&slot),
        ],
    }
}

fn app_menu(slot: &impl Fn(&str, &str, Option<Shortcut>) -> MenuEntry) -> MenuSpec {
    MenuSpec {
        title: "Shelbi".into(),
        items: vec![
            // The registry's About, as the palette runs it.
            slot("action:about", "About Shelbi", None),
            MenuEntry::Separator,
            // The registry's Settings, with ⌘, (bound to the window's
            // OpenSettings for good; here it labels the item).
            slot("action:settings", "Settings…", fixed_shortcut(AppCommand::Settings)),
            MenuEntry::Separator,
            MenuEntry::Services,
            MenuEntry::Separator,
            // Detach closes this client like ⌘Q (agents keep running); the
            // registry offers it, so it is here too.
            slot("action:detach", "Detach", None),
            slot("action:detach-others", "Detach Other Clients", None),
            MenuEntry::Separator,
            // The two that stop agents: confirmed, and never on a key.
            slot("action:quit-project", "Quit Project…", None),
            slot("action:quit-shelbi", "Quit Shelbi and Stop All Agents…", None),
            MenuEntry::Separator,
            app("Hide Shelbi", AppCommand::Hide),
            app("Hide Others", AppCommand::HideOthers),
            app("Show All", AppCommand::ShowAll),
            MenuEntry::Separator,
            app("Quit Shelbi", AppCommand::QuitClient),
        ],
    }
}

fn file_menu<'a, I: Iterator<Item = &'a Command>>(
    model: &CommandModel,
    by_place: &impl Fn(Placement) -> I,
    slot: &impl Fn(&str, &str, Option<Shortcut>) -> MenuEntry,
) -> MenuSpec {
    let mut projects = Vec::new();
    let mut editors = Vec::new();
    for c in by_place(Placement::File) {
        match c.kind() {
            CommandKind::SwitchProject { project } if !project.is_empty() => {
                let label = model
                    .other_projects
                    .iter()
                    .find(|p| &p.slug == project)
                    .map_or(project.as_str(), |p| p.label.as_str());
                projects.push(command(c, label));
            }
            CommandKind::OpenEditor { .. } => editors.push(command(c, c.title())),
            // The fixed slots below.
            _ => {}
        }
    }
    // ⌘⇧N: the window takes the chord itself (`crate::keys::NEW_WINDOW`);
    // here it labels the item.
    let new_window = KeyChord::parse(crate::keys::NEW_WINDOW)
        .ok()
        .and_then(|c| shortcut(&c));
    MenuSpec {
        title: "File".into(),
        items: vec![
            slot("action:new-window", "New Window", new_window),
            MenuEntry::Separator,
            slot("action:add-project", "Add Project…", None),
            slot("action:switch-project", "Switch Project…", None),
            submenu("Open Project", projects, "No Other Projects"),
            MenuEntry::Separator,
            submenu("Edit Configuration", editors, "Nothing to Edit"),
            MenuEntry::Separator,
            app("Close Window", AppCommand::CloseWindow),
        ],
    }
}

fn edit_menu() -> MenuSpec {
    MenuSpec {
        title: "Edit".into(),
        items: vec![
            app("Cut", AppCommand::Cut),
            app("Copy", AppCommand::Copy),
            app("Paste", AppCommand::Paste),
            app("Select All", AppCommand::SelectAll),
        ],
    }
}

fn view_menu(
    model: &CommandModel,
    keymaps: &Keymaps,
    facts: WindowFacts,
    slot: &impl Fn(&str, &str, Option<Shortcut>) -> MenuEntry,
) -> MenuSpec {
    let keyed = |title: &str, command: AppCommand, action: GlobalAction| {
        MenuEntry::Item(MenuItemSpec {
            shortcut: global_shortcut(keymaps, action),
            ..app_item(title, command)
        })
    };
    let zen_title = if model.zen_on {
        "Turn Zen Mode Off"
    } else {
        "Turn Zen Mode On"
    };
    MenuSpec {
        title: "View".into(),
        items: vec![
            keyed("Command Palette", AppCommand::CommandPalette, GlobalAction::OpenPalette),
            app(
                if facts.sidebar_collapsed {
                    "Show Sidebar"
                } else {
                    "Hide Sidebar"
                },
                AppCommand::ToggleSidebar,
            ),
            MenuEntry::Separator,
            keyed("Focus Sidebar", AppCommand::FocusSidebar, GlobalAction::FocusSidebar),
            keyed("Focus Main Area", AppCommand::FocusMain, GlobalAction::FocusMain),
            MenuEntry::Separator,
            // The terminal font, in every terminal pane (the window zoom is
            // Window ▸ Zoom).
            app("Bigger", AppCommand::FontBigger),
            app("Smaller", AppCommand::FontSmaller),
            app("Actual Size", AppCommand::FontActualSize),
            MenuEntry::Separator,
            slot(
                "action:toggle-zen",
                zen_title,
                global_shortcut(keymaps, GlobalAction::ZenToggle),
            ),
            slot("action:error-log", "Error Log", None),
        ],
    }
}

fn go_menu<'a, I: Iterator<Item = &'a Command>>(
    model: &CommandModel,
    by_place: &impl Fn(Placement) -> I,
) -> MenuSpec {
    // The nav views, with the sidebar's chords (⌘O ⌘I ⌘L; the window takes
    // them itself, the menu labels them).
    let mut items: Vec<MenuEntry> = by_place(Placement::GoView)
        .map(|c| match c.kind() {
            CommandKind::ShowView(view) => {
                let mut entry = command(c, c.title());
                if let MenuEntry::Item(item) = &mut entry {
                    item.shortcut = crate::keys::nav_view_chord(view).and_then(|chord| shortcut(&chord));
                }
                entry
            }
            _ => command(c, c.title()),
        })
        .collect();
    items.push(MenuEntry::Separator);

    let workspaces = by_place(Placement::GoWorkspace)
        .map(|c| command(c, c.title()))
        .collect();
    items.push(submenu("Workspaces", workspaces, "No Workspaces"));

    // One submenu per review task: open it, approve, reject.
    let review_cmds: Vec<&Command> = by_place(Placement::GoReview).collect();
    let reviews = model
        .reviews
        .iter()
        .map(|r| {
            let of_task = |kind: &dyn Fn(&CommandKind) -> bool, title: &str| {
                review_cmds
                    .iter()
                    .find(|c| kind(c.kind()))
                    .map(|c| command(c, title))
            };
            let id = r.task_id.as_str();
            let entries = [
                of_task(
                    &|k| matches!(k, CommandKind::LoadReview { task_id } if task_id == id),
                    "Open Review",
                ),
                of_task(
                    &|k| matches!(k, CommandKind::ApproveReview { id: t } if t == id),
                    "Approve",
                ),
                of_task(
                    &|k| matches!(k, CommandKind::RejectReview { id: t, .. } if t == id),
                    "Reject…",
                ),
            ];
            MenuEntry::Submenu {
                title: r.title.clone(),
                items: entries.into_iter().flatten().collect(),
            }
        })
        .collect();
    items.push(submenu("Reviews", reviews, "No Reviews"));
    MenuSpec {
        title: "Go".into(),
        items,
    }
}

fn window_menu() -> MenuSpec {
    // AppKit appends the open windows itself (this is the app's Windows
    // menu), each titled with the project it shows now (the title follows a
    // switch), the key one checked.
    MenuSpec {
        title: "Window".into(),
        items: vec![
            app("Minimize", AppCommand::Minimize),
            app("Zoom", AppCommand::Zoom),
            MenuEntry::Separator,
        ],
    }
}

fn help_menu(slot: &impl Fn(&str, &str, Option<Shortcut>) -> MenuEntry) -> MenuSpec {
    MenuSpec {
        title: "Help".into(),
        items: vec![
            app("Shelbi Documentation", AppCommand::Documentation),
            app("Report an Issue", AppCommand::ReportIssue),
            // The registry's Send Feedback: Contact Us, as the sidebar
            // footer's megaphone opens it.
            slot("action:send-feedback", "Send Feedback\u{2026}", None),
            MenuEntry::Separator,
            // The registry's Acknowledgements, as About's link opens it.
            slot("action:acknowledgements", "Acknowledgements", None),
        ],
    }
}

/// Where Help ▸ Report an Issue goes.
pub const ISSUES_URL: &str = "https://github.com/jlong/shelbi/issues";

fn app_item(title: &str, command: AppCommand) -> MenuItemSpec {
    MenuItemSpec {
        title: title.to_string(),
        action: MenuAction::App(command),
        enabled: true,
        checked: false,
        shortcut: fixed_shortcut(command).or_else(|| font_zoom_shortcut(command)),
    }
}

fn app(title: &str, command: AppCommand) -> MenuEntry {
    MenuEntry::Item(app_item(title, command))
}

/// An available registry command.
fn command(c: &Command, title: &str) -> MenuEntry {
    MenuEntry::Item(MenuItemSpec {
        title: title.to_string(),
        action: MenuAction::Command(c.id().to_string()),
        enabled: true,
        checked: false,
        shortcut: None,
    })
}

/// A submenu of `items`, or of one disabled `empty` line when there are none.
fn submenu(title: &str, items: Vec<MenuEntry>, empty: &str) -> MenuEntry {
    let items = if items.is_empty() {
        // Disabled, so its action never fires.
        vec![MenuEntry::Item(MenuItemSpec {
            enabled: false,
            ..app_item(empty, AppCommand::Documentation)
        })]
    } else {
        items
    };
    MenuEntry::Submenu {
        title: title.to_string(),
        items,
    }
}

/// Every item in `entries`, depth first.
pub fn items(entries: &[MenuEntry]) -> Vec<&MenuItemSpec> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            MenuEntry::Item(item) => out.push(item),
            MenuEntry::Submenu { items: sub, .. } => out.extend(items(sub)),
            MenuEntry::Separator | MenuEntry::Services => {}
        }
    }
    out
}

impl MenuTree {
    /// Every item in the bar, menu by menu.
    pub fn items(&self) -> Vec<&MenuItemSpec> {
        self.menus.iter().flat_map(|m| items(&m.items)).collect()
    }

    /// The menu titled `title`.
    #[cfg(test)]
    pub fn menu(&self, title: &str) -> Option<&MenuSpec> {
        self.menus.iter().find(|m| m.title == title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::command::{
        EditItem, ProjectItem, ReviewItem, ViewItem, WorkspaceItem,
    };
    use shelbi_app::{EditTarget, View};

    fn defaults() -> Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml("", None).0
    }

    fn keymaps(yaml: &str) -> Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml(yaml, None).0
    }

    /// A project with two nav views, a workspace, a review,
    /// another project, an edit target and a second client.
    fn fixture() -> CommandModel {
        let view = |id: &str, title: &str, view: View| ViewItem {
            id: id.into(),
            title: title.into(),
            view,
            decoration: None,
        };
        CommandModel {
            project: Some("web".into()),
            zen_on: false,
            zen_shortcut: None,
            views: vec![
                view("view:orch", "Orchestrator", View::Session("orch".into())),
                view("view:tasks", "Issues", View::Issues),
                view("view:machines", "Machines", View::Machines),
            ],
            workspaces: vec![WorkspaceItem {
                name: "alpha".into(),
                subtitle: None,
                decoration: None,
            }],
            reviews: vec![ReviewItem {
                task_id: "T-1".into(),
                title: "Fix the thing".into(),
                subtitle: None,
                decoration: None,
            }],
            other_projects: vec![ProjectItem {
                slug: "api".into(),
                label: "API".into(),
            }],
            edit_targets: vec![EditItem {
                target: EditTarget::Project,
                title: "Edit Project Settings".into(),
            }],
            other_clients_attached: true,
            multi_window: true,
        }
    }

    fn find<'a>(tree: &'a MenuTree, title: &str) -> &'a MenuItemSpec {
        tree.items()
            .into_iter()
            .find(|i| i.title == title)
            .unwrap_or_else(|| panic!("no item {title:?}"))
    }

    fn titles(entries: &[MenuEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                MenuEntry::Item(i) => i.title.clone(),
                MenuEntry::Submenu { title, .. } => format!("{title} ▸"),
                MenuEntry::Separator => "-".into(),
                MenuEntry::Services => "Services ▸".into(),
            })
            .collect()
    }

    #[test]
    fn the_bar_has_the_mac_menus_in_order() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let names: Vec<&str> = tree.menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(names, ["Shelbi", "File", "Edit", "View", "Go", "Window", "Help"]);
    }

    #[test]
    fn every_available_command_is_in_a_menu_and_enabled() {
        let model = fixture();
        let tree = build(&model, &defaults(), WindowFacts::default());
        let in_menus: HashSet<String> = tree
            .items()
            .into_iter()
            .filter(|i| i.enabled)
            .filter_map(|i| match &i.action {
                MenuAction::Command(id) => Some(id.clone()),
                MenuAction::App(_) => None,
            })
            .collect();
        for c in CommandRegistry::new().commands(&model) {
            assert!(in_menus.contains(c.id()), "{} ({}) has no menu item", c.id(), c.title());
        }
        // And every command item names a command the registry can run.
        for item in tree.items() {
            if let MenuAction::Command(id) = &item.action {
                assert!(CommandKind::from_id(id).is_some(), "{id}");
            }
        }
    }

    #[test]
    fn unavailable_fixed_commands_are_disabled_not_dropped() {
        // No project open, no other client: quit-project and detach-others
        // leave the registry, so their items go gray.
        let model = CommandModel {
            project: None,
            other_clients_attached: false,
            ..fixture()
        };
        let tree = build(&model, &defaults(), WindowFacts::default());
        assert!(!find(&tree, "Quit Project…").enabled);
        assert!(!find(&tree, "Detach Other Clients").enabled);
        assert!(find(&tree, "Quit Shelbi and Stop All Agents…").enabled);
        assert!(find(&tree, "Detach").enabled);
    }

    #[test]
    fn the_app_menu_keeps_agents_safe_from_a_stray_chord() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let shelbi = tree.menu("Shelbi").unwrap();
        assert_eq!(
            titles(&shelbi.items),
            [
                "About Shelbi",
                "-",
                "Settings…",
                "-",
                "Services ▸",
                "-",
                "Detach",
                "Detach Other Clients",
                "-",
                "Quit Project…",
                "Quit Shelbi and Stop All Agents…",
                "-",
                "Hide Shelbi",
                "Hide Others",
                "Show All",
                "-",
                "Quit Shelbi",
            ]
        );
        // ⌘Q closes the client; the commands that stop agents carry no key.
        assert_eq!(
            find(&tree, "Quit Shelbi").action,
            MenuAction::App(AppCommand::QuitClient)
        );
        assert_eq!(find(&tree, "Quit Shelbi").shortcut.as_ref().unwrap().display, "⌘Q");
        assert_eq!(find(&tree, "Quit Project…").shortcut, None);
        assert_eq!(find(&tree, "Quit Shelbi and Stop All Agents…").shortcut, None);
        assert_eq!(find(&tree, "Settings…").shortcut.as_ref().unwrap().keystroke, "cmd-,");
    }

    #[test]
    fn go_lists_views_workspaces_and_reviews() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let go = tree.menu("Go").unwrap();
        assert_eq!(
            titles(&go.items),
            ["Orchestrator", "Issues", "Machines", "-", "Workspaces ▸", "Reviews ▸"]
        );
        let MenuEntry::Submenu { items: reviews, .. } = &go.items[5] else {
            panic!("reviews submenu");
        };
        let MenuEntry::Submenu { title, items: task } = &reviews[0] else {
            panic!("task submenu");
        };
        assert_eq!(title, "Fix the thing");
        assert_eq!(titles(task), ["Open Review", "Approve", "Reject…"]);
        let ids: Vec<&MenuAction> = items(task).iter().map(|i| &i.action).collect();
        assert_eq!(
            ids,
            [
                &MenuAction::Command("review:T-1".into()),
                &MenuAction::Command("action:approve-review:T-1".into()),
                &MenuAction::Command("action:reject-review:T-1".into()),
            ]
        );
    }

    #[test]
    fn empty_families_show_a_disabled_placeholder() {
        let model = CommandModel {
            workspaces: vec![],
            reviews: vec![],
            other_projects: vec![],
            edit_targets: vec![],
            ..fixture()
        };
        let tree = build(&model, &defaults(), WindowFacts::default());
        for placeholder in ["No Workspaces", "No Reviews", "No Other Projects", "Nothing to Edit"] {
            assert!(!find(&tree, placeholder).enabled, "{placeholder}");
        }
    }

    #[test]
    fn file_opens_a_new_window_on_cmd_shift_n() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let file = tree.menu("File").unwrap();
        assert_eq!(
            titles(&file.items)[..4],
            ["New Window", "-", "Add Project…", "Switch Project…"]
        );
        let item = find(&tree, "New Window");
        assert!(item.enabled);
        assert_eq!(item.action, MenuAction::Command("action:new-window".into()));
        let shortcut = item.shortcut.as_ref().unwrap();
        assert_eq!((shortcut.keystroke.as_str(), shortcut.display.as_str()), ("shift-cmd-n", "⇧⌘N"));
    }

    #[test]
    fn file_names_projects_by_label() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let item = find(&tree, "API");
        assert_eq!(item.action, MenuAction::Command("action:switch-project:api".into()));
        assert!(find(&tree, "Edit Project Settings").enabled);
    }

    #[test]
    fn keys_yaml_chords_become_key_equivalents() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let shortcut = |title: &str| find(&tree, title).shortcut.clone().map(|s| s.display);
        assert_eq!(shortcut("Command Palette").as_deref(), Some("⌃P"));
        assert_eq!(shortcut("Focus Sidebar").as_deref(), Some("⌃H"));
        assert_eq!(shortcut("Focus Main Area").as_deref(), Some("⌃L"));
        assert_eq!(shortcut("Turn Zen Mode On").as_deref(), Some("⌥Z"));
        assert_eq!(find(&tree, "Turn Zen Mode On").shortcut.as_ref().unwrap().keystroke, "alt-z");

        // A rebinding in keys.yaml moves the key equivalent with it.
        let custom = keymaps("defaults:\n  global:\n    zen_toggle: ctrl-alt-shift-z\n    open_palette: super-shift-p\n");
        let tree = build(&fixture(), &custom, WindowFacts::default());
        let item = find(&tree, "Turn Zen Mode On");
        assert_eq!(item.shortcut.as_ref().unwrap().display, "⌃⌥⇧Z");
        assert_eq!(item.shortcut.as_ref().unwrap().keystroke, "ctrl-alt-shift-z");
        let palette = find(&tree, "Command Palette").shortcut.clone().unwrap();
        assert_eq!((palette.keystroke.as_str(), palette.display.as_str()), ("shift-cmd-p", "⇧⌘P"));
    }

    #[test]
    fn view_has_the_terminal_font_zoom_beside_the_window_zoom() {
        let tree = build(&fixture(), &defaults(), WindowFacts::default());
        let view = tree.menu("View").unwrap();
        assert_eq!(
            titles(&view.items),
            [
                "Command Palette",
                "Hide Sidebar",
                "-",
                "Focus Sidebar",
                "Focus Main Area",
                "-",
                "Bigger",
                "Smaller",
                "Actual Size",
                "-",
                "Turn Zen Mode On",
                "Error Log",
            ]
        );
        for (title, command, keystroke, display) in [
            ("Bigger", AppCommand::FontBigger, "cmd-+", "⌘+"),
            ("Smaller", AppCommand::FontSmaller, "cmd--", "⌘-"),
            ("Actual Size", AppCommand::FontActualSize, "cmd-0", "⌘0"),
        ] {
            let item = find(&tree, title);
            assert!(item.enabled, "{title}");
            assert_eq!(item.action, MenuAction::App(command));
            let shortcut = item.shortcut.as_ref().unwrap();
            assert_eq!(
                (shortcut.keystroke.as_str(), shortcut.display.as_str()),
                (keystroke, display)
            );
        }
        // Window ▸ Zoom stays the window zoom.
        assert_eq!(find(&tree, "Zoom").action, MenuAction::App(AppCommand::Zoom));
        assert_eq!(titles(&tree.menu("Window").unwrap().items)[..2], ["Minimize", "Zoom"]);
    }

    #[test]
    fn zen_and_sidebar_titles_follow_state() {
        let on = CommandModel {
            zen_on: true,
            ..fixture()
        };
        let tree = build(&on, &defaults(), WindowFacts { sidebar_collapsed: true });
        assert!(find(&tree, "Turn Zen Mode Off").enabled);
        assert!(find(&tree, "Show Sidebar").enabled);
    }

    #[test]
    fn menu_shortcuts_need_a_modifier_and_a_free_chord() {
        let chord = |s: &str| KeyChord::parse(s).unwrap();
        // Bare keys would steal typing.
        assert_eq!(menu_shortcut(&chord("q")), None);
        assert_eq!(menu_shortcut(&chord("shift-j")), None);
        assert_eq!(menu_shortcut(&chord("enter")), None);
        // Function keys stand alone.
        assert_eq!(menu_shortcut(&chord("f6")).unwrap().keystroke, "f6");
        // macOS's own.
        assert_eq!(menu_shortcut(&chord("super-w")), None);
        assert_eq!(menu_shortcut(&chord("ctrl-space")), None);

        let rendered = |s: &str| {
            let sc = menu_shortcut(&chord(s)).unwrap();
            (sc.keystroke, sc.display)
        };
        assert_eq!(rendered("ctrl-p"), ("ctrl-p".into(), "⌃P".into()));
        assert_eq!(rendered("alt-z"), ("alt-z".into(), "⌥Z".into()));
        assert_eq!(rendered("super-k"), ("cmd-k".into(), "⌘K".into()));
        assert_eq!(rendered("ctrl-alt-shift-super-x"), ("ctrl-alt-shift-cmd-x".into(), "⌃⌥⇧⌘X".into()));
        assert_eq!(rendered("ctrl-shift-up"), ("ctrl-shift-up".into(), "⌃⇧↑".into()));
        assert_eq!(rendered("alt-page-down"), ("alt-pagedown".into(), "⌥⇟".into()));
        assert_eq!(rendered("ctrl-esc"), ("ctrl-escape".into(), "⌃Esc".into()));
    }

    #[test]
    fn fixed_shortcuts_render_as_the_mac_standards() {
        let display = |c| fixed_shortcut(c).map(|s| s.display);
        assert_eq!(display(AppCommand::Settings).as_deref(), Some("⌘,"));
        assert_eq!(display(AppCommand::QuitClient).as_deref(), Some("⌘Q"));
        assert_eq!(display(AppCommand::HideOthers).as_deref(), Some("⌥⌘H"));
        assert_eq!(display(AppCommand::ToggleSidebar).as_deref(), Some("⌃⌘S"));
        assert_eq!(fixed_shortcut(AppCommand::ToggleSidebar).unwrap().keystroke, "ctrl-cmd-s");
        assert_eq!(display(AppCommand::Zoom), None);
    }

    #[test]
    fn the_default_keymap_has_no_reserved_conflicts() {
        assert_eq!(reserved_conflicts(&defaults()), []);
    }

    #[test]
    fn a_reserved_rebinding_is_reported_and_kept_off_the_menu() {
        let km = keymaps("defaults:\n  global:\n    open_palette: super-w\n  kanban:\n    refresh: [r, super-space]\n");
        let conflicts = reserved_conflicts(&km);
        assert_eq!(
            conflicts
                .iter()
                .map(|c| (c.mode, c.action, c.chord.as_str(), c.reserved_by))
                .collect::<Vec<_>>(),
            [
                ("global", "open_palette", "⌘W", "Close Window"),
                ("kanban", "refresh", "⌘Space", "Spotlight"),
            ]
        );
        assert_eq!(
            conflicts[0].message(),
            "global.open_palette is bound to ⌘W, which macOS uses for Close Window: it won't reach Shelbi."
        );
        let tree = build(&fixture(), &km, WindowFacts::default());
        assert_eq!(find(&tree, "Command Palette").shortcut, None);
    }

    #[test]
    fn placement_is_by_command_family() {
        let s = String::new;
        assert_eq!(place(&CommandKind::ShowView(View::Activity)), Placement::GoView);
        assert_eq!(place(&CommandKind::ToggleZen), Placement::View);
        assert_eq!(place(&CommandKind::QuitShelbi), Placement::App);
        assert_eq!(place(&CommandKind::AddProject), Placement::File);
        assert_eq!(place(&CommandKind::NewWindow), Placement::File);
        assert_eq!(place(&CommandKind::ApproveReview { id: s() }), Placement::GoReview);
        assert_eq!(place(&CommandKind::StartIssue { id: s() }), Placement::Board);
    }
}
