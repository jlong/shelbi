//! Connect to Remote Session (`launch-desktop`, Figma "Connect to Remote
//! Session · Host" 2539:10950 and "· Choose session" 2539:10951): one dialog
//! wherever it starts. The launch window's Connect to remote, Shelbi ▸
//! Connect to Remote Session… and the palette all open it over the window
//! they were asked from, on the scrim.
//!
//! [`ConnectFlow`] (shared with the TUI) holds the two steps and decides
//! what each button does; this view paints it, takes the keys, keeps the host field in
//! step with it, and runs its searches and connections through the
//! [`LaunchServices`] connector on the background executor, remembering a
//! connected session in the recent remote sessions.
//!
//! - **Host**: "Connect to remote", the line under it, the SSH host
//!   Textbox and its note, then Cancel and Find Sessions (Primary). While a
//!   search runs Find Sessions is the disabled "Finding Sessions…" with a
//!   spinner; an SSH error shows under the field, which edits clear.
//! - **Choose session**: "Choose a session on <host>.", the green check and
//!   "Connected as <user>" with Change host, the sessions as cards (the
//!   launch window's, [`super::launch::card`]) each with its own Connect,
//!   then Cancel and Connect (Primary) for the selected one.
//!
//! Keys, ahead of the host field: Return is the screen's primary action, Esc
//! cancels, ↑ / ↓ move through the sessions. The pointer over a session
//! selects it.

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Role, Stateful,
    Subscription, Window,
};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_app::launch::connect::{Busy, ConnectFlow, Stage};
use shelbi_app::launch::remote as copy;
use shelbi_app::launch::{self, RemoteSession};
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::launch::card;
use super::workflow_editor::text_field;
use crate::assets::{heroicon, icons};
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::platform::launch::{services, LaunchServices};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The panel's width (the frames').
pub const DIALOG_W: f32 = 580.0;
/// Each screen's height when its content fits (the frames'): the actions sit
/// at the bottom, past the flexible space.
const HOST_H: f32 = 340.0;
const CHOOSE_H: f32 = 456.0;
/// The panel's inset, its corners, and the room between its parts.
const PADDING: f32 = 32.0;
const RADIUS: f32 = 26.0;
const GAP: f32 = 24.0;
/// The heading's size (Desktop/Heading in these frames: 18 / 28 semibold).
const TITLE_SIZE: f32 = 18.0;
/// How many sessions show before the list scrolls.
const VISIBLE_ROWS: f32 = 5.0;

/// What the dialog tells the window it is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectEvent {
    /// Close it: cancelled, or connected to this session (remembered).
    Closed(Option<RemoteSession>),
}

/// The open dialog.
pub struct ConnectDialog {
    flow: ConnectFlow,
    focus: FocusHandle,
    host_input: Entity<InputState>,
    services: LaunchServices,
    _subscriptions: Vec<Subscription>,
}

