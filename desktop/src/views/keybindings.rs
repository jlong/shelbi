//! Settings ▸ Keybindings: the bindings editor (Figma frame 2252:28172).
//!
//! The header (the title, Open keys.yaml, the helper), then the search, the
//! context picker and Reset all, then one group per keymap context with a
//! row per action: its label and its chords as keycaps, `or` between them.
//! The list scrolls under the header.
//!
//! Clicking a keycap captures a new chord for it ([`KeybindingsState`] has
//! the rules); a row's hover adds a + (another chord) and, when the row
//! differs from the built-in chords, a reset. Neither is drawn in the frame
//! (the resting layout is the frame's), nor are the capture and conflict
//! states: the capture keycap takes the focus ring and reads "Press keys",
//! and a conflict is a line in the error color under its row.
//!
//! While capturing, a keystroke interceptor takes every key before the
//! window's own bindings and menus can (⌘ chords, Ctrl+P, Esc), so any chord
//! can be bound. A finished capture is written to `keys.yaml` as it is on
//! disk (`shelbi_state::keymap::edit`, comments kept) off the UI thread, and
//! [`KeybindingsEvent::Saved`] hands the window the merged keymaps, which it
//! takes for its keys and menus at once (other windows reread `keys.yaml`
//! when they come to the front, as for any edit to it).

