//! Settings' terminal font picker: the Picker component (the family in a
//! textbox, then "Choose font"), opening a list under it.
//!
//! Open, it lists the bundled faces (Source Code Pro, the default, then
//! Geist Mono), a rule, then the installed monospaced families
//! ([`fonts::installed`], read off the UI thread once per process), each row
//! set in its own font. Typing filters the list; ↑ / ↓ move through it,
//! Return picks and Escape closes. A pick is announced as
//! [`FontPickerEvent::Picked`]; the settings view saves it and applies it to
//! every terminal.
//!
//! A saved family that isn't installed stays the value (terminals fall back
//! to Source Code Pro when they measure it) with a muted "Not installed"
//! note in the field.

use gpui::{
    deferred, div, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    ScrollHandle, SharedString, Subscription, Window,
};
use gpui_kit::component::input::{Enter, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};

use crate::assets::{heroicon, icons, ICON_SIZE};
use crate::elements::controls;
use crate::elements::placed::placed;
use crate::elements::terminal::fonts::{self, Choice, InstalledFonts};
use crate::theme::style::{self, Anchor, Type, Typography, BUTTON_H};
use crate::theme::Theme;

/// How tall the open list grows before it scrolls.
const LIST_MAX_H: f32 = 320.0;

/// What the picker tells its view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontPickerEvent {
    /// A family was chosen from the list.
    Picked(String),
}

/// The terminal font picker.
pub struct FontPicker {
    /// The family shown (the saved one).
    family: String,
    /// The installed families, once read.
    fonts: Option<&'static InstalledFonts>,
    open: bool,
    query: Entity<InputState>,
    /// The keyboard's row among the listed families.
    highlight: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Where the picker (textbox and button) last painted, so a press on it
    /// toggles rather than counting as a press outside the open list.
    field: Anchor,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FontPickerEvent> for FontPicker {}

impl FontPicker {
    pub fn new(family: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search fonts"));
        let subscription = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.highlight = 0;
                this.scroll.scroll_to_item(0);
                cx.notify();
            }
        });
        let read = cx.background_executor().spawn(async { fonts::installed() });
        cx.spawn(async move |this, cx| {
            let installed = read.await;
            this.update(cx, |this, cx| {
                this.fonts = Some(installed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        FontPicker {
            family,
            fonts: None,
            open: false,
            query,
            highlight: 0,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            field: Anchor::default(),
            _subscriptions: vec![subscription],
        }
    }

    /// The family shown.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Show `family` (read from the config, or changed elsewhere).
    pub fn set_family(&mut self, family: String, cx: &mut Context<Self>) {
        if self.family != family {
            self.family = family;
            cx.notify();
        }
    }

    #[cfg(feature = "visual-check")]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the installed families have been read.
    #[cfg(feature = "visual-check")]
    pub fn fonts_loaded(&self) -> bool {
        self.fonts.is_some()
    }

    /// Whether the shown family is known to be missing (false until the
    /// installed families are read).
    pub fn not_installed(&self) -> bool {
        self.fonts.is_some_and(|f| !f.has(&self.family))
    }

    /// The rows for the current query.
    fn choices(&self, cx: &Context<Self>) -> Vec<Choice> {
        let empty = Default::default();
        let monospace = self.fonts.map_or(&empty, |f| &f.monospace);
        fonts::choices(monospace, &self.query.read(cx).value())
    }

    /// Open the list with the search cleared and the shown family as the
    /// keyboard's row, the search field focused.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return;
        }
        self.open = true;
        self.query.update(cx, |q, cx| q.set_value("", window, cx));
        let choices = self.choices(cx);
        let families = fonts::families(&choices);
        self.highlight = families.iter().position(|f| *f == self.family).unwrap_or(0);
        if let Some(row) = row_of(&choices, self.highlight) {
            self.scroll.scroll_to_item(row);
        }
        let focus = self.query.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Close the list, the field keeping the keyboard focus.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Move the keyboard's row by `delta`, stopping at the ends.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let count = fonts::families(&choices).len();
        if count == 0 {
            return;
        }
        self.highlight = (self.highlight as isize + delta).clamp(0, count as isize - 1) as usize;
        if let Some(row) = row_of(&choices, self.highlight) {
            self.scroll.scroll_to_item(row);
        }
        cx.notify();
    }

    /// Choose `family`: shown at once, announced, and the list closed.
    fn pick(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        self.family = family.clone();
        self.close(window, cx);
        cx.emit(FontPickerEvent::Picked(family));
    }

    /// Choose the keyboard's row.
    fn pick_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        if let Some(family) = fonts::families(&choices).get(self.highlight) {
            let family = family.to_string();
            self.pick(family, window, cx);
        }
    }

    /// Keys on the closed field: Return, Space or ↓ open the list.
    fn field_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return;
        }
        if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
            self.open(window, cx);
            cx.stop_propagation();
        }
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = self.choices(cx);
        let mut family_ix = 0;
        let rows: Vec<gpui::AnyElement> = choices
            .iter()
            .map(|choice| match choice {
                Choice::Divider => div()
                    .flex_none()
                    .h(px(1.0))
                    .mx_2()
                    .my_1()
                    .bg(theme.divider)
                    .into_any_element(),
                Choice::Family(family) => {
                    let ix = family_ix;
                    family_ix += 1;
                    let current = *family == self.family;
                    let pick = family.clone();
                    style::menu_row(
                        SharedString::from(format!("settings-font-{family}")),
                        ix == self.highlight,
                        theme,
                    )
                    .flex_none()
                    .child(
                        // Each family previewed in itself.
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(SharedString::from(family.clone()))
                            .child(family.clone()),
                    )
                    .child(div().w(ICON_SIZE).flex_none().when(current, |d| {
                        d.child(heroicon(icons::CHECK).text_color(theme.accent))
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(pick.clone(), window, cx)
                    }))
                    .into_any_element()
                }
            })
            .collect();
        let note = if self.fonts.is_none() {
            Some("Finding installed fonts…")
        } else if rows.is_empty() {
            Some("No monospaced font matches.")
        } else {
            None
        };
        div()
            .id("settings-font-list")
            .flex()
            .flex_col()
            .max_h(px(LIST_MAX_H))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_1()
            .children(rows)
            .children(note.map(|note| {
                div()
                    .flex_none()
                    .px_2()
                    .h(px(BUTTON_H))
                    .flex()
                    .items_center()
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(note)
            }))
    }
}

