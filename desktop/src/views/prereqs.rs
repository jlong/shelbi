//! The startup requirements check (Figma "00 · Requirements · …": Checking
//! 2840:13076, Missing tools 2840:13181, Fresh Mac 2840:13378, Locate gh
//! 2840:13528, All set 2840:13657, Skip confirmation 2843:13163; the
//! `Requirement` and `Command` components, 2839:12955 and 2839:12815).
//!
//! The shared [`shelbi_app::prereqs`] model in the wizard's 800 × 580
//! window: a row per tool Shelbi depends on (Git, GitHub CLI, a coding
//! agent, Homebrew), found ones compact with their version and path, missing
//! ones with what to do, the install command(s) to copy and Locate… for a
//! binary outside the login `PATH`. The list scrolls inside the window,
//! clipped above the actions with a fade into the window and a scrollbar;
//! the window never grows and the actions never move.
//!
//! Every launch checks ([`main`](crate) calls one of the two):
//!
//! - **First launch** ([`show_first_launch`]): the window opens on
//!   "Checking your tools" and becomes the CLI setup
//!   ([`super::cli_setup`]) by itself when everything is found; when
//!   something isn't, once the user continues or skips.
//! - **Every other launch** ([`check_in_background`]): the windows open as
//!   usual while the tools are checked quietly off the UI thread (existence
//!   only, nothing run); only a missing tool opens this window, in front.
//!   Continue or Skip anyway closes it.
//!
//! Nothing re-runs on a timer: Check again runs every check once more.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, AnyElement, AnyWindowHandle, App, ClipboardItem,
    Context, Entity, FocusHandle, Focusable, KeyDownEvent, PathPromptOptions, ScrollHandle, SharedString, Stateful,
    Subscription, Window,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use shelbi_app::prereqs::{
    self as model, CheckEvent, Found, InstallCommand, Phase, Platform, Prereqs, Probe, RowState, SystemProbe, Tool,
    ToolPaths,
};

use crate::app::CloseWindow;
use crate::assets::{heroicon, icons};
use crate::elements::controls;
use crate::elements::spinner::spinner_sized;
use crate::elements::terminal::metrics::PRIMARY_FAMILY as MONO_FAMILY;
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::{self, Theme};
use crate::views::add_project::SIZE;

/// The window's padding around the step (the frames' `Step content`).
const PAD: f32 = 48.0;
/// Room the scrollbar takes beside the rows while the list scrolls.
const SCROLLBAR_GUTTER: f32 = 14.0;
/// The fade over the list's foot while it scrolls.
const FADE_H: f32 = 64.0;
/// A command's label column in the coding-agent row.
const AGENT_LABEL_W: f32 = 84.0;

/// How the screen ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Everything's there.
    Continue,
    /// Skip anyway.
    Skip,
}

/// What the window does once the check is done with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    /// Become the first launch's CLI setup (the same window).
    CliSetup,
    /// Close (the launch's windows are open behind it).
    Close,
}

/// Remembers a located binary (runs off the UI thread).
type SaveFn = dyn Fn(&Found) -> Result<(), String> + Send + Sync;
/// Leaves the screen.
type DoneFn = dyn Fn(Exit, AnyWindowHandle, &mut Window, &mut App);

/// What the view runs; a test swaps in a fake machine and records where it
/// went.
#[derive(Clone)]
pub struct Deps {
    pub probe: Arc<dyn Probe>,
    pub located: Arc<dyn Fn() -> ToolPaths + Send + Sync>,
    pub save: Arc<SaveFn>,
    pub done: Rc<DoneFn>,
    pub home: Option<PathBuf>,
    pub platform: Platform,
}

impl Deps {
    /// The real machine, going on to `then`.
    pub fn real(then: Then) -> Self {
        Deps {
            probe: Arc::new(SystemProbe::login()),
            located: Arc::new(shelbi_state::tool_paths::located),
            save: Arc::new(model::save_located),
            done: Rc::new(move |exit, _, window, cx| finish(then, exit, window, cx)),
            home: shelbi_core::home_dir(),
            platform: Platform::current(),
        }
    }
}

/// The real way on: the CLI setup in this window, or close it.
fn finish(then: Then, exit: Exit, window: &mut Window, cx: &mut App) {
    tracing::info!(?exit, ?then, "requirements check done");
    match then {
        Then::CliSetup => super::cli_setup::replace_root(window, cx),
        Then::Close => window.remove_window(),
    }
}