use gpui::{
    div, prelude::*, px, relative, App, ClickEvent, Context, Entity, EventEmitter, Focusable, MouseButton,
    SharedString, Subscription, Window,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_state::keymap::{Action, KeyChord, Keymaps, CONTEXTS};

use super::keybindings_state::{conflict_message, Group, KeyOutcome, KeybindingsState, Row, Slot};
use crate::assets::{heroicon, icons};
use crate::elements::controls;
use crate::keys::{chord_from_keystroke, keycap_keys};
use crate::menus::tree::{reserved_conflicts, ShortcutConflict};
use crate::theme::style::{self, ButtonKind, ButtonSize, IconButtonSize, Type, Typography};
use crate::theme::Theme;

/// A binding row's height, its 1px rule below inside the frame's 40.
const ROW_H: f32 = 39.0;
/// A keycap's least width: the Button component at Small, at least
/// square-ish.
const KEYCAP_MIN_W: f32 = 36.0;
/// Room past the rows' right edge for a row's hover buttons (+, reset).
const HOVER_W: f32 = 6.0 + 24.0 + 4.0 + 24.0;
/// The context picker's menu width.
const PICKER_W: f32 = 220.0;

/// What the page tells the settings view.
#[derive(Debug, Clone)]
pub enum KeybindingsEvent {
    /// `keys.yaml` was written, giving these merged keymaps: the window
    /// takes them at once.
    Saved(Box<Keymaps>),
    /// Esc in the empty search: hand the keys back to the settings sidebar.
    Leave,
}

/// The Keybindings page.
pub struct KeybindingsPage {
    state: KeybindingsState,
    search: Entity<InputState>,
    picker_open: bool,
    picker_anchor: style::Anchor,
    /// `keys.yaml` bindings macOS already owns.
    reserved: Vec<ShortcutConflict>,
    /// The last read, write or open that failed.
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl KeybindingsPage {
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx));
        let page = cx.entity().downgrade();
        let this_window = window.window_handle();
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.state.query = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            // Ahead of every binding and menu: a capture takes any chord.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                let taken = page
                    .update(cx, |page, cx| page.capture_key(chord, cx))
                    .unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
        ];
        let mut page = KeybindingsPage {
            state: KeybindingsState::new(Some(project.to_string())),
            search,
            picker_open: false,
            picker_anchor: Default::default(),
            reserved: Vec::new(),
            error: None,
            _subscriptions: subscriptions,
        };
        page.reserved = reserved_conflicts(page.state.keymaps());
        // A test's window never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(cx);
        }
        page
    }

    /// Read `keys.yaml` off the UI thread (after the shared loader's one-shot
    /// migrations, so the file is the one the TUI reads) and show it.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let project = self.state.project().map(str::to_string);
        let read = cx.background_executor().spawn(async move {
            shelbi_state::keymap::load_keymaps(project.as_deref());
            shelbi_state::keymap::edit::read_keys_file().map_err(|e| format!("Couldn't read keys.yaml: {e}"))
        });
        cx.spawn(async move |this, cx| {
            let text = read.await;
            this.update(cx, |this, cx| {
                match text {
                    Ok(text) => this.show(text),
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show `text` (a `keys.yaml`).
    fn show(&mut self, text: String) {
        self.state.load(text);
        self.reserved = reserved_conflicts(self.state.keymaps());
    }

    /// A key while capturing; whether it was the capture's.
    fn capture_key(&mut self, chord: KeyChord, cx: &mut Context<Self>) -> bool {
        let outcome = self.state.key(chord);
        self.settle(outcome, cx)
    }

    /// Act on what a capture, reset or Reset all did; whether it was one.
    fn settle(&mut self, outcome: KeyOutcome, cx: &mut Context<Self>) -> bool {
        match outcome {
            KeyOutcome::Ignored => return false,
            KeyOutcome::Cancelled | KeyOutcome::Conflict => {}
            KeyOutcome::Save(change) => {
                self.error = None;
                self.reserved = reserved_conflicts(self.state.keymaps());
                // A test's window never writes the real `~/.shelbi`: it takes
                // the change as written.
                if cfg!(test) {
                    cx.emit(KeybindingsEvent::Saved(Box::new(self.state.keymaps().clone())));
                    cx.notify();
                    return true;
                }
                let project = self.state.project().map(str::to_string);
                let write = cx.background_executor().spawn(async move {
                    shelbi_state::keymap::edit::update_keys_file(|text| change.apply(text, project.as_deref()))
                });
                cx.spawn(async move |this, cx| {
                    let written = write.await;
                    this.update(cx, |this, cx| {
                        match written {
                            Ok(text) => {
                                this.show(text);
                                cx.emit(KeybindingsEvent::Saved(Box::new(this.state.keymaps().clone())));
                            }
                            // Back to what is on disk.
                            Err(e) => {
                                this.error = Some(format!("Couldn't save keys.yaml: {e}"));
                                this.load(cx);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            KeyOutcome::Failed(e) => self.error = Some(format!("{e}. Fix it in keys.yaml.")),
        }
        cx.notify();
        true
    }

    /// End an open capture (the page is leaving the screen).
    pub fn cancel_capture(&mut self, cx: &mut Context<Self>) {
        if self.state.capture().is_some() || self.picker_open {
            self.state.cancel();
            self.picker_open = false;
            cx.notify();
        }
    }

    fn start_capture(&mut self, action: Action, slot: Slot, cx: &mut Context<Self>) {
        self.picker_open = false;
        self.state.start_capture(action, slot);
        cx.notify();
    }

    /// Esc in the search: clear it, or, empty, leave it.
    fn escape_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.query.is_empty() {
            cx.emit(KeybindingsEvent::Leave);
        } else {
            self.clear_search(window, cx);
        }
        cx.notify();
    }

    /// Empty the search (every row shows again), leaving the keyboard where
    /// it is.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.query.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    /// Open `keys.yaml` in the default text editor, creating a commented stub
    /// first when there is none (an editor won't open a missing file).
    fn open_keys(&mut self, cx: &mut Context<Self>) {
        let opened = cx.background_executor().spawn(async { open_keys_yaml() });
        cx.spawn(async move |this, cx| {
            if let Err(e) = opened.await {
                this.update(cx, |this, cx| {
                    this.error = Some(e);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let reserved: Vec<String> = self.reserved.iter().map(ShortcutConflict::message).collect();
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(controls::CONTROL_GAP))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child("Keybindings"),
                    )
                    .child(
                        style::button("settings-open-keys", "Open keys.yaml", ButtonKind::Secondary, ButtonSize::Small, false, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.open_keys(cx))),
                    ),
            )
            .child(
                div()
                    .type_style(Type::XSmall)
                    .text_color(theme.form_hint)
                    .child("Click a shortcut to change it. Bindings are shared with the TUI."),
            )
            .children(reserved.into_iter().map(|line| {
                div()
                    .debug_selector(|| "settings-shortcut-conflict".into())
                    .type_style(Type::XSmall)
                    .text_color(theme.error)
                    .child(line)
            }))
            .children(self.error.clone().map(|e| {
                div()
                    .debug_selector(|| "keybindings-error".into())
                    .type_style(Type::XSmall)
                    .text_color(theme.error)
                    .child(e)
            }))
    }

    fn render_filters(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let empty = self.state.query.is_empty();
        let field = div()
            .debug_selector(|| "keybindings-search-field".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .type_style(Type::Base)
            .text_color(theme.form_text)
            .child(
                Input::new(&self.search)
                    .appearance(false)
                    .px(px(0.0))
                    .py(px(0.0))
                    .h_full()
                    .type_style(Type::Base),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .text_color(theme.form_hint)
                        .child("Search actions or shortcuts"),
                )
            });
        let focus = self.search.read(cx).focus_handle(cx);
        let refocus = focus.clone();
        let search = style::search_box_base("keybindings-search", field, focused, theme)
            .flex_1()
            .min_w_0()
            .on_click(move |_, window, cx| window.focus(&focus, cx))
            // The clear button keeps the keyboard in the field, for the
            // next search.
            .when(!empty, |d| {
                d.child(style::search_clear("keybindings-search-clear", theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.clear_search(window, cx);
                        window.focus(&refocus, cx);
                        cx.stop_propagation();
                    },
                )))
            })
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.escape_search(window, cx);
                cx.stop_propagation();
            }));
        let context_label = self
            .state
            .context
            .and_then(shelbi_state::keymap::context)
            .map_or("All contexts", |c| c.title);
        let picker = base_button("keybindings-context", ButtonKind::Secondary, theme)
            .relative()
            .gap(px(6.0))
            .child(div().whitespace_nowrap().child(context_label))
            // Figma's 20pt chevron in a 14pt slot, hanging past its right.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .w(px(14.0))
                    .child(heroicon(icons::CHEVRON_UP_DOWN).text_color(theme.button_icon)),
            )
            .child(style::anchor_probe(&self.picker_anchor))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.picker_open = !this.picker_open;
                this.state.cancel();
                cx.notify();
            }));
        let customized = self.state.any_customized();
        let reset_all = base_button("keybindings-reset-all", ButtonKind::Secondary, theme)
            .child("Reset all")
            .when(!customized, |b| b.opacity(0.4).cursor_default())
            .when(customized, |b| {
                b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    let outcome = this.state.reset_all();
                    this.settle(outcome, cx);
                }))
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(32.0))
            .child(search)
            .child(picker)
            .child(reset_all)
    }

    fn render_group(&self, group: &Group, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let context = group.context;
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w(px(controls::LABELED_MAX_W))
            .debug_selector(move || format!("keybindings-group-{}", context.mode))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child(context.title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .type_style(Type::XSmall)
                            .text_color(theme.form_hint)
                            .child(context.hint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(group.rows.iter().map(|row| self.render_row(row, theme, cx))),
            )
    }

    fn render_row(&self, row: &Row, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let action = row.action;
        let id = format!("keybinding-{}-{}", action.mode(), action.key_name());
        let capture = self.state.capture().filter(|c| c.action == action);
        let conflict = capture.and_then(|_| self.state.conflict());
        let capturing = |slot: Slot| capture.is_some_and(|c| c.slot == slot);

        let mut caps: Vec<gpui::AnyElement> = Vec::new();
        let or = || {
            div()
                .flex_none()
                .type_style(Type::XSmall)
                .text_color(theme.form_hint)
                .child("or")
                .into_any_element()
        };
        for (shown, (at, chord)) in row.shown().into_iter().enumerate() {
            if shown > 0 {
                caps.push(or());
            }
            let slot = Slot::Chord(at);
            let cap = keycap(format!("{id}-key-{at}"), keycap_keys(&chord), capturing(slot), theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.start_capture(action, slot, cx)));
            caps.push(cap.into_any_element());
        }
        if capturing(Slot::New) {
            if !row.chords.is_empty() {
                caps.push(or());
            }
            caps.push(keycap(format!("{id}-key-new"), Vec::new(), true, theme).into_any_element());
        }
        // An unbound action still offers a keycap to bind it.
        if row.chords.is_empty() && capture.is_none() {
            let cap = keycap(format!("{id}-key-new"), Vec::new(), false, theme)
                .child(div().type_style(Type::XSmall).text_color(theme.form_hint).child("None"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.start_capture(action, Slot::New, cx)));
            caps.push(cap.into_any_element());
        }

        // The hover buttons sit past the row's right edge, so the resting
        // row is the frame's.
        let group_id = SharedString::from(id.clone());
        let mut hover = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(relative(1.0))
            .pl(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .invisible()
            .group_hover(group_id.clone(), |s| s.visible())
            .child(
                style::icon_button(format!("{id}-add"), icons::PLUS, IconButtonSize::Small, false, theme)
                    .tooltip(style::tooltip("Add a shortcut"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.start_capture(action, Slot::New, cx)
                    })),
            );
        if row.customized {
            hover = hover.child(
                style::icon_button(format!("{id}-reset"), icons::ARROW_UTURN_LEFT, IconButtonSize::Small, false, theme)
                    .tooltip(style::tooltip("Reset to default"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let outcome = this.state.reset(action);
                        this.settle(outcome, cx);
                    })),
            );
        }

        div()
            .id(group_id.clone())
            .debug_selector({
                let id = id.clone();
                move || id
            })
            .group(group_id)
            .relative()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(theme.divider_strong)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .h(px(ROW_H))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .type_style(Type::Base)
                            .text_color(theme.form_text)
                            .child(row.label),
                    )
                    .child(div().flex().flex_row().flex_none().items_center().gap(px(6.0)).children(caps)),
            )
            .children(conflict.map(|c| {
                div()
                    .debug_selector(|| "keybindings-conflict".into())
                    .pb(px(10.0))
                    .type_style(Type::XSmall)
                    .text_color(theme.error)
                    .child(conflict_message(&c))
            }))
            .child(hover)
    }

    fn render_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.picker_open {
            return None;
        }
        let current = self.state.context;
        let mut options: Vec<(String, Option<&'static str>)> = vec![("All contexts".into(), None)];
        options.extend(CONTEXTS.iter().map(|c| (c.title.to_string(), Some(c.mode))));
        let rows: Vec<_> = options
            .into_iter()
            .enumerate()
            .map(|(ix, (label, mode))| {
                let selector = format!("keybindings-context-{}", mode.unwrap_or("all"));
                super::activity::picker_row(theme, mode == current, label)
                    .id(("keybindings-context-option", ix))
                    .debug_selector(move || selector)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.context = mode;
                        this.picker_open = false;
                        cx.notify();
                    }))
            })
            .collect();
        Some(
            div()
                .id("keybindings-picker-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.picker_open = false;
                    cx.notify();
                }))
                .child(style::below(
                    &self.picker_anchor,
                    gpui::point(px(0.0), px(0.0)),
                    style::menu_entrance(
                        "keybindings-picker-menu",
                        super::issues::panel(theme)
                            .id("keybindings-picker-panel")
                            .debug_selector(|| "keybindings-picker-panel".into())
                            .w(px(PICKER_W))
                            .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                            .children(rows),
                    ),
                )),
        )
    }
}

