//! The bootstrap screens: progress while the app finds Shelbi, a full-window
//! screen for each failure naming its fix, and the relaunch bar shown over the
//! shell when the CLI is upgraded under a running app.
//!
//! Each failure's words are a pure [`Screen`] (tested without gpui); the
//! render functions only lay it out.

use std::path::Path;

use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, IntoElement, SharedString,
};

use crate::assets::{heroicon, icons, Icon};
use crate::platform::discovery::{config_path_label, CliNotFound};
use crate::platform::version::{ProtocolCheck, VersionCheck};
use crate::platform::{BootstrapState, Failure, RelaunchNotice};
use crate::theme::style::{self, ButtonKind};
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

/// The CLI's install command.
pub const INSTALL_COMMAND: &str = "brew install jlong/shelbi/shelbi";
/// The CLI's upgrade command.
pub const UPGRADE_COMMAND: &str = "brew upgrade shelbi";
/// The app's own upgrade command (its cask).
pub const APP_UPGRADE_COMMAND: &str = "brew upgrade --cask shelbi-desktop";
/// Restarting the daemon onto the installed binary.
pub const DAEMON_RESTART_COMMAND: &str = "shelbi daemon restart";

/// The width of a bootstrap screen's column.
const COLUMN_WIDTH: f32 = 560.0;

/// What a screen's primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAction {
    /// Run bootstrap again.
    Retry,
    /// Start a new instance of the app and quit this one.
    Relaunch,
}

impl ScreenAction {
    fn label(self) -> &'static str {
        match self {
            ScreenAction::Retry => "Try Again",
            ScreenAction::Relaunch => "Relaunch Shelbi",
        }
    }

    fn icon(self) -> Icon {
        icons::ARROW_PATH
    }
}

/// One step of a fix: a sentence, optionally with a command to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixStep {
    pub text: String,
    pub command: Option<String>,
}

impl FixStep {
    fn say(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            command: None,
        }
    }

    fn run(text: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            command: Some(command.into()),
        }
    }
}

/// A failure screen's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub title: String,
    /// What happened, a paragraph per entry.
    pub body: Vec<String>,
    /// How to fix it, in order.
    pub fix: Vec<FixStep>,
    /// A labelled block of monospace detail (the paths searched, the
    /// daemon's stderr).
    pub details: Option<(String, Vec<String>)>,
    pub action: ScreenAction,
}

/// The screen for a failure.
pub fn failure_screen(failure: &Failure) -> Screen {
    match failure {
        Failure::CliNotFound(nf) => cli_not_found(nf),
        Failure::DaemonFailedToStart { cli, failure } => {
            let mut body = vec![failure.reason.clone()];
            body.push(format!("The app ran `{} daemon`.", cli.display()));
            let details = if failure.stderr_tail.is_empty() {
                failure
                    .stderr_log
                    .as_ref()
                    .map(|log| (format!("It wrote nothing to {}.", log.display()), Vec::new()))
            } else {
                let from = failure
                    .stderr_log
                    .as_ref()
                    .map(|log| format!(" (full log: {})", log.display()))
                    .unwrap_or_default();
                Some((
                    format!("The daemon's last output{from}:"),
                    failure.stderr_tail.clone(),
                ))
            };
            Screen {
                title: "The Shelbi daemon didn't start".into(),
                body,
                fix: vec![
                    FixStep::run(
                        "Run it in a terminal to see the whole error, fix what it reports, then try again:",
                        format!("{} daemon", shell_quote(cli)),
                    ),
                    FixStep::run("To see what a running daemon reports:", "shelbi daemon status"),
                ],
                details,
                action: ScreenAction::Retry,
            }
        }
        Failure::ControlUnreachable { socket, error } => Screen {
            title: "Can't reach the Shelbi daemon".into(),
            body: vec![
                "The daemon started, but its control socket didn't answer.".into(),
                format!("{}: {error}", socket.display()),
            ],
            fix: vec![FixStep::run(
                "Restart the daemon (agent sessions are separate processes and keep running), then try again:",
                DAEMON_RESTART_COMMAND,
            )],
            details: None,
            action: ScreenAction::Retry,
        },
        Failure::VersionMismatch(check) => version_screen(check),
        Failure::ProtocolMismatch { cli, check } => protocol_screen(cli, check),
    }
}

