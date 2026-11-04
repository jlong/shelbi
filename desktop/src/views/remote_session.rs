//! A remote session's window (`launch-remote-connect`): a project of Shelbi
//! on another machine, opened from Connect to Remote Session or a recent
//! remote session.
//!
//! The window shows that machine's own Shelbi for the project, in a terminal
//! over `ssh -t` (`shelbi_app::launch::terminal::TerminalConnector`), so it
//! always matches the far side's version and configuration. Above it, a
//! titlebar row names who and where ("Connected to jlong@devbox · Shelbi")
//! with a dot: green while connected, yellow while reconnecting. When `ssh`
//! drops, the [`SessionView`] shows the shared "Reconnecting to <host>…
//! (attempt N)" notice and backs off until the host answers; each attempt
//! checks the host first, so a reconnect never starts the far daemon.
//! Quitting the far Shelbi closes the window; closing the window ends the
//! local `ssh` ([`terminal::end_local_session`]).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Subscription, Window, WindowHandle,
};
use shelbi_app::launch::link::HostLink;
use shelbi_app::launch::remote::{RemoteConnector, RemoteSession};
use shelbi_app::launch::RemoteListing;
use shelbi_app::remote_window::{self, RemoteOpen};
use shelbi_app::launch::terminal::{self, TerminalConnector};
use shelbi_app::session::{
    self as app_session, ConnectFailure, GridSize, NoopRelauncher, ReconnectPolicy, RetryPolicy,
    SessionRef,
};
use shelbi_term::Size;

use crate::app::CloseWindow;
use crate::elements::terminal::session::{self as pane_session, Connected};
use crate::theme::style::{Type, Typography};
use crate::theme::{self, Theme};
use crate::views::session_view::{OnDeath, SessionConnector, SessionDeps, SessionView, SessionViewEvent};
use crate::views::sidebar;

/// The window's first size.
pub const SIZE: (f32, f32) = (1100.0, 720.0);
/// The titlebar row: as tall as the main window's, so the traffic lights
/// center on it.
pub const TITLEBAR_H: f32 = 52.0;
/// The connection dot's diameter.
const DOT: f32 = 8.0;

/// Open `session`, a remote project the Connect dialog or a recent remote
/// session just connected to, in a window of its own.
///
/// The host's listing decides ([`remote_window::choose`]): the same Shelbi
/// opens the full main window over the host's relay link
/// ([`crate::app::open_remote_window`]); any other version (or one from
/// before the native window) opens the terminal window with a line saying
/// why. Every check runs off the UI thread. A test records the session
/// instead ([`tests::OPENED`]).
pub fn open_remote_session(session: RemoteSession, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
        tests::OPENED.with(|o| o.borrow_mut().push(session));
    }
    #[cfg(not(test))]
    {
        let remote = crate::platform::launch::services(cx).remote;
        let decide = cx.background_executor().spawn({
            let (session, remote) = (session.clone(), remote.clone());
            async move { decide(&session, remote) }
        });
        cx.spawn(async move |cx| {
            let decided = decide.await;
            cx.update(|cx| open_decided(session, remote, decided, cx));
        })
        .detach();
    }
}

/// Where `session` opens, and for a native window its host's link, already
/// up. Blocking (`ssh`): the listing, then the link (whose check is the
/// connector's: the host has the project and its daemon runs; it is never
/// started here).
#[cfg_attr(test, allow(dead_code))]
pub fn decide(session: &RemoteSession, remote: Arc<dyn RemoteConnector>) -> Decided {
    decide_with(session, remote, &HostLink::shared)
}

/// [`decide`] with the host's link from `link_for` (a test's in-process
/// relay).
pub fn decide_with(
    session: &RemoteSession,
    remote: Arc<dyn RemoteConnector>,
    link_for: &dyn Fn(&str) -> Arc<HostLink>,
) -> Decided {
    match remote_window::choose(&session.host, || remote.listing(&session.host)) {
        RemoteOpen::Terminal { notice } => Decided::Terminal { notice },
        RemoteOpen::Native(listing) => {
            let link = link_for(&session.host);
            // The user is opening this host, and its listing just came back:
            // an earlier No to a sign-in no longer stands.
            link.allow_sign_in();
            match link.connect(&|| remote.connect(session)) {
                Ok(_) => Decided::Native { listing, link },
                Err(error) => {
                    tracing::warn!(host = %session.host, %error, "the remote link didn't open");
                    Decided::Terminal {
                        notice: Some(remote_window::link_failed_notice(&session.host, &error)),
                    }
                }
            }
        }
    }
}