impl EventEmitter<KeybindingsEvent> for KeybindingsPage {}

impl Render for KeybindingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let groups = self.state.groups();
        let list = div()
            .id("keybindings-list")
            .debug_selector(|| "keybindings-list".into())
            .flex_1()
            .min_h_0()
            .w(px(controls::LABELED_MAX_W + HOVER_W))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(24.0))
                    .children(groups.iter().map(|g| self.render_group(g, &theme, cx)))
                    .when(groups.is_empty(), |d| {
                        d.child(
                            div()
                                .type_style(Type::Base)
                                .text_color(theme.form_hint)
                                .child("No actions or shortcuts match."),
                        )
                    }),
            );
        div()
            .id("keybindings")
            .debug_selector(|| "keybindings".into())
            .relative()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .h_full()
            .w_full()
            // A click off the keycaps ends a capture.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.state.capture().is_some() {
                        this.state.cancel();
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .w_full()
                    .max_w(px(controls::LABELED_MAX_W))
                    .child(self.render_header(&theme, cx))
                    .child(self.render_filters(&theme, window, cx)),
            )
            .child(list)
            .children(self.render_picker(&theme, cx))
    }
}

/// The Button component at Base (14 / 18 medium, 32 tall), its content to
/// come.
fn base_button(id: &'static str, kind: ButtonKind, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    style::button_base(id, kind, ButtonSize::Base, false, theme)
}