fn cli_not_found(nf: &CliNotFound) -> Screen {
    let mut body = Vec::new();
    match &nf.setting {
        Some(path) => body.push(format!(
            "desktop.cli_path in {} is {}, which isn't an executable file.",
            config_path_label(),
            path.display()
        )),
        None => body.push(
            "The desktop app runs the shelbi command-line tool, and it isn't installed anywhere the app looked."
                .into(),
        ),
    }
    if let Some(error) = &nf.config_error {
        body.push(format!(
            "config.yaml couldn't be read, so any desktop.cli_path in it was not applied: {error}"
        ));
    }
    let mut fix = Vec::new();
    if nf.setting.is_some() {
        fix.push(FixStep::say(format!(
            "Correct desktop.cli_path in {}, or remove it to let the app search.",
            config_path_label()
        )));
    } else {
        fix.push(FixStep::run("Install it with Homebrew:", INSTALL_COMMAND));
    }
    fix.push(FixStep::run(
        format!(
            "Or tell the app where shelbi is, in {}:",
            config_path_label()
        ),
        "desktop:\n  cli_path: /path/to/shelbi",
    ));
    Screen {
        title: "Shelbi can't find the shelbi command".into(),
        body,
        fix,
        details: Some((
            "Searched:".into(),
            nf.searched
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        )),
        action: ScreenAction::Retry,
    }
}

fn version_screen(check: &VersionCheck) -> Screen {
    match check {
        VersionCheck::AppNewer { app, cli } => Screen {
            title: "The shelbi CLI needs updating".into(),
            body: vec![
                format!("This app is Shelbi {app}, and the installed shelbi CLI is {cli}."),
                "The app and the CLI ship together and must be the same version.".into(),
            ],
            fix: vec![FixStep::run(
                "Upgrade the CLI, then try again:",
                UPGRADE_COMMAND,
            )],
            details: Some((
                "Versions:".into(),
                vec![format!("app  {app}"), format!("cli  {cli}")],
            )),
            action: ScreenAction::Retry,
        },
        VersionCheck::DaemonStale { app, daemon } => Screen {
            title: "The Shelbi daemon is out of date".into(),
            body: vec![format!(
                "The daemon is still running Shelbi {daemon} from before an upgrade. This app and the installed CLI are {app}."
            )],
            fix: vec![FixStep::run(
                "Restart the daemon on the new version (agent sessions are separate processes and keep running), then try again:",
                DAEMON_RESTART_COMMAND,
            )],
            details: Some((
                "Versions:".into(),
                vec![format!("app     {app}"), format!("daemon  {daemon}")],
            )),
            action: ScreenAction::Retry,
        },
        VersionCheck::AppOutdated { app, daemon } => Screen {
            title: "Shelbi was updated".into(),
            body: vec![format!(
                "The shelbi CLI is now {daemon}, and this app is still Shelbi {app}. Relaunch to use the new version."
            )],
            fix: vec![FixStep::run(
                "If it still says this after relaunching, update the app:",
                APP_UPGRADE_COMMAND,
            )],
            details: Some((
                "Versions:".into(),
                vec![format!("app     {app}"), format!("daemon  {daemon}")],
            )),
            action: ScreenAction::Relaunch,
        },
        // Bootstrap never fails with a match; render it as the retry case.
        VersionCheck::Match => Screen {
            title: "Shelbi versions match".into(),
            body: Vec::new(),
            fix: Vec::new(),
            details: None,
            action: ScreenAction::Retry,
        },
    }
}

