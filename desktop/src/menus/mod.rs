//! The native macOS menu bar.
//!
//! [`tree`] builds the menus as plain data from the active window's command
//! model and keymaps; this module turns a [`MenuTree`] into gpui menus and
//! owns the actions their items dispatch.
//!
//! - An enabled item dispatches [`MenuInvoke`] (a registry command id or an
//!   [`AppCommand`]) to the active window, which runs it the way the palette
//!   does. Items whose action has a standing binding (⌘, ⌘W ⌘Q ⌘C ...) use
//!   that action instead, so the binding is the item's key equivalent.
//! - A disabled item dispatches [`MenuUnavailable`], which nothing handles:
//!   gpui validates menu items by whether their action has a handler on the
//!   focused path, so the item is drawn gray.
//! - A `keys.yaml` shortcut becomes a key equivalent through a binding gpui's
//!   menu builder can find. It is bound in a key context no element has
//!   ([`MENU_LABEL_CONTEXT`]), so it never fires from the keyboard: the keys
//!   themselves stay with the window's own `keys.yaml` handling, which takes
//!   them first (a key the window handles never reaches the menu). gpui can't
//!   remove a binding, so each new set of shortcuts is bound under a fresh
//!   [`MenuInvoke::generation`]; the old ones match no item and stay inert.

pub mod tree;

use gpui::{actions, App, Global, KeyBinding, Menu, MenuItem, SystemMenuType};

use crate::app::{CloseWindow, OpenSettings, ToggleSidebar};
use crate::elements::terminal::{FontZoom, TerminalSettings};
use tree::{AppCommand, MenuAction, MenuEntry, MenuTree, FIXED_SHORTCUTS};

actions!(
    shelbi_menu,
    [
        /// A disabled menu item: no element handles it, so it is drawn gray.
        MenuUnavailable,
        /// Hide the app (⌘H).
        Hide,
        /// Hide every other app (⌥⌘H).
        HideOthers,
        /// Show every app.
        ShowAll,
        /// Minimize the window (⌘M).
        Minimize,
        /// Zoom the window.
        Zoom,
        /// Edit ▸ Cut (⌘X).
        Cut,
        /// Edit ▸ Copy (⌘C): the focused terminal's selection, an overlay's text.
        Copy,
        /// Edit ▸ Paste (⌘V): into the focused terminal or overlay.
        Paste,
        /// Edit ▸ Select All (⌘A).
        SelectAll,
        /// Close this client (⌘Q): every window; agents and the daemon keep
        /// running. The registry's Quit Shelbi (stop everything) is separate.
        Quit,
    ]
);

/// Run a menu item: a registry command or a client action.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = shelbi_menu, no_json)]
pub struct MenuInvoke {
    pub target: MenuAction,
    /// The shortcut set this item was built with (see the module docs).
    pub generation: u64,
}

/// The key context the `keys.yaml` menu-label bindings are bound in. No
/// element declares it, so the bindings only label menu items.
const MENU_LABEL_CONTEXT: &str = "ShelbiMenuLabel";

/// The key context the terminal element declares; the Edit shortcuts act on
/// it.
const TERMINAL_CONTEXT: &str = "Terminal";

/// The menu bar last installed.
#[derive(Default)]
struct MenuBar {
    tree: Option<MenuTree>,
    /// The `keys.yaml` shortcuts bound for the current generation.
    shortcuts: Vec<(String, MenuAction)>,
    generation: u64,
}

impl Global for MenuBar {}