/// A chord's keycap: the Button component at Small with only its Hotkey,
/// the keys in gray. Capturing, it takes the focus ring and reads "Press
/// keys".
fn keycap(id: String, keys: Vec<String>, capturing: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let cap = style::button_base(id, ButtonKind::Secondary, ButtonSize::Small, false, theme)
        .relative()
        .min_w(px(KEYCAP_MIN_W))
        // Keep a click on a keycap from ending the capture it starts.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if capturing {
        cap.child(
            div()
                .type_style(Type::XSmall)
                .text_color(theme.text_loud)
                .whitespace_nowrap()
                .child("Press keys"),
        )
        .children(style::focus_bands(6.0, theme))
    } else {
        cap.child(style::keycap_label(keys, theme))
    }
}

/// `~/.shelbi/keys.yaml`, the shared key bindings.
pub fn keys_yaml_path() -> Option<std::path::PathBuf> {
    shelbi_state::shelbi_home()
        .ok()
        .map(|home| home.join(shelbi_state::keymap::KEYS_FILENAME))
}

/// The stub a missing `keys.yaml` starts as: comments only, so it changes no
/// binding.
const KEYS_STUB: &str = "# Shelbi key bindings, shared by the TUI and the desktop app.\n\
# Override a built-in chord per mode; unlisted actions keep their defaults.\n\
# `shelbi config lint` checks this file.\n";