/// The first launch: the requirements window, checking, which becomes the
/// CLI setup.
pub fn show_first_launch(cx: &mut App) {
    open(Then::CliSetup, cx);
}

/// Any other launch: check quietly off the UI thread and open the
/// requirements window only when something's missing.
pub fn check_in_background(cx: &mut App) {
    let platform = Platform::current();
    let task = cx.background_executor().spawn(async move {
        let located = shelbi_state::tool_paths::located();
        model::quick_check(&located, platform, &SystemProbe::login())
    });
    cx.spawn(async move |cx| {
        if !task.await {
            tracing::info!("a required tool is missing: the requirements window");
            cx.update(|cx| open(Then::Close, cx));
        }
    })
    .detach();
}

fn open(then: Then, cx: &mut App) {
    let opened = cx.open_window(super::about::options("Shelbi", SIZE, false, cx), |window, cx| {
        cx.new(|cx| PrereqsView::new(Deps::real(then), window, cx))
    });
    match opened {
        Ok(handle) => {
            if then == Then::CliSetup {
                super::add_project::opened(handle.into(), cx);
            }
            cx.activate(true);
        }
        Err(e) => tracing::error!(error = %e, "could not open the requirements window"),
    }
}

/// The requirements screen.
pub struct PrereqsView {
    model: Prereqs,
    deps: Deps,
    focus: FocusHandle,
    window: AnyWindowHandle,
    path_input: Entity<InputState>,
    scroll: ScrollHandle,
    /// The command whose Copy reads "Copied", and the click that did it (a
    /// later click restarts the two seconds).
    copied: Option<(String, u64)>,
    clicks: u64,
    skip_dialog: bool,
    validating: bool,
    /// Go on by itself while the first check finds everything.
    auto_continue: bool,
    save_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl PrereqsView {
    pub fn new(deps: Deps, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        let (path_input, subscription) = Self::path_field("", window, cx);
        let mut this = PrereqsView {
            model: Prereqs::new(deps.platform),
            deps,
            focus,
            window: window.window_handle(),
            path_input,
            scroll: ScrollHandle::new(),
            copied: None,
            clicks: 0,
            skip_dialog: false,
            validating: false,
            auto_continue: true,
            save_error: None,
            _subscriptions: vec![subscription],
        };
        this.check_again(cx);
        this
    }

    /// A Locate path field: Return checks the path.
    fn path_field(placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> (Entity<InputState>, Subscription) {
        let placeholder = placeholder.to_string();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                if let Some(tool) = this.model.locating() {
                    this.validate(tool, cx);
                }
            }
        });
        (input, subscription)
    }

    /// The model (tests).
    #[cfg(test)]
    pub fn model(&self) -> &Prereqs {
        &self.model
    }

    /// Whether the skip confirmation is open (tests).
    #[cfg(test)]
    pub fn skip_dialog_open(&self) -> bool {
        self.skip_dialog
    }

    /// Show a state the checks didn't reach (the visual check's frames):
    /// `edit` the model, open the skip confirmation or not, and type `path`
    /// in an open Locate field.
    #[cfg(feature = "visual-check")]
    pub(crate) fn preview(
        &mut self,
        edit: impl FnOnce(&mut Prereqs),
        skip_dialog: bool,
        path: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.auto_continue = false;
        edit(&mut self.model);
        if let (Some(tool), Some(path)) = (self.model.locating(), path) {
            let (input, subscription) = Self::path_field(&tool.locate_placeholder(), window, cx);
            input.update(cx, |s, cx| s.set_value(path.to_string(), window, cx));
            self.path_input = input;
            self._subscriptions = vec![subscription];
        }
        self.skip_dialog = skip_dialog;
        cx.notify();
    }

    /// Run every check, off the UI thread, each row as it finishes.
    pub(crate) fn check_again(&mut self, cx: &mut Context<Self>) {
        let run = self.model.begin_check();
        self.save_error = None;
        let (tx, rx) = async_channel::unbounded::<CheckEvent>();
        let (probe, located_fn, platform) = (self.deps.probe.clone(), self.deps.located.clone(), self.deps.platform);
        cx.background_executor()
            .spawn(async move {
                let located = located_fn();
                model::run_check(run, &located, platform, probe.as_ref(), |e| {
                    let _ = tx.send_blocking(e);
                });
            })
            .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this.update(cx, |this, cx| {
                    if this.model.apply(event) {
                        cx.notify();
                    }
                }).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| this.checked(run, cx));
        })
        .detach();
        cx.notify();
    }

    /// A check run finished: the first one finding everything goes on.
    fn checked(&mut self, run: u64, cx: &mut Context<Self>) {
        if run != self.model.run() {
            return;
        }
        match self.model.phase() {
            Phase::Ready if self.auto_continue => {
                self.auto_continue = false;
                self.leave(Exit::Continue, cx);
            }
            _ => self.auto_continue = false,
        }
    }

    fn leave(&mut self, exit: Exit, cx: &mut Context<Self>) {
        let (done, handle) = (self.deps.done.clone(), self.window);
        // After this frame: the window's root may be replaced.
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| done(exit, handle, window, cx));
        });
    }

    pub(crate) fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.clicks += 1;
        let click = self.clicks;
        self.copied = Some((text, click));
        let timer = cx.background_executor().timer(model::COPIED_FOR);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.as_ref().is_some_and(|(_, c)| *c == click) {
                    this.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn start_locate(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        self.model.start_locate(tool);
        self.save_error = None;
        let (input, subscription) = Self::path_field(&tool.locate_placeholder(), window, cx);
        self.path_input = input;
        self._subscriptions = vec![subscription];
        let focus = self.path_input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    fn browse(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            if let Some(path) = picked {
                let _ = this.update_in(cx, |this, window, cx| {
                    let text = path.display().to_string();
                    this.path_input.update(cx, |s, cx| s.set_value(text, window, cx));
                    this.validate(tool, cx);
                });
            }
        })
        .detach();
    }

    /// Check the Locate field's path off the UI thread, and remember a good
    /// one.
    pub(crate) fn validate(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.validating {
            return;
        }
        self.validating = true;
        let input = self.path_input.read(cx).value().to_string();
        let (probe, save, home, platform) =
            (self.deps.probe.clone(), self.deps.save.clone(), self.deps.home.clone(), self.deps.platform);
        let task = cx.background_executor().spawn(async move {
            let result = model::validate_located(tool, &input, home.as_deref(), platform, probe.as_ref());
            let saved = match &result {
                Ok(found) => save(found).err(),
                Err(_) => None,
            };
            (result, saved)
        });
        cx.spawn(async move |this, cx| {
            let (result, save_error) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.validating = false;
                this.save_error = save_error.map(|e| format!("Couldn't remember this location: {e}"));
                this.model.located(tool, result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn skip(&mut self, cx: &mut Context<Self>) {
        if self.model.phase() == Phase::Missing {
            self.skip_dialog = true;
            cx.notify();
        }
    }

    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) {
        self.skip_dialog = false;
        cx.notify();
    }

    pub(crate) fn skip_anyway(&mut self, cx: &mut Context<Self>) {
        self.skip_dialog = false;
        self.leave(Exit::Skip, cx);
    }

    pub(crate) fn continue_on(&mut self, cx: &mut Context<Self>) {
        if self.model.phase() == Phase::Ready {
            self.leave(Exit::Continue, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.modified() {
            return;
        }
        match k.key.as_str() {
            "escape" if self.skip_dialog => self.go_back(cx),
            "escape" if self.model.locating().is_some() => {
                self.model.cancel_locate();
                self.focus.focus(window, cx);
                cx.notify();
            }
            "enter" if self.skip_dialog => self.skip_anyway(cx),
            "enter" if self.model.locating().is_none() => self.continue_on(cx),
            _ => {}
        }
    }

    // ---- drawing -------------------------------------------------------

    fn icon(&self, tool: Tool, theme: &Theme) -> AnyElement {
        let slot = || div().flex_none().size(px(20.0)).flex().items_center().justify_center();
        if self.model.locating() == Some(tool) {
            return slot().child(heroicon(icons::FOLDER).text_color(theme.icon_cyan)).into_any_element();
        }
        match self.model.state(tool) {
            RowState::Found(_) => slot().child(heroicon(icons::CHECK).text_color(theme.icon_green)).into_any_element(),
            RowState::Missing => slot().child(heroicon(icons::X_MARK).text_color(theme.error)).into_any_element(),
            RowState::Checking => slot()
                .child(spinner_sized(SharedString::from(format!("prereq-spin-{tool:?}")), theme.progress, 20.0))
                .into_any_element(),
            RowState::Pending => slot()
                .child(div().size(px(12.0)).rounded_full().border(px(1.25)).border_color(theme.muted))
                .into_any_element(),
        }
    }

    /// A row's card: `surface/depth-10` with the divider's edge; a row not
    /// checked yet sits on the canvas.
    fn card(&self, tool: Tool, theme: &Theme) -> Stateful<gpui::Div> {
        let pending = matches!(self.model.state(tool), RowState::Pending);
        div()
            .id(SharedString::from(format!("prereq-row-{tool:?}")))
            .debug_selector(move || format!("prereq-row-{tool:?}"))
            .flex()
            .flex_row()
            .flex_none()
            .w_full()
            .gap(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.divider)
            .bg(if pending { theme.canvas } else { theme.toolbar })
    }

    fn heading(&self, tool: Tool, theme: &Theme) -> gpui::Div {
        let pending = matches!(self.model.state(tool), RowState::Pending);
        div()
            .type_style(Type::BaseSemibold)
            .text_color(if pending { theme.muted } else { theme.text_loud })
            .whitespace_nowrap()
            .child(tool.name())
    }

    fn row(&self, tool: Tool, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.model.locating() == Some(tool) {
            return self.locate_row(tool, theme, window, cx);
        }
        let home = self.deps.home.as_deref();
        let detail = match self.model.state(tool) {
            RowState::Missing => return self.missing_row(tool, theme, cx),
            RowState::Found(found) => found.detail(home),
            RowState::Checking => model::CHECKING_ROW.to_string(),
            RowState::Pending => model::WAITING_ROW.to_string(),
        };
        self.card(tool, theme)
            .items_center()
            .px(px(16.0))
            .py(px(12.0))
            .child(self.icon(tool, theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.0))
                    .child(self.heading(tool, theme))
                    .child(div().flex_1())
                    .child(
                        div()
                            .debug_selector(move || format!("prereq-detail-{tool:?}"))
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .type_style(Type::Small)
                            .text_color(theme.muted)
                            .child(detail),
                    ),
            )
            .into_any_element()
    }

    fn missing_row(&self, tool: Tool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let commands = self.model.install_commands(tool);
        let command_rows = commands.into_iter().map(|c| match c.label {
            Some(label) => div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(AGENT_LABEL_W))
                        .type_style(Type::SmallMedium)
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(self.command(&c, theme, cx).flex_1())
                .into_any_element(),
            None => self.command(&c, theme, cx).w_full().into_any_element(),
        }).collect::<Vec<_>>();
        let locate = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(div().type_style(Type::Small).text_color(theme.muted).child(model::LOCATE_PROMPT))
            .child(
                style::button(
                    format!("prereq-locate-{tool:?}"),
                    tool.locate_label(),
                    ButtonKind::Ghost,
                    ButtonSize::Small,
                    false,
                    theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| this.start_locate(tool, window, cx))),
            );
        self.card(tool, theme)
            .items_start()
            .p(px(16.0))
            .child(self.icon(tool, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(self.heading(tool, theme))
                    .child(
                        div()
                            .debug_selector(move || format!("prereq-missing-{tool:?}"))
                            .type_style(Type::Small)
                            .text_color(theme.muted)
                            .child(self.model.missing_detail(tool)),
                    )
                    .children(command_rows)
                    .child(locate),
            )
            .into_any_element()
    }

    /// The Command component: the command on the code fill, Copy (or, for
    /// two seconds after a click, Copied with a green check) at its end.
    fn command(&self, command: &InstallCommand, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let id = format!("prereq-copy-{}", command.binary.command());
        let copied = self.copied.as_ref().is_some_and(|(t, _)| *t == command.text);
        let size = ButtonSize::Small;
        let button = if copied {
            style::button_base(id.clone(), ButtonKind::Ghost, size, false, theme)
                .child(style::left_icon_slot(icons::CHECK, size, theme.icon_green))
                .child(div().debug_selector(move || format!("{id}-label")).child(model::COPIED))
        } else {
            style::content_button(
                id,
                ButtonContent::label(model::COPY).left(icons::DOCUMENT_DUPLICATE),
                ButtonKind::Ghost,
                size,
                false,
                theme,
            )
        };
        let text = command.text.clone();
        let selector = format!("prereq-command-{}", command.binary.command());
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .min_w_0()
            .h(px(36.0))
            .gap(px(8.0))
            .pl(px(12.0))
            .pr(px(4.0))
            .py(px(2.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.code_bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO_FAMILY)
                    .text_size(px(14.0))
                    .line_height(px(18.0))
                    .text_color(theme.code_fg)
                    .child(command.text.clone()),
            )
            .child(button.on_click(cx.listener(move |this, _, _, cx| this.copy(text.clone(), cx))))
    }

    /// The Locate panel: what to choose, the path field and Browse…, and
    /// what checking the path said.
    fn locate_row(&self, tool: Tool, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.path_input.read(cx).focus_handle(cx).is_focused(window);
        let field = controls::textbox(
            div().flex_1().min_w_0().child(
                Input::new(&self.path_input)
                    .appearance(false)
                    .h_full()
                    .px_0()
                    .type_style(Type::Base)
                    .text_color(theme.form_text),
            ),
            focused,
            theme,
        )
        .debug_selector(|| "prereq-locate-path".into())
        .flex_1();
        let browse = style::button("prereq-browse", model::BROWSE, ButtonKind::Secondary, ButtonSize::Base, false, theme)
            .on_click(cx.listener(move |this, _, window, cx| this.browse(tool, window, cx)));
        let validation: Option<AnyElement> = if self.validating {
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(spinner_sized("prereq-validating", theme.progress, 16.0))
                    .child(div().type_style(Type::Small).text_color(theme.muted).child("Checking…"))
                    .into_any_element(),
            )
        } else {
            match self.model.locate_result() {
                Some(Ok(found)) => Some(
                    div()
                        .debug_selector(|| "prereq-located".into())
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(heroicon(icons::CHECK_MENU).text_color(theme.icon_green))
                        .child(div().type_style(Type::SmallMedium).text_color(theme.success).child(found.located_message()))
                        .into_any_element(),
                ),
                Some(Err(e)) => Some(
                    div()
                        .debug_selector(|| "prereq-locate-error".into())
                        .type_style(Type::Small)
                        .text_color(theme.error)
                        .child(e.clone())
                        .into_any_element(),
                ),
                None => None,
            }
        };
        let save_error = self
            .save_error
            .clone()
            .map(|e| div().type_style(Type::Small).text_color(theme.error).child(e));
        self.card(tool, theme)
            .items_start()
            .p(px(16.0))
            .child(self.icon(tool, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(self.heading(tool, theme))
                    .child(div().type_style(Type::Small).text_color(theme.foreground).child(tool.locate_body()))
                    .child(div().flex().flex_row().items_center().w_full().gap(px(8.0)).child(field).child(browse))
                    .children(validation)
                    .children(save_error),
            )
            .into_any_element()
    }

    fn actions(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let size = ButtonSize::Base;
        let row = div().flex().flex_row().flex_none().items_center().w_full().gap(px(8.0)).child(div().flex_1());
        match self.model.phase() {
            Phase::Checking => row.child(style::disabled_button("prereq-continue", model::CONTINUE, ButtonKind::Primary, size, theme)),
            Phase::Ready => row.child(
                style::button("prereq-continue", model::CONTINUE, ButtonKind::Primary, size, false, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.continue_on(cx))),
            ),
            Phase::Missing => row
                .child(
                    style::button("prereq-skip", model::SKIP_FOR_NOW, ButtonKind::Secondary, size, false, theme)
                        .on_click(cx.listener(|this, _, _, cx| this.skip(cx))),
                )
                .child(
                    style::button("prereq-check-again", model::CHECK_AGAIN, ButtonKind::Primary, size, false, theme)
                        .on_click(cx.listener(|this, _, _, cx| this.check_again(cx))),
                ),
        }
    }

    /// The list, clipped above the actions: while it scrolls, a fade into
    /// the window over its foot and a scrollbar beside it; when it fits,
    /// centered down the space.
    fn details(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let max = self.scroll.max_offset().y;
        let scrolls = max > px(0.5);
        let rows: Vec<AnyElement> = self.model.tools().into_iter().map(|t| self.row(t, theme, window, cx)).collect();
        let list = div()
            .id("prereq-list")
            .debug_selector(|| "prereq-list".into())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_h_full()
                    .gap(px(8.0))
                    .when(!scrolls, |d| d.justify_center())
                    .when(scrolls, |d| d.pr(px(SCROLLBAR_GUTTER)))
                    .children(rows),
            );
        let canvas = theme.opaque().canvas;
        let fade = scrolls.then(|| {
            div()
                .debug_selector(|| "prereq-fade".into())
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(FADE_H))
                .bg(linear_gradient(180.0, linear_color_stop(canvas.opacity(0.0), 0.0), linear_color_stop(theme.canvas, 1.0)))
        });
        let scrollbar = scrolls.then(|| {
            let bounds = self.scroll.bounds();
            let view = bounds.size.height;
            let content = view + max;
            let track = view - px(4.0);
            let thumb = (track * (view / content)).max(px(24.0));
            let at = (-self.scroll.offset().y / max).clamp(0.0, 1.0);
            let top = px(2.0) + (track - thumb) * at;
            div()
                .debug_selector(|| "prereq-scrollbar".into())
                .absolute()
                .right(px(1.0))
                .top(top)
                .w(px(6.0))
                .h(thumb)
                .rounded(px(3.0))
                .bg(theme.muted)
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(list)
            .children(fade)
            .children(scrollbar)
    }

    /// The skip confirmation over the dimmed screen.
    fn skip_dialog(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (title, body, footer) = self.model.skip_copy();
        let items = self.model.consequences().into_iter().map(|(tool, consequence)| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .w_full()
                .gap(px(10.0))
                .child(div().pt(px(1.0)).child(heroicon(icons::X_MARK_MENU).text_color(theme.error)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .type_style(Type::Small)
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text_loud)
                                .child(tool.name()),
                        )
                        .child(div().text_color(theme.foreground).child(consequence)),
                )
        });
        let panel = div()
            .id("prereq-skip-dialog")
            .debug_selector(|| "prereq-skip-dialog".into())
            .flex()
            .flex_col()
            .w(px(480.0))
            .gap(px(20.0))
            .p(px(24.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.divider_strong)
            .bg(theme.opaque().canvas)
            .shadow(style::dialog_shadow(theme))
            .occlude()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().type_style(Type::Heading).text_color(theme.text_loud).child(title))
                    .child(div().type_style(Type::Small).text_color(theme.foreground).child(body)),
            )
            .child(
                div()
                    .debug_selector(|| "prereq-skip-list".into())
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .bg(theme.search)
                    .children(items),
            )
            .child(div().type_style(Type::Small).text_color(theme.muted).child(footer))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        style::button("prereq-go-back", model::GO_BACK, ButtonKind::Secondary, ButtonSize::Small, false, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
                    )
                    .child(
                        style::button("prereq-skip-anyway", model::SKIP_ANYWAY, ButtonKind::Danger, ButtonSize::Small, false, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.skip_anyway(cx))),
                    ),
            );
        div()
            .id("prereq-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.scrim)
            .occlude()
            .child(panel)
            .into_any_element()
    }
}

impl Focusable for PrereqsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PrereqsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let dialog = self.skip_dialog.then(|| self.skip_dialog(&theme, cx));
        div()
            .id("prereqs")
            .debug_selector(|| "prereqs".into())
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .bg(theme.canvas)
            .font_family(style::UI_FAMILY)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .items_center()
                    .gap(px(24.0))
                    .p(px(PAD))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w_full()
                            .items_center()
                            .gap(px(8.0))
                            .text_center()
                            .child(
                                div()
                                    .debug_selector(|| "prereq-title".into())
                                    .type_style(Type::Title)
                                    .text_color(theme.text_loud)
                                    .child(self.model.title()),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "prereq-body".into())
                                    .w(px(600.0))
                                    .type_style(Type::Base)
                                    .text_color(theme.foreground)
                                    .child(self.model.body()),
                            ),
                    )
                    .child(self.details(&theme, window, cx))
                    .child(self.actions(&theme, cx)),
            )
            .children(dialog)
    }
}

#[cfg(test)]
mod tests;
