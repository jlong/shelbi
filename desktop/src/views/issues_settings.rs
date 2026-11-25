//! Settings ▸ Issues (`settings-issues-page`): where the project's issues
//! live. The page's title and help over the tracker section
//! ([`TrackerSection`], over the shared `TrackerForm` the TUI's Issues page
//! draws too).
//!
//! ```text
//!  Issues
//!  Where this project's issues live. A project uses one external tracker
//!  at most: GitHub or Linear.
//!
//!  (•) Local board   ( ) GitHub Issues   ( ) Linear
//!  ...the chosen board's fields, the agent rules...
//!                                               [Cancel] [Connect]
//! ```
//!
//! The project's config is read off the UI thread through the window's
//! source (`workflow_config`, which carries the `issue_tracker:` block), and
//! read again each time the page is shown and after a connection lands.
//! Nothing is written until Connect (see [`TrackerSection`]).

use gpui::{div, prelude::*, px, Context, Entity, FontWeight, Subscription, Window};
use shelbi_app::settings::tracker::HELP;
use shelbi_app::settings::SettingsPage;
use shelbi_app::source::ProjectHandle;
use shelbi_state::workflow_config::WorkflowConfig;

use super::tracker_section::{tracker_service, TrackerEvent, TrackerSection};
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// The page title's size (Desktop/Heading, as the Properties page's).
const TITLE_SIZE: f32 = 18.0;

/// The Issues page.
pub struct IssuesSettingsPage {
    handle: ProjectHandle,
    tracker: Entity<TrackerSection>,
    /// Whether the project's connection has been read.
    loaded: bool,
    /// Why the project's config didn't read, if it didn't.
    unreadable: Option<String>,
    /// Re-read the config on the next draw (the page was shown again).
    reload: bool,
    _subscriptions: Vec<Subscription>,
}

impl IssuesSettingsPage {
    pub fn new(handle: &ProjectHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tracker = cx.new(|cx| TrackerSection::new(tracker_service(handle), window, cx));
        let subscriptions =
            vec![cx.subscribe_in(&tracker, window, |this, _, event: &TrackerEvent, window, cx| match event {
                // The project changed under the page: show it as written.
                TrackerEvent::Connected => {
                    if !cfg!(test) {
                        this.load(window, cx);
                    }
                }
            })];
        let mut page = IssuesSettingsPage {
            handle: handle.clone(),
            tracker,
            loaded: false,
            unreadable: None,
            reload: false,
            _subscriptions: subscriptions,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(window, cx);
        }
        page
    }

    /// Read the project's config off the UI thread and show it.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.handle.source.clone();
        let read = cx.background_executor().spawn(async move { source.workflow_config() });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(config, window, cx)).ok();
        })
        .detach();
    }

    /// The config was read (a test hands it in).
    pub fn loaded(&mut self, config: Result<WorkflowConfig, String>, window: &mut Window, cx: &mut Context<Self>) {
        match config {
            Ok(config) => {
                self.unreadable = None;
                self.loaded = true;
                let cfg = config.issue_tracker().config.clone();
                self.tracker.update(cx, |t, cx| t.load(&cfg, window, cx));
            }
            Err(e) => self.unreadable = Some(e),
        }
        cx.notify();
    }

    /// Re-read the config when next drawn (the page was shown again). A
    /// test's page reads no disk.
    pub fn reload_when_drawn(&mut self) {
        self.reload = !cfg!(test);
    }

    /// The tracker section.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn tracker(&self) -> Entity<TrackerSection> {
        self.tracker.clone()
    }

    fn header(theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .type_style(Type::Heading)
                    .text_size(px(TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_loud)
                    .child(SettingsPage::Issues.label()),
            )
            .child(
                div()
                    .debug_selector(|| "issues-settings-subtitle".into())
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child(HELP),
            )
    }
}

impl Render for IssuesSettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let unreadable = self.unreadable.as_ref().map(|e| {
            div()
                .debug_selector(|| "issues-settings-unreadable".into())
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(format!("The project's settings don't read: {e}"))
        });
        div()
            .id("settings-issues")
            .debug_selector(|| "settings-issues".into())
            .flex()
            .flex_col()
            .w_full()
            .gap_6()
            .child(Self::header(&theme))
            .children(unreadable)
            .children(self.loaded.then(|| self.tracker.clone()))
    }
}
