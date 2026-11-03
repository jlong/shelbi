//! Edit, for a file on another machine (remote-window P8).
//!
//! A window on a project on another machine can't hand a far path to this
//! Mac's editor: the file isn't here. Instead the far daemon starts an
//! editor session there ([`ProjectLink::open_far_editor`]: the far
//! `$EDITOR`, in a terminal session named `<project>/edit/<key>`), and this
//! module shows it in a window of its own, attached over the window's relay
//! link. The titlebar says what's being edited where ("Editing task.yaml on
//! devbox"); quitting the editor closes the window.
//!
//! Every Edit a remote window offers comes here: the `edit:*` commands,
//! Settings' Edit YAML, and a path Cmd+clicked in a far terminal.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{div, prelude::*, px, App, Context, Entity, FocusHandle, Subscription, Task, Window};
use shelbi_app::far::FarFile;
use shelbi_app::session::{
    self as app_session, ConnectFailure, NoopRelauncher, ReconnectPolicy, RetryPolicy, SessionRef,
};
use shelbi_app::source::ProjectLink;
use shelbi_term::Size;

use crate::app::CloseWindow;
use crate::elements::terminal::session::{self as pane_session, Connected};
use crate::theme::style::{Type, Typography};
use crate::theme::{self, Theme};
use crate::views::session_view::{OnDeath, SessionConnector, SessionDeps, SessionView, SessionViewEvent};
use crate::views::sidebar;

/// The window's first size.
#[cfg_attr(test, allow(dead_code))]
const SIZE: (f32, f32) = (900.0, 640.0);
/// The titlebar row, as tall as the main window's.
const TITLEBAR_H: f32 = 52.0;

/// Ask the far daemon for an editor session on `file`, off the UI thread.
/// Answers the session's name for [`show`].
pub fn start(link: &ProjectLink, file: FarFile, cx: &App) -> Task<Result<String, String>> {
    let link = link.clone();
    cx.background_executor().spawn(async move { link.open_far_editor(file) })
}

/// [`start`], then [`show`], from view `V`: `on_error` gets why it
/// couldn't start, on the view.
pub fn open_from<V: 'static>(
    link: &ProjectLink,
    file: FarFile,
    label: String,
    cx: &mut Context<V>,
    on_error: impl FnOnce(&mut V, String, &mut Context<V>) + 'static,
) {
    let started = start(link, file, cx);
    let link = link.clone();
    cx.spawn(async move |this, cx| match started.await {
        Ok(session) => {
            cx.update(|cx| show(&link, session, label, cx));
        }
        Err(e) => {
            let _ = this.update(cx, |this, cx| on_error(this, e, cx));
        }
    })
    .detach();
}

/// What a label for `file` is: its file name.
pub fn label_of(path: &str) -> String {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path).to_string()
}

/// Show far editor session `session` (on `link`'s host) in a window of its
/// own, titled for `label`. A test records it instead ([`tests::SHOWN`]).
pub fn show(link: &ProjectLink, session: String, label: String, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = (link, cx);
        tests::SHOWN.with(|s| s.borrow_mut().push((session, label)));
    }
    #[cfg(not(test))]
    {
        let host = link.key().host().unwrap_or_default().to_string();
        let title = format!("{label} · {}", shelbi_app::remote_window::bare_host(&host));
        let options = super::about::options(&title, SIZE, true, cx);
        let link = link.clone();
        if let Err(e) = cx.open_window(options, |window, cx| {
            cx.new(|cx| FarEditorView::new(link, session, label, window, cx))
        }) {
            tracing::error!(error = %e, "could not open the far editor window");
        }
    }
}

/// Attaches to the far editor session by name, over the window's link.
struct FarSession {
    link: ProjectLink,
    name: String,
    host: String,
}

impl app_session::Connector for FarSession {
    fn connect(&self, _project: &str, _target: &SessionRef) -> Result<app_session::Connected, ConnectFailure> {
        self.link.attach_named(&self.name)
    }
}

impl SessionConnector for FarSession {
    fn connect(&self, project: &str, target: &SessionRef, retry: RetryPolicy) -> Result<Connected, ConnectFailure> {
        let c = app_session::retry_connect(self, project, target, retry)?;
        pane_session::wire(c.conn, c.events, Size::new(c.size.cols, c.size.rows), self.name.clone())
            .map_err(ConnectFailure::Message)
    }

    fn machine(&self) -> Option<&str> {
        Some(&self.host)
    }
}

/// The far editor window's root.
pub struct FarEditorView {
    label: String,
    host: String,
    terminal: Entity<SessionView>,
    focus: FocusHandle,
    _ended: Subscription,
    _changed: Subscription,
    _appearance: Subscription,
}

impl FarEditorView {
    #[cfg_attr(test, allow(dead_code))]
    fn new(link: ProjectLink, session: String, label: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        theme::window::apply_to(window, cx);
        let host = shelbi_app::remote_window::bare_host(link.key().host().unwrap_or_default()).to_string();
        let project = link.key().name.clone();
        let deps = SessionDeps {
            connector: Arc::new(FarSession {
                link,
                name: session,
                host: host.clone(),
            }),
            relauncher: Arc::new(NoopRelauncher),
            retry: RetryPolicy::default(),
            reconnect: ReconnectPolicy::default(),
            clock: Rc::new(Instant::now),
        };
        let handle = window.window_handle();
        // The target only names the session in the view's notices.
        let target = SessionRef::Workspace(label.clone());
        let terminal = cx.new(|cx| {
            let mut view = SessionView::new(&project, target, "editor", handle, deps, None, cx);
            view.set_remote_machine(Some(host.clone()));
            // Quitting the editor closes the window.
            view.set_on_death(OnDeath::Close);
            view.show(cx);
            view
        });
        let ended = cx.subscribe_in(&terminal, window, |_, _, event: &SessionViewEvent, window, _| {
            if *event == SessionViewEvent::Ended {
                window.remove_window();
            }
        });
        let changed = cx.observe(&terminal, |_, _, cx| cx.notify());
        let focus = terminal.read(cx).focus_target(cx);
        focus.focus(window, cx);
        FarEditorView {
            label,
            host,
            terminal,
            focus: cx.focus_handle(),
            _ended: ended,
            _changed: changed,
            _appearance: theme::follow_system(window, cx),
        }
    }
}

impl Render for FarEditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let titlebar = div()
            .flex_none()
            .h(px(TITLEBAR_H))
            .pl(px(sidebar::BESIDE_TRAFFIC_LIGHTS))
            .pr(px(sidebar::BESIDE_TRAFFIC_LIGHTS))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .debug_selector(|| "far-editor-label".into())
                    .type_style(Type::SmallMedium)
                    .text_color(theme.foreground)
                    .truncate()
                    .child(format!("Editing {} on {}", self.label, self.host)),
            );
        div()
            .id("far-editor")
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .font_family(crate::theme::style::UI_FAMILY)
            .child(titlebar)
            .child(self.terminal.clone())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;

    thread_local! {
        /// The far editor sessions [`show`] was asked to show: the session's
        /// name and the label, in order.
        pub(crate) static SHOWN: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    }

    #[test]
    fn a_label_is_the_file_name() {
        assert_eq!(label_of("/srv/p/.shelbi/workflows/task.yaml"), "task.yaml");
        assert_eq!(label_of("/srv/p/workflows/"), "workflows");
        assert_eq!(label_of("x"), "x");
    }
}