/// What [`decide`] decided.
pub enum Decided {
    /// The full main window, over `link` (up).
    Native { listing: RemoteListing, link: Arc<HostLink> },
    /// The terminal window, with the line saying why when there is one.
    Terminal { notice: Option<String> },
}

/// Open `session` as `decided`: the native main window, or the terminal
/// window (whose local `ssh` session runs through the CLI bootstrap found;
/// from the launch window, before any bootstrap, the CLI is found the same
/// way, off the UI thread, never starting this machine's daemon).
#[cfg_attr(test, allow(dead_code))]
pub fn open_decided(session: RemoteSession, remote: Arc<dyn RemoteConnector>, decided: Decided, cx: &mut App) {
    let notice = match decided {
        Decided::Native { listing, link } => {
            let window = crate::app::RemoteWindow::new(session, listing, link, remote);
            crate::app::open_remote_window(window, cx);
            return;
        }
        Decided::Terminal { notice } => notice,
    };
    if let Some((cli, _)) = crate::app::running_cli(cx) {
        open(session, remote, cli, notice, cx);
        return;
    }
    let find = cx.background_executor().spawn(async move {
        use crate::platform::bootstrap::{find_cli, Platform, SystemPlatform};
        let mut platform = SystemPlatform::default();
        let env = platform.login_env();
        find_cli(&mut platform, &env).map(|found| found.path)
    });
    cx.spawn(async move |cx| {
        // No CLI: the window says so when its session can't start.
        let cli = find.await.unwrap_or_else(|_| std::path::PathBuf::from("shelbi"));
        let _ = cx.update(|cx| open(session, remote, cli, notice, cx));
    })
    .detach();
}

/// Open in Terminal View: `session`, shown in a native window, in the
/// terminal window too (the far TUI over `ssh -t`), with no notice: the
/// user asked for it. A test records the session instead
/// ([`tests::TERMINAL_OPENED`]).
pub fn open_terminal_view(session: RemoteSession, remote: Arc<dyn RemoteConnector>, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = (remote, cx);
        tests::TERMINAL_OPENED.with(|o| o.borrow_mut().push(session));
    }
    #[cfg(not(test))]
    open_decided(session, remote, Decided::Terminal { notice: None }, cx);
}

/// Open `session` in a window of its own. The connect runs in the window
/// (the dialog that asked has already checked the host). `cli` is the
/// `shelbi` that runs the local `ssh` session.
// A test records the session instead (`open_remote_session`).
#[cfg_attr(test, allow(dead_code))]
pub fn open(
    session: RemoteSession,
    remote: Arc<dyn RemoteConnector>,
    cli: std::path::PathBuf,
    notice: Option<String>,
    cx: &mut App,
) -> Option<WindowHandle<RemoteSessionView>> {
    let connector = Arc::new(Terminal(TerminalConnector {
        session: session.clone(),
        remote,
        exe: cli,
        size: GridSize::new(120, 40),
    }));
    let ender = session.clone();
    let on_close: OnClose = Box::new(move || {
        std::thread::spawn(move || terminal::end_local_session(&ender));
    });
    open_with(session, connector, Some(on_close), notice, cx)
}

/// What closing the window does besides closing it (ending the local `ssh`).
pub type OnClose = Box<dyn FnOnce()>;

/// [`open`] over any connector and close hook (tests pass fakes).
pub fn open_with(
    session: RemoteSession,
    connector: Arc<dyn SessionConnector>,
    on_close: Option<OnClose>,
    notice: Option<String>,
    cx: &mut App,
) -> Option<WindowHandle<RemoteSessionView>> {
    let title = terminal::window_title(&session);
    let options = super::about::options(&title, SIZE, true, cx);
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| RemoteSessionView::new(session, connector, on_close, notice, window, cx))
    });
    match opened {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!(error = %e, "could not open the remote session window");
            None
        }
    }
}