/// The row (divider included) of the `highlight`-th family.
fn row_of(choices: &[Choice], highlight: usize) -> Option<usize> {
    choices
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Choice::Family(_)))
        .nth(highlight)
        .map(|(row, _)| row)
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let focused = self.focus.is_focused(window);
        let not_installed = self.not_installed();
        // The Picker: the family in a textbox, "Choose font" after it; a
        // press on either toggles the list.
        let family = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().child(self.family.clone()))
            .when(not_installed, |d| {
                d.child(
                    div()
                        .debug_selector(|| "settings-font-not-installed".into())
                        .flex_none()
                        .type_style(Type::XSmall)
                        .text_color(theme.form_hint)
                        .child("Not installed"),
                )
            });
        let field = controls::textbox(family, focused || self.open, &theme)
            .id("settings-font-picker")
            .debug_selector(|| "settings-font-picker".into())
            .flex_1()
            .cursor_pointer()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::field_key))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)));
        let choose = controls::form_button("settings-font-choose", "Choose font", &theme)
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)));
        let field = controls::picker(field, choose).child(style::anchor_probe(&self.field));

        // The list floats 6px under the picker (or over it where the window
        // is short of room below), its width, drawn after everything
        // (`deferred`) so nothing below it moves. Capped, the list under the
        // search box scrolls.
        let field_bounds = self.field.clone();
        let list = self.open.then(|| {
            deferred(
                placed(
                    &self.field,
                    style::popover(
                        div()
                            .id("settings-font-menu")
                            .debug_selector(|| "settings-font-menu".into())
                            .flex()
                            .flex_col()
                            .w(px(controls::CONTROL_W))
                            .min_h_0()
                            .occlude(),
                        &theme,
                    )
                    .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                        this.step(-1, cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                        this.step(1, cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                        this.pick_highlighted(window, cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }))
                    .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        // A press on the picker toggles it closed through its click.
                        if field_bounds.get().is_some_and(|b| b.contains(&event.position)) {
                            return;
                        }
                        this.close(window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .border_b_1()
                            .border_color(theme.divider)
                            .child(heroicon(icons::MAGNIFYING_GLASS).text_color(theme.icon))
                            .child(
                                Input::new(&self.query)
                                    .appearance(false)
                                    .h(px(BUTTON_H + 4.0))
                                    .type_style(Type::Base),
                            ),
                    )
                    .child(self.render_list(&theme, cx)),
                )
                .gap(6.0),
            )
            .with_priority(1)
        });
        field.children(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
    use shelbi_state::ThemePreference;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn picker(cx: &mut TestAppContext) -> (Entity<FontPicker>, VisualTestContext, Rc<RefCell<Vec<String>>>) {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let window = cx.add_window(|window, cx| FontPicker::new(fonts::BUNDLED[0].into(), window, cx));
        let picker = window.root(cx).unwrap();
        let picked = Rc::new(RefCell::new(Vec::new()));
        let sink = picked.clone();
        cx.update(|cx| {
            cx.subscribe(&picker, move |_, event: &FontPickerEvent, _| match event {
                FontPickerEvent::Picked(f) => sink.borrow_mut().push(f.clone()),
            })
            .detach()
        });
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        (picker, vcx, picked)
    }

    #[gpui::test]
    fn the_keyboard_opens_moves_and_picks(cx: &mut TestAppContext) {
        let (picker, mut cx, picked) = picker(cx);
        picker.update_in(&mut cx, |p, window, cx| p.open(window, cx));
        cx.run_until_parked();
        assert!(picker.read_with(&cx, |p, _| p.open));
        // The current family is the keyboard's row; ↓ moves to Geist Mono.
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(*picked.borrow(), ["Geist Mono"]);
        picker.read_with(&cx, |p, _| {
            assert_eq!(p.family(), "Geist Mono");
            assert!(!p.open, "a pick closes the list");
        });
    }

    #[gpui::test]
    fn escape_closes_without_picking_and_typing_filters(cx: &mut TestAppContext) {
        let (picker, mut cx, picked) = picker(cx);
        picker.update_in(&mut cx, |p, window, cx| p.open(window, cx));
        cx.simulate_input("geist");
        cx.run_until_parked();
        picker.update(&mut cx, |p, cx| {
            assert_eq!(fonts::families(&p.choices(cx)), ["Geist Mono"]);
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(picked.borrow().is_empty());
        picker.read_with(&cx, |p, _| {
            assert!(!p.open);
            assert_eq!(p.family(), "Source Code Pro");
        });
    }

    #[test]
    fn row_of_skips_the_divider() {
        let choices = vec![
            Choice::Family("Source Code Pro".into()),
            Choice::Family("Geist Mono".into()),
            Choice::Divider,
            Choice::Family("Menlo".into()),
        ];
        assert_eq!(row_of(&choices, 0), Some(0));
        assert_eq!(row_of(&choices, 1), Some(1));
        assert_eq!(row_of(&choices, 2), Some(3));
        assert_eq!(row_of(&choices, 3), None);
    }
}