/// A control-protocol mismatch: the daemon answered, so the screen names which
/// side is behind rather than calling it unreachable.
fn protocol_screen(cli: &Path, check: &ProtocolCheck) -> Screen {
    let protocols = |rows: Vec<(&str, Option<u32>)>| {
        let lines = rows
            .into_iter()
            .map(|(who, p)| match p {
                Some(p) => format!("{who:<7} {p}"),
                None => format!("{who:<7} (too old to report)"),
            })
            .collect();
        Some(("Control protocols:".to_string(), lines))
    };
    match *check {
        ProtocolCheck::CliOlder { app, cli: cli_p, daemon } => Screen {
            title: "The shelbi CLI is older than this app".into(),
            body: vec![match cli_p {
                Some(cli_p) => format!(
                    "{} speaks control protocol {cli_p}; this app needs {app}.",
                    cli.display()
                ),
                None => format!(
                    "The daemon speaks control protocol {daemon}; this app needs {app}. {} is too old to report its own.",
                    cli.display()
                ),
            }],
            fix: vec![FixStep::run(
                format!("Upgrade the CLI ({UPGRADE_COMMAND}), then restart the daemon:"),
                DAEMON_RESTART_COMMAND,
            )],
            details: protocols(vec![
                ("app", Some(app)),
                ("cli", cli_p),
                ("daemon", Some(daemon)),
            ]),
            action: ScreenAction::Retry,
        },
        ProtocolCheck::DaemonStale { app, cli: cli_p, daemon } => Screen {
            title: "The Shelbi daemon is out of date".into(),
            body: vec![format!(
                "The daemon is still running an older shelbi from before an upgrade: it speaks control protocol {daemon}, and {} speaks {cli_p}.",
                cli.display()
            )],
            fix: vec![FixStep::run(
                "Restart the daemon on the new version (agent sessions are separate processes and keep running), then try again:",
                DAEMON_RESTART_COMMAND,
            )],
            details: protocols(vec![
                ("app", Some(app)),
                ("cli", Some(cli_p)),
                ("daemon", Some(daemon)),
            ]),
            action: ScreenAction::Retry,
        },
        ProtocolCheck::AppOutdated { app, daemon } => Screen {
            title: "Shelbi was updated".into(),
            body: vec![format!(
                "The daemon speaks control protocol {daemon}, newer than this app's {app}. Relaunch to use the new version."
            )],
            fix: vec![FixStep::run(
                "If it still says this after relaunching, update the app:",
                APP_UPGRADE_COMMAND,
            )],
            details: protocols(vec![("app", Some(app)), ("daemon", Some(daemon))]),
            action: ScreenAction::Relaunch,
        },
    }
}

/// The relaunch bar's sentence.
pub fn relaunch_message(notice: &RelaunchNotice) -> String {
    let to = notice
        .daemon_version
        .as_deref()
        .filter(|v| *v != notice.app_version)
        .map(|v| format!(" to {v}"))
        .unwrap_or_default();
    format!(
        "Shelbi was updated{to}. Relaunch to keep working: changes are paused until then; open sessions keep streaming."
    )
}

/// `path` for a shell, single-quoted when it needs it.
fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+~".contains(c))
    {
        s
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

// ---- rendering --------------------------------------------------------------

/// The in-flight screen: the wordmark and the current step.
pub fn render_progress(state: &BootstrapState, theme: &Theme) -> AnyElement {
    let label = state.progress_label().unwrap_or_default();
    let detail = match state {
        BootstrapState::StartingDaemon { cli } | BootstrapState::Connecting { cli } => Some(
            format!("{} (from {})", cli.path.display(), cli.source.label()),
        ),
        _ => None,
    };
    centered(theme)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .type_style(Type::Heading)
                        .text_color(theme.accent)
                        .child("Shelbi"),
                )
                .child(div().text_color(theme.foreground).child(format!("{label}…")))
                .when_some(detail, |col, detail| {
                    col.child(div().type_style(Type::XSmall).text_color(theme.muted).child(detail))
                }),
        )
        .into_any_element()
}