/// The desktop's [`SessionConnector`] over the shared terminal connector:
/// the attach, then the pane's reader and writer threads.
#[cfg_attr(test, allow(dead_code))]
struct Terminal(TerminalConnector);

impl SessionConnector for Terminal {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure> {
        let c = app_session::retry_connect(&self.0, project, target, retry)?;
        pane_session::wire(
            c.conn,
            c.events,
            Size::new(c.size.cols, c.size.rows),
            terminal::local_session_name(&self.0.session),
        )
        .map_err(ConnectFailure::Message)
    }
}

/// The remote session window's root.
pub struct RemoteSessionView {
    session: RemoteSession,
    /// Why this host opened in the terminal view rather than the full
    /// window (the versions differ), shown under the titlebar.
    notice: Option<String>,
    terminal: Entity<SessionView>,
    focus: FocusHandle,
    _closed: Subscription,
    _ended: Subscription,
    _changed: Subscription,
    _appearance: Subscription,
}

impl RemoteSessionView {
    fn new(
        session: RemoteSession,
        connector: Arc<dyn SessionConnector>,
        on_close: Option<OnClose>,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        theme::window::apply_to(window, cx);
        let deps = SessionDeps {
            connector,
            relauncher: Arc::new(NoopRelauncher),
            retry: RetryPolicy::default(),
            reconnect: ReconnectPolicy::default(),
            clock: Rc::new(Instant::now),
        };
        let handle = window.window_handle();
        let target = TerminalConnector::target(&session);
        let host = session.host.clone();
        let project = session.project.clone();
        let terminal = cx.new(|cx| {
            let mut view = SessionView::new(&project, target, "remote", handle, deps, None, cx);
            view.set_remote_machine(Some(host));
            view.set_ssh_terminal(true);
            // Quitting the far Shelbi closes the window.
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
        let mut on_close = on_close;
        let closed = cx.on_release(move |_, _| {
            if let Some(on_close) = on_close.take() {
                on_close();
            }
        });
        let focus = terminal.read(cx).focus_target(cx);
        focus.focus(window, cx);
        RemoteSessionView {
            session,
            notice,
            terminal,
            focus: cx.focus_handle(),
            _closed: closed,
            _ended: ended,
            _changed: changed,
            _appearance: theme::follow_system(window, cx),
        }
    }

    /// The titlebar row's words and whether the link is up.
    pub fn status(&self, cx: &App) -> (String, Connection) {
        let terminal = self.terminal.read(cx);
        let connection = if terminal.is_reconnecting() {
            Connection::Reconnecting
        } else if terminal.pane().is_some() {
            Connection::Connected
        } else {
            Connection::Connecting
        };
        (terminal::connected_label(&self.session), connection)
    }

    /// Why the host opened in the terminal view, when it says.
    #[cfg(test)]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The terminal view (tests).
    #[cfg(test)]
    pub fn terminal(&self) -> &Entity<SessionView> {
        &self.terminal
    }

}

/// Where the connection stands, for the titlebar's dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Reconnecting,
}

impl Render for RemoteSessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (label, connection) = self.status(cx);
        let dot = match connection {
            Connection::Connected => theme.success,
            Connection::Reconnecting => theme.warning,
            Connection::Connecting => theme.muted,
        };
        let titlebar = div()
            .id("remote-titlebar")
            .debug_selector(|| "remote-titlebar".into())
            .flex_none()
            .h(px(TITLEBAR_H))
            .pl(px(sidebar::BESIDE_TRAFFIC_LIGHTS))
            .pr(px(sidebar::BESIDE_TRAFFIC_LIGHTS))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .debug_selector(|| "remote-dot".into())
                    .size(px(DOT))
                    .rounded_full()
                    .bg(dot),
            )
            .child(
                div()
                    .debug_selector(|| "remote-label".into())
                    .type_style(Type::SmallMedium)
                    .text_color(theme.foreground)
                    .truncate()
                    .child(label),
            );
        div()
            .id("remote-session")
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .font_family(crate::theme::style::UI_FAMILY)
            .child(titlebar)
            .when_some(self.notice.clone(), |root, notice| {
                root.child(
                    div()
                        .debug_selector(|| "remote-notice".into())
                        .flex_none()
                        .px_4()
                        .py_2()
                        .bg(theme.notice)
                        .border_b_1()
                        .border_color(theme.divider)
                        .type_style(Type::Small)
                        .text_color(theme.text_loud)
                        .child(notice),
                )
            })
            .child(self.terminal.clone())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::sync::Mutex;

    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use gpui::{TestAppContext, VisualTestContext};
    use shelbi_client::SessionEvent;

    use super::*;
    use crate::elements::terminal::session::SessionIo;
    use shelbi_app::launch::remote::RemoteError;
    use shelbi_app::launch::FakeRemote;

    thread_local! {
        /// The sessions [`open_remote_session`] was asked to open, in order.
        pub(crate) static OPENED: RefCell<Vec<RemoteSession>> = const { RefCell::new(Vec::new()) };
        /// The sessions [`open_terminal_view`] was asked to open, in order.
        pub(crate) static TERMINAL_OPENED: RefCell<Vec<RemoteSession>> = const { RefCell::new(Vec::new()) };
    }

    struct NoIo;

    impl SessionIo for NoIo {
        fn input(&self, _: Vec<u8>) {}
        fn paste(&self, _: String, _: bool) {}
        fn resize(&self, _: Size) {}
    }

    /// Unreachable for the first `down` connects, then live; keeps each
    /// attach's sender to push an exit through.
    struct FakeTerminal {
        down: Mutex<u32>,
        streams: Mutex<Vec<UnboundedSender<SessionEvent>>>,
    }

    impl FakeTerminal {
        fn exit(&self, code: i32) {
            let streams = self.streams.lock().unwrap();
            streams
                .last()
                .unwrap()
                .unbounded_send(SessionEvent::Exited(shelbi_proto::Exited {
                    code: Some(code),
                    signal: None,
                    reason: None,
                }))
                .unwrap();
        }
    }

    impl SessionConnector for FakeTerminal {
        fn connect(&self, _: &str, _: &SessionRef, _: RetryPolicy) -> Result<Connected, ConnectFailure> {
            let mut down = self.down.lock().unwrap();
            if *down > 0 {
                *down -= 1;
                return Err(ConnectFailure::Unreachable("Network is unreachable".into()));
            }
            let (tx, rx) = unbounded();
            self.streams.lock().unwrap().push(tx);
            Ok(Connected {
                link: Box::new(NoIo),
                events: rx,
                size: Size::new(80, 24),
                name: "remote/devbox/shelbi".into(),
                cwd: None,
            })
        }
    }

    fn session() -> RemoteSession {
        RemoteSession {
            host: "devbox".into(),
            user: "jlong".into(),
            project: "shelbi".into(),
            label: Some("Shelbi".into()),
            path: "~/Workspaces/shelbi".into(),
        }
    }

    fn open_fake(
        cx: &mut TestAppContext,
        down: u32,
    ) -> (WindowHandle<RemoteSessionView>, VisualTestContext, Arc<FakeTerminal>, Rc<std::cell::Cell<bool>>) {
        open_fake_with(cx, down, None)
    }

    fn open_fake_with(
        cx: &mut TestAppContext,
        down: u32,
        notice: Option<String>,
    ) -> (WindowHandle<RemoteSessionView>, VisualTestContext, Arc<FakeTerminal>, Rc<std::cell::Cell<bool>>) {
        crate::views::about::tests::init(cx);
        let fake = Arc::new(FakeTerminal {
            down: Mutex::new(down),
            streams: Mutex::new(Vec::new()),
        });
        let closed = Rc::new(std::cell::Cell::new(false));
        let on_close: OnClose = {
            let closed = closed.clone();
            Box::new(move || closed.set(true))
        };
        let connector = fake.clone();
        let handle = cx
            .update(|cx| open_with(session(), connector, Some(on_close), notice, cx))
            .expect("the window opens");
        let vcx = VisualTestContext::from_window(handle.into(), cx);
        vcx.run_until_parked();
        (handle, vcx, fake, closed)
    }

    fn status(handle: WindowHandle<RemoteSessionView>, cx: &mut VisualTestContext) -> (String, Connection) {
        handle.update(cx, |view, _, cx| view.status(cx)).unwrap()
    }

    #[gpui::test]
    fn the_window_names_who_and_where_its_connected(cx: &mut TestAppContext) {
        let (handle, mut vcx, _, _) = open_fake(cx, 0);
        assert_eq!(
            status(handle, &mut vcx),
            ("Connected to jlong@devbox · Shelbi".to_string(), Connection::Connected)
        );
        assert!(vcx.debug_bounds("remote-titlebar").is_some());
        assert!(vcx.debug_bounds("remote-dot").is_some());
    }

    #[gpui::test]
    fn a_dropped_ssh_shows_the_reconnect_notice_then_comes_back(cx: &mut TestAppContext) {
        let (handle, mut vcx, fake, closed) = open_fake(cx, 0);
        // `ssh` exits 255: the link dropped. The next attempt finds the host
        // down once (the fake), so the view waits out the backoff.
        *fake.down.lock().unwrap() = 1;
        fake.exit(255);
        vcx.run_until_parked();
        assert_eq!(status(handle, &mut vcx).1, Connection::Reconnecting);
        let notice = handle
            .update(&mut vcx, |view, _, cx| view.terminal().read(cx).notice())
            .unwrap();
        assert_eq!(
            notice.as_deref(),
            Some("Reconnecting to devbox… (attempt 2) Network is unreachable")
        );
        // The view's reconnect backoff runs on the wall clock (500ms first).
        std::thread::sleep(std::time::Duration::from_millis(600));
        vcx.executor().advance_clock(std::time::Duration::from_secs(1));
        vcx.run_until_parked();
        assert_eq!(status(handle, &mut vcx).1, Connection::Connected);
        assert!(!closed.get(), "a drop never closes the window");
    }

    #[gpui::test]
    fn quitting_the_far_shelbi_closes_the_window_and_ends_the_local_ssh(cx: &mut TestAppContext) {
        let (handle, mut vcx, fake, closed) = open_fake(cx, 0);
        fake.exit(0);
        vcx.run_until_parked();
        assert!(handle.update(&mut vcx, |_, _, _| ()).is_err(), "the window is gone");
        assert!(closed.get());
    }

    #[gpui::test]
    fn a_window_opened_for_another_version_says_why_under_its_titlebar(cx: &mut TestAppContext) {
        let line = "Shelbi on devbox is 0.12.1 and this Mac has 0.13.1, so it opens in its terminal view. \
                    Update both to get the full window.";
        let (handle, mut vcx, _, _) = open_fake_with(cx, 0, Some(line.to_string()));
        let notice = handle.update(&mut vcx, |view, _, _| view.notice().map(str::to_string)).unwrap();
        assert_eq!(notice.as_deref(), Some(line));
        assert!(vcx.debug_bounds("remote-notice").is_some());
        assert_eq!(status(handle, &mut vcx).1, Connection::Connected, "the terminal view runs as ever");
    }

    #[gpui::test]
    fn with_no_notice_the_window_is_todays(cx: &mut TestAppContext) {
        let (handle, mut vcx, _, _) = open_fake(cx, 0);
        assert!(handle.update(&mut vcx, |view, _, _| view.notice().is_none()).unwrap());
        assert!(vcx.debug_bounds("remote-notice").is_none());
    }

    /// The link a test must never open: deciding the terminal view never
    /// reaches for it.
    fn no_link(_: &str) -> Arc<HostLink> {
        panic!("the terminal view opens no relay link")
    }

    #[test]
    fn a_host_from_before_the_native_window_opens_the_terminal_view_with_the_notice() {
        // An old far Shelbi: its listing has no native capability.
        let remote = Arc::new(
            FakeRemote::new()
                .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
                .with_far_version("devbox", "0.0.1", None),
        );
        let Decided::Terminal { notice: Some(notice) } = decide_with(&session(), remote, &no_link) else {
            panic!("an old host opens the terminal view, saying why");
        };
        assert!(notice.starts_with("Shelbi on devbox is 0.0.1 and "), "{notice}");
        assert!(notice.contains("so it opens in its terminal view"), "{notice}");
    }

    #[test]
    fn a_host_another_patch_release_apart_opens_the_terminal_view() {
        // The same protocols, a patch release apart: exact versions only.
        let mut parts: Vec<u64> = env!("CARGO_PKG_VERSION")
            .split(['.', '-'])
            .take(3)
            .map(|p| p.parse().unwrap())
            .collect();
        parts[2] += 1;
        let other = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
        let remote = Arc::new(
            FakeRemote::new()
                .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
                .with_far_version("devbox", &other, Some(shelbi_app::launch::NativeCapability::current())),
        );
        let Decided::Terminal { notice: Some(notice) } = decide_with(&session(), remote, &no_link) else {
            panic!("a patch release apart opens the terminal view");
        };
        assert!(notice.contains(&other), "{notice}");
    }

    #[test]
    fn another_far_version_opens_the_terminal_view_with_the_notice() {
        let remote = Arc::new(
            FakeRemote::new()
                .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
                .with_far_version("devbox", "0.0.1", Some(shelbi_app::launch::NativeCapability::current())),
        );
        let Decided::Terminal { notice: Some(notice) } = decide_with(&session(), remote, &no_link) else {
            panic!("another version opens the terminal view, saying why");
        };
        let here = if cfg!(target_os = "macos") { "this Mac" } else { "this computer" };
        assert_eq!(
            notice,
            format!(
                "Shelbi on devbox is 0.0.1 and {here} has {}, so it opens in its terminal view. \
                 Update both to get the full window.",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn the_same_version_opens_the_full_window_over_the_link() {
        let far = crate::remote_fixture::FarHost::new("decide");
        let link = far.link.clone();
        let decided = decide_with(&far.session(), far.remote.clone(), &move |_| link.clone());
        let Decided::Native { listing, link } = decided else {
            panic!("the same version opens natively");
        };
        assert_eq!(listing.projects[0].name, crate::remote_fixture::PROJECT);
        assert!(link.is_up());
    }

    /// A host that only takes a password: the first step to reach it asks the
    /// user, and everything after rides that sign-in, as the shared
    /// connection does for real. `accept` is the user's answer.
    struct PasswordOnly {
        far: Arc<FakeRemote>,
        accept: bool,
        asked: Arc<std::sync::atomic::AtomicU32>,
        signed_in: Arc<std::sync::atomic::AtomicBool>,
    }

    impl PasswordOnly {
        fn new(far: Arc<FakeRemote>, accept: bool) -> PasswordOnly {
            PasswordOnly {
                far,
                accept,
                asked: Arc::default(),
                signed_in: Arc::default(),
            }
        }

        fn sign_in(&self, host: &str) -> Result<(), RemoteError> {
            use std::sync::atomic::Ordering::SeqCst;
            if self.signed_in.load(SeqCst) {
                return Ok(());
            }
            self.asked.fetch_add(1, SeqCst);
            if !self.accept {
                return Err(denied(host, true));
            }
            self.signed_in.store(true, SeqCst);
            Ok(())
        }

        /// The relay's link to this host: it opens over `channel` when a sign-in
        /// is there to ride, and with none, as a key-only `ssh` would, it's
        /// refused.
        fn link(&self, channel: Arc<dyn shelbi_app::source::remote::RelayLink>) -> Arc<HostLink> {
            let signed_in = self.signed_in.clone();
            Arc::new(HostLink::new("alex@studio", move |host| {
                if signed_in.load(std::sync::atomic::Ordering::SeqCst) {
                    Ok(channel.clone())
                } else {
                    Err(denied(host, false))
                }
            }))
        }
    }

    fn denied(host: &str, asked: bool) -> RemoteError {
        RemoteError::AuthFailed {
            host: host.into(),
            reason: format!("{host}: Permission denied (publickey,password,keyboard-interactive)."),
            asked,
        }
    }

    impl RemoteConnector for PasswordOnly {
        fn find_sessions(&self, host: &str) -> Result<(String, Vec<RemoteSession>), RemoteError> {
            self.sign_in(host)?;
            self.far.find_sessions(host)
        }
        fn connect(&self, session: &RemoteSession) -> Result<(), RemoteError> {
            self.sign_in(&session.host)?;
            self.far.connect(session)
        }
        fn start_daemon(&self, host: &str) -> Result<(), RemoteError> {
            self.sign_in(host)?;
            self.far.start_daemon(host)
        }
        fn listing(&self, host: &str) -> Result<RemoteListing, RemoteError> {
            self.sign_in(host)?;
            self.far.listing(host)
        }
    }

    fn studio_session() -> RemoteSession {
        RemoteSession {
            host: "alex@studio".into(),
            user: "alex".into(),
            project: "dxc4".into(),
            label: None,
            path: "~/dxc4".into(),
        }
    }

    fn studio() -> Arc<FakeRemote> {
        Arc::new(FakeRemote::new().with_host("alex@studio", "alex", &[("dxc4", "~/dxc4")]))
    }

    #[test]
    fn a_password_only_host_opens_the_full_window_after_one_sign_in() {
        let far = crate::remote_fixture::FarHost::new("password");
        let host = Arc::new(PasswordOnly::new(studio(), true));
        let link = host.link(far.link.clone());
        let asked = host.asked.clone();
        let opened = link.clone();
        let decided = decide_with(&studio_session(), host, &move |_| opened.clone());
        assert!(matches!(decided, Decided::Native { .. }), "the full window, not the terminal view");
        assert!(link.is_up());
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1, "the user was asked once");
    }

    #[test]
    fn a_link_that_cant_sign_in_says_what_to_do() {
        // The steps got in (their connection is up), but the relay couldn't
        // use it and had no way to ask: the banner from the report.
        let far = Arc::new(FakeRemote::new().with_host("alex@studio", "alex", &[("dxc4", "~/dxc4")]));
        let link = Arc::new(HostLink::new("alex@studio", |host| Err(denied(host, false))));
        let decided = decide_with(&studio_session(), far, &move |_| link.clone());
        let Decided::Terminal { notice: Some(notice) } = decided else {
            panic!("it opens the terminal view, saying why");
        };
        assert!(notice.contains("ssh-copy-id alex@studio"), "{notice}");
        assert!(notice.contains("set up SSH key login to alex@studio"), "{notice}");
        assert!(notice.contains("terminal view"), "{notice}");
        assert!(notice.contains("Permission denied"), "ssh's own words stay: {notice}");
    }

    #[test]
    fn opening_a_host_again_may_ask_again_after_a_no() {
        // The window's reconnect heard No, and the link holds that answer;
        // the user opening the host afresh has changed their mind.
        let far = crate::remote_fixture::FarHost::new("again");
        let channel = far.link.clone();
        let link = Arc::new(HostLink::new("alex@studio", move |_| {
            Ok(channel.clone() as Arc<dyn shelbi_app::source::remote::RelayLink>)
        }));
        let no = || Err(denied("alex@studio", true));
        assert!(link.connect(&no).is_err());
        assert!(link.connect(&|| Ok(())).is_err(), "held: nothing asks");
        let opened = link.clone();
        let decided = decide_with(&studio_session(), studio(), &move |_| opened.clone());
        assert!(matches!(decided, Decided::Native { .. }));
    }

    #[test]
    fn a_stopped_far_daemon_is_never_started_by_deciding() {
        let remote = Arc::new(
            FakeRemote::new()
                .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
                .with_stopped_daemon("devbox"),
        );
        let link = Arc::new(HostLink::new("devbox", |_| panic!("no channel opens for a stopped daemon")));
        let opened = link.clone();
        let decided = decide_with(&session(), remote.clone(), &move |_| opened.clone());
        let Decided::Terminal { notice: Some(notice) } = decided else {
            panic!("a stopped far daemon opens nothing natively");
        };
        assert!(notice.contains("Shelbi isn't running on devbox."), "{notice}");
        assert!(remote.started().is_empty(), "the far daemon is never started implicitly");
    }
}