impl ConnectDialog {
    /// The Host screen, run through the app's [`LaunchServices`].
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = services(cx);
        let focus = cx.focus_handle();
        let host_input = cx.new(|cx| InputState::new(window, cx));
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let subscriptions = vec![
            // Ahead of the host field's own bindings: Return and Esc are the
            // dialog's everywhere in it.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                if me.update(cx, |this, cx| this.key(chord, window, cx)).unwrap_or(false) {
                    cx.stop_propagation();
                }
            }),
            cx.subscribe_in(&host_input, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    let text = input.read(cx).value().to_string();
                    this.flow.set_host(&text);
                    cx.notify();
                }
            }),
        ];
        let mut dialog = ConnectDialog {
            flow: ConnectFlow::default(),
            focus,
            host_input,
            services,
            _subscriptions: subscriptions,
        };
        dialog.sync_focus(window, cx);
        dialog
    }

    #[cfg(test)]
    pub fn flow(&self) -> &ConnectFlow {
        &self.flow
    }

    /// Put `host` in the field (tests, `--visual-figma`).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn type_host(&mut self, host: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.host_input.update(cx, |s, cx| s.set_value(host.to_string(), window, cx));
        self.flow.set_host(host);
        cx.notify();
    }

    /// Hold Find Sessions busy without searching (`--visual-figma`'s
    /// progress frame).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_finding(&mut self, cx: &mut Context<Self>) {
        let _ = self.flow.find();
        cx.notify();
    }

    /// Close without connecting (Cancel, Esc, the window's ⌘W), dropping
    /// whatever runs.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ConnectEvent::Closed(None));
    }

    /// A key the dialog takes. `false` lets it through to the host field.
    pub(crate) fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if chord.mods != Mods::NONE {
            return false;
        }
        let choose = self.flow.stage() == Stage::Choose;
        match chord.code {
            Code::Enter => self.primary(None, window, cx),
            Code::Esc => self.dismiss(cx),
            Code::Up | Code::Down if choose => {
                if self.flow.busy().is_none() {
                    let at = self.flow.selected();
                    let to = if chord.code == Code::Up { at.saturating_sub(1) } else { at + 1 };
                    self.flow.select(to);
                    cx.notify();
                }
            }
            _ => return false,
        }
        true
    }

    /// The screen's primary action: Find Sessions, or Connect (to row
    /// `row`, else the selected one).
    fn primary(&mut self, row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        match self.flow.stage() {
            Stage::Host => {
                if let Some(host) = self.flow.find() {
                    self.search(host, window, cx);
                }
            }
            // A connect found the host's Shelbi stopped: the user asked to
            // start it.
            Stage::Choose if row.is_none() && self.flow.offers_start() => {
                if let Some(host) = self.flow.start() {
                    self.start(host, window, cx);
                }
            }
            Stage::Choose => {
                if let Some(session) = self.flow.connect(row) {
                    self.connect(session, window, cx);
                }
            }
        }
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Start Shelbi on `host` off the UI thread, then connect.
    fn start(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        let remote = self.services.remote.clone();
        let work = cx.background_executor().spawn(async move { remote.start_daemon(&host) });
        cx.spawn_in(window, async move |this, cx| {
            let answer = work.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(session) = this.flow.started(answer) {
                    this.connect(session, window, cx);
                }
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Change host: back to the field, its text kept.
    fn change_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flow.change_host();
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Whether the primary button can be pressed now.
    fn primary_enabled(&self) -> bool {
        self.flow.busy().is_none()
            && match self.flow.stage() {
                Stage::Host => !self.flow.host().trim().is_empty(),
                Stage::Choose => !self.flow.sessions().is_empty(),
            }
    }

    /// Find `host`'s sessions off the UI thread; the answer goes to the flow
    /// (a closed dialog drops it).
    fn search(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        let remote = self.services.remote.clone();
        let searched = host.clone();
        let work = cx.background_executor().spawn(async move { remote.find_sessions(&searched) });
        cx.spawn_in(window, async move |this, cx| {
            let answer = work.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = &answer {
                    tracing::info!("no remote sessions: {e}");
                }
                this.flow.found(&host, answer);
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Connect to `session` (remembering it) off the UI thread.
    fn connect(&mut self, session: RemoteSession, window: &mut Window, cx: &mut Context<Self>) {
        let LaunchServices { remote, recents } = self.services.clone();
        let work = cx
            .background_executor()
            .spawn(async move { launch::connect_remote(session, remote.as_ref(), &recents) });
        cx.spawn_in(window, async move |this, cx| {
            let answer = work.await;
            this.update_in(cx, |this, window, cx| {
                let session = answer.as_ref().ok().cloned();
                if this.flow.connected(answer) {
                    cx.emit(ConnectEvent::Closed(session));
                    return;
                }
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The keyboard goes to the host field on the Host screen, else to the
    /// dialog.
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.flow.stage() {
            Stage::Host => self.host_input.read(cx).focus_handle(cx),
            Stage::Choose => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }
}

impl EventEmitter<ConnectEvent> for ConnectDialog {}

impl Focusable for ConnectDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// "Connect to remote" and the line under it.
fn heading(intro: String, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .child(
            div()
                .debug_selector(|| "connect-heading".into())
                .type_style(Type::Heading)
                .text_size(px(TITLE_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_loud)
                .child(copy::DIALOG_HEADING),
        )
        .child(
            div()
                .debug_selector(|| "connect-intro".into())
                .type_style(Type::Base)
                .text_color(theme.foreground)
                .child(intro),
        )
}

/// An inline error line.
fn error_line(text: &str, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "connect-error".into())
        .type_style(Type::Small)
        .text_color(theme.error)
        .child(text.to_string())
}

/// The footer's primary button: `label` (the flow's, its busy form while its
/// call runs), pending with a spinner while `busy`.
fn action(id: &'static str, label: &'static str, enabled: bool, busy: bool, theme: &Theme) -> Stateful<Div> {
    let kind = ButtonKind::Primary;
    let b = if busy {
        style::pending_button(
            id,
            label,
            spinner::spinner_sized(format!("{id}-spinner"), theme.primary_label, 16.0),
            kind,
            ButtonSize::Base,
            theme,
        )
    } else if enabled {
        style::button(id, label, kind, ButtonSize::Base, false, theme)
    } else {
        style::disabled_button(id, label, kind, ButtonSize::Base, theme)
    };
    b.role(Role::Button).aria_label(label)
}

impl ConnectDialog {
    fn render_host(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let flow = &self.flow;
        let field = text_field(&self.host_input, "connect-host", None, flow.error().is_some(), window, cx).w_full();
        let details = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .w_full()
                    .child(
                        div()
                            .type_style(Type::SmallMedium)
                            .text_color(theme.foreground)
                            .child(copy::HOST_LABEL),
                    )
                    .child(field),
            )
            .children(flow.error().map(|e| error_line(e, theme)))
            .child(
                div()
                    .debug_selector(|| "connect-host-hint".into())
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(copy::HOST_HINT),
            );
        let find = action(
            "connect-find",
            flow.primary_label(),
            self.primary_enabled(),
            flow.busy() == Some(Busy::Finding),
            theme,
        )
        .aria_keyshortcuts("Return")
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.primary(None, window, cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .size_full()
            .child(heading(flow.intro(), theme))
            .child(details)
            .child(div().flex_1().min_h(px(12.0)))
            .child(self.footer(find, theme, cx))
    }

    fn render_choose(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let flow = &self.flow;
        let busy = flow.busy().is_some();
        let connected = div()
            .debug_selector(|| "connect-connected".into())
            .flex()
            .items_center()
            .gap(px(12.0))
            .w_full()
            .child(heroicon(icons::CHECK).text_color(theme.icon_green))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_style(Type::SmallMedium)
                    .text_color(theme.foreground)
                    .children(flow.connected_as()),
            )
            .child({
                let b = if busy {
                    style::disabled_button("connect-change-host", copy::CHANGE_HOST_LABEL, ButtonKind::Secondary, ButtonSize::Base, theme)
                } else {
                    style::button("connect-change-host", copy::CHANGE_HOST_LABEL, ButtonKind::Secondary, ButtonSize::Base, false, theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.change_host(window, cx)))
                };
                b.role(Role::Button).aria_label(copy::CHANGE_HOST_LABEL)
            });
        let rows = flow.sessions().iter().enumerate().map(|(i, s)| {
            let button = style::button(format!("connect-session-{i}-connect"), copy::CONNECT_LABEL, ButtonKind::Secondary, ButtonSize::Small, false, theme)
                .role(Role::Button)
                .aria_label(format!("{} {}", copy::CONNECT_LABEL, s.display_label()))
                .when(!busy, |b| {
                    b.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.primary(Some(i), window, cx);
                    }))
                });
            card(
                format!("connect-session-{i}"),
                s.display_label().to_string(),
                s.address(),
                i == flow.selected(),
                Some(button.into_any_element()),
                theme,
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.flow.busy().is_none() {
                    this.flow.select(i);
                    cx.notify();
                }
            }))
        });
        let list = div()
            .id("connect-sessions")
            .debug_selector(|| "connect-sessions".into())
            .role(Role::List)
            .flex()
            .flex_col()
            .gap(px(4.0))
            .w_full()
            .max_h(px(VISIBLE_ROWS * 64.0))
            .overflow_y_scroll()
            .children(rows);
        let connect = action(
            "connect-connect",
            flow.primary_label(),
            self.primary_enabled(),
            matches!(flow.busy(), Some(Busy::Connecting | Busy::Starting)),
            theme,
        )
        .aria_keyshortcuts("Return")
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.primary(None, window, cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .size_full()
            .child(heading(flow.intro(), theme))
            .child(connected)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .w_full()
                    .child(list)
                    .children(flow.error().map(|e| error_line(e, theme))),
            )
            .child(div().flex_1().min_h(px(12.0)))
            .child(self.footer(connect, theme, cx))
    }

    /// Cancel and the screen's primary button, at the right.
    fn footer(&self, primary: Stateful<Div>, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let cancel = style::button("connect-cancel", copy::CANCEL_LABEL, ButtonKind::Secondary, ButtonSize::Base, false, theme)
            .role(Role::Button)
            .aria_label(copy::CANCEL_LABEL)
            .aria_keyshortcuts("Escape")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss(cx)));
        div()
            .flex()
            .justify_end()
            .gap(px(12.0))
            .w_full()
            .child(cancel)
            .child(primary)
    }
}

impl Render for ConnectDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (content, min_h) = match self.flow.stage() {
            Stage::Host => (self.render_host(&theme, window, cx), HOST_H),
            Stage::Choose => (self.render_choose(&theme, cx), CHOOSE_H),
        };
        let panel = div()
            .id("connect-dialog")
            .debug_selector(|| "connect-dialog".into())
            .role(Role::Dialog)
            .aria_label(copy::DIALOG_TITLE)
            .flex()
            .flex_col()
            .w(px(DIALOG_W))
            .min_h(px(min_h))
            .p(px(PADDING))
            .rounded(px(RADIUS))
            .bg(theme.opaque().canvas)
            .shadow(style::dialog_shadow(&theme))
            .text_color(theme.foreground)
            .occlude()
            .child(content);
        div()
            .id("connect-layer")
            .track_focus(&self.focus)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .font_family(style::UI_FAMILY)
            .type_style(Type::Base)
            .child(
                div()
                    .id("connect-scrim")
                    .debug_selector(|| "connect-scrim".into())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.scrim)
                    .occlude()
                    .child(panel),
            )
    }
}

/// The [`LaunchServices`] a test runs the dialog against.
#[cfg(test)]
pub fn test_services(remote: shelbi_app::launch::FakeRemote, recents: shelbi_app::launch::RecentRemotes) -> LaunchServices {
    LaunchServices {
        remote: std::sync::Arc::new(remote),
        recents,
    }
}