/// A failure screen. `on_action` runs the screen's button.
pub fn render_failure(
    screen: &Screen,
    theme: &Theme,
    on_action: impl Fn(ScreenAction, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let action = screen.action;
    let mut column = div()
        .flex()
        .flex_col()
        .gap_4()
        .w(px(COLUMN_WIDTH))
        .max_w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    heroicon(icons::EXCLAMATION_TRIANGLE)
                        .text_color(theme.warning),
                )
                .child(
                    div()
                        .type_style(Type::Heading)
                        .text_color(theme.text_loud)
                        .child(screen.title.clone()),
                ),
        );
    for paragraph in &screen.body {
        column = column.child(div().text_color(theme.foreground).child(paragraph.clone()));
    }
    for (ix, step) in screen.fix.iter().enumerate() {
        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(theme.foreground).child(step.text.clone()));
        if let Some(command) = &step.command {
            block = block.child(command_box(ix, command, theme));
        }
        column = column.child(block);
    }
    if let Some((label, lines)) = &screen.details {
        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(theme.muted).child(label.clone()));
        if !lines.is_empty() {
            block = block.child(
                div()
                    .flex()
                    .flex_col()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.well)
                    .border_1()
                    .border_color(theme.divider)
                    .type_style(Type::XSmall)
                    .text_color(theme.foreground)
                    .children(lines.iter().map(|line| div().child(line.clone()))),
            );
        }
        column = column.child(block);
    }
    column = column.child(
        div().flex().flex_row().child(
            style::icon_label_button(
                "bootstrap-action",
                action.icon(),
                action.label(),
                ButtonKind::Primary,
                theme,
            )
            .on_click(move |_, window, cx| on_action(action, window, cx)),
        ),
    );
    centered(theme).child(column).into_any_element()
}

/// The relaunch bar shown over the shell.
pub fn render_relaunch_bar(
    notice: &RelaunchNotice,
    theme: &Theme,
    on_relaunch: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_2()
        .bg(theme.notice)
        .border_b_1()
        .border_color(theme.divider)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_color(theme.text_loud)
                .child(
                    heroicon(icons::EXCLAMATION_TRIANGLE)
                        .text_color(theme.warning),
                )
                .child(relaunch_message(notice)),
        )
        .child(
            style::button("relaunch", ScreenAction::Relaunch.label(), ButtonKind::Primary, false, theme)
                .on_click(move |_, window, cx| on_relaunch(window, cx)),
        )
        .into_any_element()
}

/// A full-window, centered, scrollable stage.
fn centered(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("bootstrap-stage")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .p_8()
        .overflow_y_scroll()
        .bg(theme.canvas)
        .type_style(Type::Base)
}

