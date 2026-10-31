//! Settings ▸ General's Editor and Diff tool pickers: the Select (the
//! Button with the choice and a chevron, as the workspace dialog's Machine
//! picker), opening a menu of a [`ToolPicker`]'s entries under it.
//!
//! The menu lists the default (with what it runs now), the installed
//! command-line tools (with their command), the installed apps (with their
//! icons, as Finder shows them), the current setting when it is none of
//! those, then Browse… and Other command…, a check on the current one.
//! ↑ / ↓ move through it, Return picks and Escape closes; Return, Space or ↓
//! on the closed button opens it. A pick is announced as
//! [`ToolSelectEvent::Picked`]; the settings view carries it out through the
//! shared model (`shelbi_app::settings::general::GeneralForm`), which owns
//! the entries, the current choice and the save.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    deferred, div, img, prelude::*, px, AnyElement, Context, Div, EventEmitter, FocusHandle, Focusable, Image,
    KeyDownEvent, SharedString, Window,
};
use shelbi_app::settings::general::{PickerEntry, ToolPicker, ToolSetting};
use shelbi_state::tools::ToolOption;
use shelbi_state::ToolKind;

use crate::assets::{heroicon, icons, ICON_SIZE};
use crate::elements::controls;
use crate::elements::placed::{placed, MenuScroll};
use crate::theme::style::{self, Anchor, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// How tall the open menu grows before it scrolls (less where the window
/// is short of room: [`placed`]).
const MENU_MAX_H: f32 = 420.0;

/// The apps' icons, by bundle path, as the pickers draw them.
pub type AppIcons = HashMap<PathBuf, Arc<Image>>;

/// An app icon's size in a row and on the button.
const APP_ICON: f32 = 16.0;

/// The pixels an app icon is drawn at (2x for Retina).
pub const APP_ICON_PX: u32 = 32;

/// What the picker tells its view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelectEvent {
    /// An entry was chosen from the menu.
    Picked(PickerEntry),
}

/// One tool setting's picker.
pub struct ToolSelect {
    setting: ToolSetting,
    picker: ToolPicker,
    icons: AppIcons,
    /// Whether it takes a pick (the config is read, no save in flight).
    enabled: bool,
    open: bool,
    /// The keyboard's row, an index into the picker's entries.
    highlight: usize,
    focus: FocusHandle,
    scroll: MenuScroll,
    /// Where the button last painted, so a press on it toggles rather than
    /// counting as a press outside the open menu.
    button: Anchor,
}

impl EventEmitter<ToolSelectEvent> for ToolSelect {}

impl Focusable for ToolSelect {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The element ids' prefix for `setting`'s picker.
pub fn selector(setting: ToolSetting) -> &'static str {
    match setting {
        ToolSetting::Editor => "settings-editor",
        ToolSetting::DiffTool => "settings-diff-tool",
    }
}

impl ToolSelect {
    pub fn new(setting: ToolSetting, picker: ToolPicker, cx: &mut Context<Self>) -> Self {
        ToolSelect {
            setting,
            picker,
            icons: AppIcons::new(),
            enabled: false,
            open: false,
            highlight: 0,
            focus: cx.focus_handle(),
            scroll: MenuScroll::default(),
            button: Anchor::default(),
        }
    }

    /// Show `picker` (the form changed: read, detected, saving, saved).
    pub fn set(&mut self, picker: ToolPicker, icons: &AppIcons, enabled: bool, cx: &mut Context<Self>) {
        let count = picker.entries().len();
        if self.picker != picker || self.enabled != enabled || self.icons.len() != icons.len() {
            self.picker = picker;
            self.icons = icons.clone();
            self.enabled = enabled;
            self.highlight = self.highlight.min(count.saturating_sub(1));
            if !enabled {
                self.open = false;
            }
            cx.notify();
        }
    }