/// Bind the macOS-standard shortcuts and the app-level handlers. Called once
/// at startup, before the first [`install`].
pub fn init(cx: &mut App) {
    let binding = |command: AppCommand, context: Option<&str>| {
        let (_, spec) = FIXED_SHORTCUTS.iter().find(|(c, _)| *c == command)?;
        let chord = shelbi_state::keymap::KeyChord::parse(spec).ok()?;
        let keys = tree::keystroke(&chord)?;
        Some(match command {
            AppCommand::Settings => KeyBinding::new(&keys, OpenSettings, context),
            AppCommand::Hide => KeyBinding::new(&keys, Hide, context),
            AppCommand::HideOthers => KeyBinding::new(&keys, HideOthers, context),
            AppCommand::QuitClient => KeyBinding::new(&keys, Quit, context),
            AppCommand::CloseWindow => KeyBinding::new(&keys, CloseWindow, context),
            AppCommand::Minimize => KeyBinding::new(&keys, Minimize, context),
            AppCommand::ToggleSidebar => KeyBinding::new(&keys, ToggleSidebar, context),
            AppCommand::Cut => KeyBinding::new(&keys, Cut, context),
            AppCommand::Copy => KeyBinding::new(&keys, Copy, context),
            AppCommand::Paste => KeyBinding::new(&keys, Paste, context),
            AppCommand::SelectAll => KeyBinding::new(&keys, SelectAll, context),
            _ => return None,
        })
    };
    let bindings: Vec<KeyBinding> = FIXED_SHORTCUTS
        .iter()
        .filter_map(|(command, _)| {
            // The Edit keys act on a focused terminal; elsewhere (gpui-kit's
            // text inputs, the overlays) the focused element's own handling
            // keeps them.
            let context = matches!(
                command,
                AppCommand::Cut | AppCommand::Copy | AppCommand::Paste | AppCommand::SelectAll
            )
            .then_some(TERMINAL_CONTEXT);
            binding(*command, context)
        })
        .collect();
    cx.bind_keys(bindings);

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // The client actions that need no window. A window handles every
    // `MenuInvoke` first (`ShelbiWindow::menu_invoke`) and hands these back
    // to [`run_app_command`].
    cx.on_action(|invoke: &MenuInvoke, cx| {
        if let MenuAction::App(command) = &invoke.target {
            run_app_command(*command, cx);
        }
    });
    cx.set_global(MenuBar::default());
}

/// Run a client action that needs no window. Returns false for one that does
/// (the window runs those itself).
pub fn run_app_command(command: AppCommand, cx: &mut App) -> bool {
    match command {
        AppCommand::Hide => cx.hide(),
        AppCommand::HideOthers => cx.hide_other_apps(),
        AppCommand::ShowAll => cx.unhide_other_apps(),
        AppCommand::QuitClient => cx.quit(),
        AppCommand::About => crate::views::about::show_about(cx),
        AppCommand::Documentation => cx.open_url(tree::DOCS_URL),
        AppCommand::ReportIssue => cx.open_url(tree::ISSUES_URL),
        // The terminal font is app-wide: every window's terminals follow.
        AppCommand::FontBigger => {
            TerminalSettings::zoom(FontZoom::Bigger, cx);
        }
        AppCommand::FontSmaller => {
            TerminalSettings::zoom(FontZoom::Smaller, cx);
        }
        AppCommand::FontActualSize => {
            TerminalSettings::zoom(FontZoom::ActualSize, cx);
        }
        AppCommand::Settings
        | AppCommand::SendFeedback
        | AppCommand::CloseWindow
        | AppCommand::Minimize
        | AppCommand::Zoom
        | AppCommand::ToggleSidebar
        | AppCommand::CommandPalette
        | AppCommand::FocusSidebar
        | AppCommand::FocusMain
        | AppCommand::Cut
        | AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::SelectAll => return false,
    }
    true
}

/// Make `tree` the menu bar. A tree equal to the installed one is a no-op, so
/// callers can offer one on every model change.
pub fn install(tree: MenuTree, cx: &mut App) {
    if !cx.has_global::<MenuBar>() {
        cx.set_global(MenuBar::default());
    }
    if cx.global::<MenuBar>().tree.as_ref() == Some(&tree) {
        return;
    }
    let shortcuts: Vec<(String, MenuAction)> = tree
        .items()
        .into_iter()
        .filter(|item| item.enabled && uses_invoke(&item.action))
        .filter_map(|item| Some((item.shortcut.clone()?.keystroke, item.action.clone())))
        .collect();
    let bar = cx.global_mut::<MenuBar>();
    if bar.shortcuts != shortcuts {
        bar.generation += 1;
        bar.shortcuts = shortcuts.clone();
        let generation = bar.generation;
        cx.bind_keys(shortcuts.into_iter().map(|(keys, target)| {
            KeyBinding::new(
                &keys,
                MenuInvoke { target, generation },
                Some(MENU_LABEL_CONTEXT),
            )
        }));
    }
    let generation = cx.global::<MenuBar>().generation;
    cx.set_menus(tree.menus.iter().map(|m| {
        Menu::new(m.title.clone()).items(m.items.iter().map(|e| entry(e, generation)))
    }));
    cx.global_mut::<MenuBar>().tree = Some(tree);
}