fn open_keys_yaml() -> Result<(), String> {
    let path = keys_yaml_path().ok_or("No Shelbi home directory.")?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, KEYS_STUB).map_err(|e| format!("Couldn't create keys.yaml: {e}"))?;
    }
    let status = std::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .status()
        .map_err(|e| format!("Couldn't open keys.yaml: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Couldn't open keys.yaml in a text editor.".into())
    }
}

#[cfg(feature = "visual-check")]
impl KeybindingsPage {
    /// Open a capture (`--visual-figma`'s capture states).
    pub(crate) fn offscreen_capture(&mut self, action: Action, slot: Slot, cx: &mut Context<Self>) {
        self.start_capture(action, slot, cx);
    }

    /// Press `chord` into the open capture.
    pub(crate) fn offscreen_key(&mut self, chord: &str, cx: &mut Context<Self>) {
        if let Ok(chord) = KeyChord::parse(chord) {
            self.capture_key(chord, cx);
        }
    }

    /// Type `query` into the search.
    pub(crate) fn offscreen_search(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(query.to_string(), window, cx));
        self.state.query = query.to_string();
        cx.notify();
    }

    /// Open the context picker.
    pub(crate) fn offscreen_open_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_open = true;
        cx.notify();
    }

    /// Show `text` as `keys.yaml`, without writing it.
    pub(crate) fn offscreen_show(&mut self, text: &str, cx: &mut Context<Self>) {
        self.show(text.to_string());
        cx.notify();
    }
}

#[cfg(test)]
impl KeybindingsPage {
    pub(crate) fn state(&self) -> &KeybindingsState {
        &self.state
    }

    pub(crate) fn picker_open(&self) -> bool {
        self.picker_open
    }

    /// Whether the search field has the keyboard.
    pub(crate) fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search.read(cx).focus_handle(cx).is_focused(window)
    }
}