    /// The picker shown.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn picker(&self) -> &ToolPicker {
        &self.picker
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the menu, the current choice as the keyboard's row.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open || !self.enabled {
            return;
        }
        self.open = true;
        self.highlight = self.picker.selected();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Move the keyboard's row by `delta`, stopping at the ends.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.picker.entries().len();
        self.highlight = (self.highlight as isize + delta).clamp(0, count as isize - 1) as usize;
        cx.notify();
    }

    /// Choose the entry at `ix`: announced, and the menu closed.
    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.picker.entries().into_iter().nth(ix) else {
            return;
        };
        self.open = false;
        cx.emit(ToolSelectEvent::Picked(entry));
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if !self.open {
            if matches!(key, "enter" | "space" | "down") {
                self.open(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        match key {
            "up" => self.step(-1, cx),
            "down" => self.step(1, cx),
            "enter" | "space" => self.pick(self.highlight, cx),
            "escape" => self.close(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn icon(&self, tool: &ToolOption) -> Option<Arc<Image>> {
        tool.app_path.as_ref().and_then(|p| self.icons.get(p)).cloned()
    }

    /// The current choice's icon, for the button.
    fn current_icon(&self) -> Option<Arc<Image>> {
        match self.picker.entries().into_iter().nth(self.picker.selected()) {
            Some(PickerEntry::Tool(t)) => self.icon(&t),
            _ => None,
        }
    }

    /// What the menu shows before entry `ix`: a group's heading, the rule
    /// before Browse…, or nothing.
    fn lead(&self, ix: usize, entry: &PickerEntry) -> Option<Lead> {
        let custom_at = 1 + self.picker.cli.len() + self.picker.apps.len();
        match entry {
            PickerEntry::Tool(_) if ix == 1 && !self.picker.cli.is_empty() => Some(Lead::Heading("Command line")),
            PickerEntry::Tool(_) if ix == 1 + self.picker.cli.len() && !self.picker.apps.is_empty() => {
                Some(Lead::Heading("Applications"))
            }
            PickerEntry::Tool(_) if ix == custom_at => Some(Lead::Heading("Your setting")),
            PickerEntry::Browse => Some(Lead::Rule),
            _ => None,
        }
    }

    /// Entry `ix`'s place among the menu's children (its headings and rule
    /// count too), for scrolling it into view.
    fn child_of(&self, ix: usize) -> usize {
        let leads = self
            .picker
            .entries()
            .iter()
            .enumerate()
            .take(ix + 1)
            .filter(|(i, e)| self.lead(*i, e).is_some())
            .count();
        ix + leads
    }

    /// The menu's rows: the entries in order, with the group headings and
    /// the rule before Browse….
    fn render_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let prefix = selector(self.setting);
        let selected = self.picker.selected();
        let mut rows = Vec::new();
        for (ix, entry) in self.picker.entries().into_iter().enumerate() {
            match self.lead(ix, &entry) {
                Some(Lead::Heading(text)) => rows.push(group_heading(text, theme)),
                Some(Lead::Rule) => rows.push(rule(theme)),
                None => {}
            }
            let (icon, detail) = match &entry {
                PickerEntry::Default => (None, self.picker.default_detail.clone()),
                PickerEntry::Tool(t) => match t.kind {
                    ToolKind::App => (Some(self.icon(t)), None),
                    ToolKind::Cli => (None, t.choice.command().map(str::to_string)),
                },
                _ => (None, None),
            };
            let label = entry.label(self.setting);
            let row = style::menu_row(SharedString::from(format!("{prefix}-option-{ix}")), ix == self.highlight, theme)
                .flex_none()
                .children(icon.map(|icon| app_icon(icon, theme)))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .children(detail.map(|d| {
                    div()
                        .flex_none()
                        .max_w(px(180.0))
                        .truncate()
                        .type_style(Type::XSmall)
                        .text_color(theme.form_hint)
                        .child(d)
                }))
                .child(div().w(ICON_SIZE).flex_none().when(ix == selected, |d| {
                    d.child(heroicon(icons::CHECK).text_color(theme.accent))
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.pick(ix, cx)
                }));
            rows.push(row.into_any_element());
        }
        rows
    }
}

/// What goes before an entry in the menu.
enum Lead {
    Heading(&'static str),
    Rule,
}

/// A menu group's heading (not choosable).
fn group_heading(text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .px_2()
        .pt_2()
        .pb_1()
        .type_style(Type::XSmall)
        .text_color(theme.form_hint)
        .child(text)
        .into_any_element()
}

/// The rule before Browse….
fn rule(theme: &Theme) -> AnyElement {
    div().flex_none().h(px(1.0)).mx_2().my_1().bg(theme.divider).into_any_element()
}

/// An app's icon in a row or on the button; an empty box of its size while
/// it is being read.
fn app_icon(icon: Option<Arc<Image>>, theme: &Theme) -> Div {
    let d = div().flex_none().size(px(APP_ICON));
    match icon {
        Some(icon) => d.child(img(icon).size(px(APP_ICON))),
        None => d.rounded(px(4.0)).bg(theme.divider),
    }
}


impl Render for ToolSelect {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let prefix = selector(self.setting);
        let focused = self.focus.is_focused(window);
        let label = self.picker.current_label();
        let chevron = style::button_colors(ButtonKind::Secondary, &theme).icon;
        let button_id = SharedString::from(format!("{prefix}-button"));
        let button = if self.enabled {
            style::button_base(button_id, ButtonKind::Secondary, ButtonSize::Base, focused || self.open, &theme)
                .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
        } else {
            style::disabled_button_base(button_id, ButtonKind::Secondary, ButtonSize::Base, &theme)
        };
        let icon = self.current_icon();
        let button = button
            .w(px(controls::CONTROL_W))
            .justify_between()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .children(icon.map(|i| style::left_slot(ButtonSize::Base, app_icon(Some(i), &theme))))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(style::dropdown_slot(ButtonSize::Base, chevron))
            .child(style::anchor_probe(&self.button));

        // The menu floats 4px under the button (or over it where the window
        // is short of room below), its width, drawn after everything
        // (`deferred`) so nothing below it moves. The keyboard's row stays
        // in view as it moves, and is scrolled to on opening.
        self.scroll.follow(self.open.then(|| self.child_of(self.highlight)));
        let anchor = self.button.clone();
        let menu = self.open.then(|| {
            let rows = self.render_rows(&theme, cx);
            let list = style::popover(
                div()
                    .id(SharedString::from(format!("{prefix}-menu")))
                    .debug_selector(move || format!("{prefix}-menu"))
                    .flex()
                    .flex_col()
                    .w(px(controls::CONTROL_W))
                    .max_h(px(MENU_MAX_H))
                    .overflow_y_scroll()
                    .track_scroll(self.scroll.handle())
                    .p_1()
                    .occlude(),
                &theme,
            )
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                // A press on the button toggles it closed through its click.
                if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                    return;
                }
                this.close(cx);
            }))
            .children(rows);
            deferred(placed(&self.button, list)).with_priority(1)
        });
        div().relative().flex_none().child(button).children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext, WindowAppearance};
    use shelbi_app::settings::general::{GeneralForm, Preferences};
    use shelbi_state::tools::DetectedTools;
    use shelbi_state::{ThemePreference, ToolChoice};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn option(name: &str, choice: ToolChoice) -> ToolOption {
        ToolOption { name: name.into(), ..ToolOption::custom(&choice) }
    }

    pub(crate) fn detected() -> DetectedTools {
        DetectedTools {
            editors: vec![
                option("Neovim", ToolChoice::Command("nvim".into())),
                option("Zed", ToolChoice::app("/Applications/Zed.app")),
            ],
            diff_tools: vec![],
            git_difftool: Some("vimdiff".into()),
        }
    }

    /// The theme and widgets set up, and the Editor's picker as detected.
    fn editor_picker(cx: &mut TestAppContext) -> ToolPicker {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let mut form = GeneralForm::default();
        form.load(Ok(Preferences::default()), None);
        form.set_detected(detected());
        form.picker(ToolSetting::Editor)
    }

    fn select(cx: &mut TestAppContext) -> (Entity<ToolSelect>, VisualTestContext, Rc<RefCell<Vec<PickerEntry>>>) {
        let picker = editor_picker(cx);
        let window = cx.add_window(|_, cx| {
            let mut s = ToolSelect::new(ToolSetting::Editor, picker.clone(), cx);
            s.set(picker, &AppIcons::new(), true, cx);
            s
        });
        let select = window.root(cx).unwrap();
        let picked = Rc::new(RefCell::new(Vec::new()));
        let sink = picked.clone();
        cx.update(|cx| {
            cx.subscribe(&select, move |_, event: &ToolSelectEvent, _| match event {
                ToolSelectEvent::Picked(e) => sink.borrow_mut().push(e.clone()),
            })
            .detach()
        });
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        (select, vcx, picked)
    }

    #[gpui::test]
    fn the_keyboard_opens_moves_and_picks(cx: &mut TestAppContext) {
        let (select, mut cx, picked) = select(cx);
        select.update_in(&mut cx, |s, window, cx| s.open(window, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-editor-menu").is_some(), "the menu draws");
        // Default is current: ↓ moves to Neovim, ↓ again to Zed.
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        assert_eq!(
            *picked.borrow(),
            [PickerEntry::Tool(option("Zed", ToolChoice::app("/Applications/Zed.app")))]
        );
        assert!(!select.read_with(&cx, |s, _| s.is_open()), "a pick closes the menu");
    }

    #[gpui::test]
    fn the_menu_lists_default_tools_apps_browse_and_other(cx: &mut TestAppContext) {
        let (select, mut cx, picked) = select(cx);
        select.update_in(&mut cx, |s, window, cx| s.open(window, cx));
        cx.run_until_parked();
        for row in ["0", "1", "2", "3", "4"].map(|ix| &*format!("settings-editor-option-{ix}").leak()) {
            assert!(cx.debug_bounds(row).is_some(), "{row} draws");
        }
        assert!(cx.debug_bounds("settings-editor-option-5").is_none());
        // Other command… is the last row; Escape closes without a pick.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(picked.borrow().is_empty());
        select.update_in(&mut cx, |s, window, cx| s.open(window, cx));
        cx.run_until_parked();
        let other = cx.debug_bounds("settings-editor-option-4").unwrap();
        cx.simulate_click(other.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(*picked.borrow(), [PickerEntry::OtherCommand]);
    }

    #[gpui::test]
    fn a_disabled_picker_doesnt_open(cx: &mut TestAppContext) {
        let (select, mut cx, _) = select(cx);
        select.update_in(&mut cx, |s, window, cx| {
            let picker = s.picker().clone();
            s.set(picker, &AppIcons::new(), false, cx);
            s.open(window, cx);
        });
        assert!(!select.read_with(&cx, |s, _| s.is_open()));
    }

    /// A picker `top` down a window `height` tall, as Settings ▸ General
    /// lays one out further down the page.
    struct Page {
        select: Entity<ToolSelect>,
        top: f32,
    }

    impl Render for Page {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().pt(px(self.top)).pl(px(16.0)).child(self.select.clone())
        }
    }

    fn on_page(cx: &mut TestAppContext, top: f32, height: f32) -> (Entity<ToolSelect>, VisualTestContext) {
        let picker = editor_picker(cx);
        let window = cx.add_window(|_, cx| {
            let select = cx.new(|cx| {
                let mut s = ToolSelect::new(ToolSetting::Editor, picker.clone(), cx);
                s.set(picker, &AppIcons::new(), true, cx);
                s
            });
            Page { select, top }
        });
        let select = window.root(cx).unwrap().read_with(cx, |p, _| p.select.clone());
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.simulate_resize(gpui::size(px(800.0), px(height)));
        vcx.run_until_parked();
        select.update_in(&mut vcx, |s, window, cx| s.open(window, cx));
        vcx.run_until_parked();
        (select, vcx)
    }

    #[gpui::test]
    fn near_the_windows_bottom_the_menu_opens_above_its_button(cx: &mut TestAppContext) {
        let (_, mut cx) = on_page(cx, 520.0, 600.0);
        let button = cx.debug_bounds("settings-editor-button").unwrap();
        let menu = cx.debug_bounds("settings-editor-menu").unwrap();
        assert!(menu.bottom() <= button.top(), "above the button: {menu:?} over {button:?}");
        assert!(menu.top() >= px(8.0), "inside the window: {menu:?}");
    }

    #[gpui::test]
    fn near_the_windows_top_the_menu_still_opens_below(cx: &mut TestAppContext) {
        let (_, mut cx) = on_page(cx, 16.0, 600.0);
        let button = cx.debug_bounds("settings-editor-button").unwrap();
        let menu = cx.debug_bounds("settings-editor-menu").unwrap();
        assert_eq!(menu.top(), button.bottom() + px(4.0), "4pt under the button");
    }

    #[gpui::test]
    fn in_a_short_window_the_menu_scrolls_and_the_keyboard_keeps_its_row_in_view(cx: &mut TestAppContext) {
        // Room for a few rows below; the menu has five entries, two
        // headings and a rule.
        let (_, mut cx) = on_page(cx, 16.0, 180.0);
        let menu = cx.debug_bounds("settings-editor-menu").unwrap();
        assert!(menu.bottom() <= px(172.0), "capped 8pt from the window's edge: {menu:?}");
        // Down to Other command…, the last row: it scrolls into view (the
        // headings before it count among the menu's children).
        cx.simulate_keystrokes("down down down down");
        cx.run_until_parked();
        let other = cx.debug_bounds("settings-editor-option-4").unwrap();
        assert!(
            other.top() >= menu.top() && other.bottom() <= menu.bottom(),
            "the keyboard's row shows: {other:?} in {menu:?}"
        );
        // And back up to the top.
        cx.simulate_keystrokes("up up up up");
        cx.run_until_parked();
        let default = cx.debug_bounds("settings-editor-option-0").unwrap();
        assert!(default.top() >= menu.top(), "the first row shows again: {default:?} in {menu:?}");
    }
}