/// Whether an item dispatches [`MenuInvoke`] (rather than an action with its
/// own standing binding).
fn uses_invoke(action: &MenuAction) -> bool {
    match action {
        MenuAction::Command(_) => true,
        MenuAction::App(command) => !FIXED_SHORTCUTS.iter().any(|(c, _)| c == command)
            && !matches!(command, AppCommand::ShowAll | AppCommand::Zoom),
    }
}

fn entry(entry: &MenuEntry, generation: u64) -> MenuItem {
    match entry {
        MenuEntry::Separator => MenuItem::separator(),
        MenuEntry::Services => MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuEntry::Submenu { title, items } => MenuItem::submenu(
            Menu::new(title.clone()).items(items.iter().map(|e| self::entry(e, generation))),
        ),
        MenuEntry::Item(item) => {
            let action: Box<dyn gpui::Action> = if !item.enabled {
                Box::new(MenuUnavailable)
            } else {
                match &item.action {
                    MenuAction::App(AppCommand::Settings) => Box::new(OpenSettings),
                    MenuAction::App(AppCommand::Hide) => Box::new(Hide),
                    MenuAction::App(AppCommand::HideOthers) => Box::new(HideOthers),
                    MenuAction::App(AppCommand::ShowAll) => Box::new(ShowAll),
                    MenuAction::App(AppCommand::QuitClient) => Box::new(Quit),
                    MenuAction::App(AppCommand::CloseWindow) => Box::new(CloseWindow),
                    MenuAction::App(AppCommand::Minimize) => Box::new(Minimize),
                    MenuAction::App(AppCommand::Zoom) => Box::new(Zoom),
                    MenuAction::App(AppCommand::ToggleSidebar) => Box::new(ToggleSidebar),
                    MenuAction::App(AppCommand::Cut) => Box::new(Cut),
                    MenuAction::App(AppCommand::Copy) => Box::new(Copy),
                    MenuAction::App(AppCommand::Paste) => Box::new(Paste),
                    MenuAction::App(AppCommand::SelectAll) => Box::new(SelectAll),
                    target => Box::new(MenuInvoke {
                        target: target.clone(),
                        generation,
                    }),
                }
            };
            MenuItem::Action {
                name: item.title.clone().into(),
                action,
                os_action: None,
                checked: item.checked,
                disabled: !item.enabled,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::KeyChord;

    #[test]
    fn every_menu_keystroke_parses_as_a_gpui_keystroke() {
        let mut specs: Vec<&str> = FIXED_SHORTCUTS.iter().map(|(_, s)| *s).collect();
        specs.extend([
            "ctrl-p", "alt-z", "ctrl-h", "ctrl-l", "super-k", "f6", "ctrl-shift-up",
            "alt-page-down", "ctrl-esc", "ctrl-space", "ctrl-alt-shift-super-x", "super-.",
            "ctrl-/", "ctrl-enter", "alt-tab", "super-+", "super--", "super-0", "-",
            "ctrl--",
        ]);
        for spec in specs {
            let chord = KeyChord::parse(spec).unwrap();
            let keys = tree::keystroke(&chord).unwrap();
            let parsed = gpui::Keystroke::parse(&keys)
                .unwrap_or_else(|e| panic!("{spec} -> {keys}: {e:?}"));
            // And it reads back as the same chord.
            assert_eq!(
                crate::keys::chord_from_keystroke(&parsed).map(|c| c.canonical()),
                Some(chord.canonical()),
                "{spec} -> {keys}"
            );
        }
    }

    #[test]
    fn only_items_without_a_standing_binding_dispatch_menu_invoke() {
        assert!(uses_invoke(&MenuAction::Command("action:toggle-zen".into())));
        assert!(uses_invoke(&MenuAction::App(AppCommand::CommandPalette)));
        assert!(uses_invoke(&MenuAction::App(AppCommand::Documentation)));
        assert!(uses_invoke(&MenuAction::App(AppCommand::FontBigger)));
        assert!(!uses_invoke(&MenuAction::App(AppCommand::Settings)));
        assert!(!uses_invoke(&MenuAction::App(AppCommand::Copy)));
    }
}