/// A command in a monospace box with a copy button.
pub(crate) fn command_box(ix: usize, command: &str, theme: &Theme) -> impl IntoElement {
    let copy: SharedString = command.to_string().into();
    div()
        .flex()
        .flex_row()
        .justify_between()
        .gap_2()
        .items_center()
        .py_1()
        .pl_3()
        .pr_1()
        .rounded_lg()
        .bg(theme.well)
        .border_1()
        .border_color(theme.divider)
        .font_family(crate::elements::terminal::metrics::PRIMARY_FAMILY)
        .type_style(Type::XSmall)
        .child(
            div()
                .flex()
                .flex_col()
                .text_color(theme.text_loud)
                .children(command.lines().map(|line| div().child(line.to_string()))),
        )
        .child(
            style::icon_button(
                SharedString::from(format!("copy-command-{ix}")),
                icons::CLIPBOARD_DOCUMENT,
                style::IconButtonSize::Small,
                false,
                theme,
            )
            .tooltip(style::tooltip("Copy"))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()))
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::daemon::DaemonFailure;
    use std::path::PathBuf;

    fn commands(screen: &Screen) -> Vec<&str> {
        screen
            .fix
            .iter()
            .filter_map(|s| s.command.as_deref())
            .collect()
    }

    #[test]
    fn cli_not_found_lists_the_paths_searched_and_the_setting() {
        let screen = failure_screen(&Failure::CliNotFound(CliNotFound {
            searched: vec![
                PathBuf::from("/usr/bin/shelbi"),
                PathBuf::from("/opt/homebrew/bin/shelbi"),
            ],
            setting: None,
            config_error: None,
        }));
        let (label, lines) = screen.details.clone().unwrap();
        assert_eq!(label, "Searched:");
        assert_eq!(lines, ["/usr/bin/shelbi", "/opt/homebrew/bin/shelbi"]);
        assert!(commands(&screen).contains(&INSTALL_COMMAND));
        assert!(commands(&screen).iter().any(|c| c.contains("cli_path:")));
        assert_eq!(screen.action, ScreenAction::Retry);
    }

    #[test]
    fn a_bad_setting_is_named_instead_of_offering_an_install() {
        let screen = failure_screen(&Failure::CliNotFound(CliNotFound {
            searched: vec![PathBuf::from("/nope/shelbi")],
            setting: Some(PathBuf::from("/nope/shelbi")),
            config_error: None,
        }));
        assert!(screen.body[0].contains("/nope/shelbi"), "{:?}", screen.body);
        assert!(!commands(&screen).contains(&INSTALL_COMMAND));
    }

    #[test]
    fn daemon_failure_shows_the_stderr_tail_and_the_command_to_rerun() {
        let screen = failure_screen(&Failure::DaemonFailedToStart {
            cli: PathBuf::from("/Applications/My Tools/shelbi"),
            failure: DaemonFailure {
                reason: "`shelbi daemon` exited during startup (exit status: 2)".into(),
                stderr_tail: vec!["error: bad key".into()],
                stderr_log: Some(PathBuf::from("/h/logs/desktop-daemon-start.log")),
            },
        });
        assert_eq!(screen.details.as_ref().unwrap().1, ["error: bad key"]);
        assert!(screen
            .details
            .as_ref()
            .unwrap()
            .0
            .contains("desktop-daemon-start.log"));
        assert_eq!(
            commands(&screen)[0],
            "'/Applications/My Tools/shelbi' daemon"
        );
    }

    #[test]
    fn the_unreachable_socket_screen_names_the_socket_and_the_restart() {
        let screen = failure_screen(&Failure::ControlUnreachable {
            socket: PathBuf::from("/h/control.sock"),
            error: "connection refused".into(),
        });
        assert!(screen.body[1].contains("/h/control.sock"));
        assert_eq!(commands(&screen), [DAEMON_RESTART_COMMAND]);
    }

    #[test]
    fn app_newer_shows_both_versions_and_brew_upgrade() {
        let screen = failure_screen(&Failure::VersionMismatch(VersionCheck::AppNewer {
            app: "0.11.0".into(),
            cli: "0.10.0".into(),
        }));
        assert!(screen.body[0].contains("0.11.0") && screen.body[0].contains("0.10.0"));
        assert_eq!(commands(&screen), [UPGRADE_COMMAND]);
        assert_eq!(screen.action, ScreenAction::Retry);
    }

    #[test]
    fn an_outdated_app_offers_a_relaunch() {
        let screen = failure_screen(&Failure::VersionMismatch(VersionCheck::AppOutdated {
            app: "0.10.0".into(),
            daemon: "0.11.0".into(),
        }));
        assert_eq!(screen.action, ScreenAction::Relaunch);
    }

    #[test]
    fn a_stale_daemon_offers_a_restart() {
        let screen = failure_screen(&Failure::VersionMismatch(VersionCheck::DaemonStale {
            app: "0.11.0".into(),
            daemon: "0.10.0".into(),
        }));
        assert_eq!(commands(&screen), [DAEMON_RESTART_COMMAND]);
    }

    /// The daemon answered with an older protocol, and the CLI a restart
    /// would run speaks it too: upgrade the CLI, never "unreachable".
    #[test]
    fn an_older_protocol_under_an_older_cli_names_the_cli_and_both_protocols() {
        let screen = failure_screen(&Failure::ProtocolMismatch {
            cli: PathBuf::from("/Users/me/bin/shelbi"),
            check: ProtocolCheck::CliOlder {
                app: 6,
                cli: Some(4),
                daemon: 4,
            },
        });
        assert_eq!(screen.title, "The shelbi CLI is older than this app");
        assert_eq!(
            screen.body,
            ["/Users/me/bin/shelbi speaks control protocol 4; this app needs 6."]
        );
        assert!(screen.fix[0].text.contains(UPGRADE_COMMAND), "{:?}", screen.fix);
        assert_eq!(commands(&screen), [DAEMON_RESTART_COMMAND]);
        assert_eq!(screen.action, ScreenAction::Retry);
        let all = format!("{} {:?}", screen.title, screen.body);
        assert!(!all.contains("reach") && !all.contains("didn't answer"), "{all}");
    }

    #[test]
    fn a_cli_too_old_to_report_its_protocol_falls_back_to_the_daemons() {
        let screen = failure_screen(&Failure::ProtocolMismatch {
            cli: PathBuf::from("/Users/me/bin/shelbi"),
            check: ProtocolCheck::CliOlder {
                app: 6,
                cli: None,
                daemon: 4,
            },
        });
        assert_eq!(screen.title, "The shelbi CLI is older than this app");
        let body = &screen.body[0];
        assert!(body.contains("protocol 4") && body.contains("needs 6"), "{body}");
        assert!(body.contains("/Users/me/bin/shelbi"), "{body}");
        assert_eq!(commands(&screen), [DAEMON_RESTART_COMMAND]);
    }

    /// The CLI was upgraded but the daemon wasn't restarted: the same restart
    /// offer the version-string check gives.
    #[test]
    fn an_older_protocol_under_a_current_cli_offers_a_daemon_restart() {
        let screen = failure_screen(&Failure::ProtocolMismatch {
            cli: PathBuf::from("/opt/homebrew/bin/shelbi"),
            check: ProtocolCheck::DaemonStale {
                app: 6,
                cli: 6,
                daemon: 4,
            },
        });
        let stale = failure_screen(&Failure::VersionMismatch(VersionCheck::DaemonStale {
            app: "0.11.0".into(),
            daemon: "0.10.0".into(),
        }));
        assert_eq!(screen.title, stale.title);
        assert_eq!(screen.fix, stale.fix);
        assert_eq!(commands(&screen), [DAEMON_RESTART_COMMAND]);
        assert!(!screen.fix[0].text.contains(UPGRADE_COMMAND));
        assert_eq!(screen.action, ScreenAction::Retry);
    }

    #[test]
    fn a_newer_protocol_offers_a_relaunch() {
        let screen = failure_screen(&Failure::ProtocolMismatch {
            cli: PathBuf::from("/opt/homebrew/bin/shelbi"),
            check: ProtocolCheck::AppOutdated { app: 6, daemon: 7 },
        });
        assert_eq!(screen.action, ScreenAction::Relaunch);
    }

    #[test]
    fn the_relaunch_message_names_the_new_version_when_known() {
        let notice = RelaunchNotice {
            app_version: "0.10.0".into(),
            daemon_version: Some("0.11.0".into()),
            reason: "x".into(),
        };
        assert!(relaunch_message(&notice).starts_with("Shelbi was updated to 0.11.0."));
        let reload = RelaunchNotice {
            daemon_version: Some("0.10.0".into()),
            ..notice
        };
        assert!(relaunch_message(&reload).starts_with("Shelbi was updated."));
    }
}
