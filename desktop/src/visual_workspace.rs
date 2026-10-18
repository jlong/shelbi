//! `shelbi-desktop --visual-workspace <dir>` (feature `visual-check`): the
//! Workspace view rendered offscreen (real Metal frames, no screen needed, as
//! `--visual-check` does) over fake sessions and a fake daemon, one frame per
//! state, plus a check that each state shows what it should:
//!
//! - `workspace-working.png`: a working workspace, its panel and agent;
//! - `workspace-shell.png`: the user shell open beside the agent;
//! - `workspace-description.png`: the task description (More);
//! - `workspace-idle.png`: an idle workspace's reduced panel and placeholder;
//! - `workspace-remote-reconnecting.png`: a remote workspace whose SSH
//!   connection dropped, reconnecting.
//!
//! The view renders as its panel; a host lays it out in a sidebar-width
//! column beside [`WorkspaceView::render_main`], as the window does.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{
    div, prelude::*, px, size, AnyWindowHandle, AppContext, Context, Entity, VisualTestAppContext,
    Window, WindowHandle,
};
use shelbi_app::session::{
    self as app_session, ConnectFailure, IdleInfo, ReconnectPolicy, Relauncher, RetryPolicy,
    SessionRef,
};
use shelbi_app::view::{WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceRow};
use shelbi_app::workspace_panel::{WsRow, WsSwitch};
use shelbi_app::WorkspaceSessionOp;
use shelbi_client::SessionEvent;
use shelbi_term::Size;

use crate::elements::terminal::session::{Connected, SessionIo};
use crate::theme::Theme;
use crate::views::session_view::{SessionConnector, SessionDeps};
use crate::views::workspace::{WorkspaceDeps, WorkspaceOps, WorkspaceView};
use crate::theme::style::{Type, Typography};

struct NullIo;
impl SessionIo for NullIo {
    fn input(&self, _bytes: Vec<u8>) {}
    fn paste(&self, _text: String, _bracketed: bool) {}
    fn resize(&self, _size: Size) {}
}

/// Sessions by name: `idle` / `unreachable` names answer that way, the rest
/// attach and get `screens[name]` as their replay.
#[derive(Default)]
struct Sessions {
    idle: Mutex<Vec<String>>,
    unreachable: Mutex<Vec<String>>,
    screens: Mutex<Vec<(String, Vec<u8>)>>,
    streams: Mutex<Vec<(String, UnboundedSender<SessionEvent>)>>,
}

impl SessionConnector for Sessions {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        _retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure> {
        let name = app_session::discovery_name(project, target);
        if self.unreachable.lock().unwrap().contains(&name) {
            return Err(ConnectFailure::Unreachable("`gpu-box` is unreachable".into()));
        }
        if self.idle.lock().unwrap().contains(&name) {
            return Err(ConnectFailure::Idle(IdleInfo {
                name: target.workspace().unwrap_or_default().into(),
                machine: "hub".into(),
                branch: Some("main".into()),
            }));
        }
        let (tx, rx) = unbounded();
        let screen = self
            .screens
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        tx.unbounded_send(SessionEvent::Resync {
            seq: 0,
            replay: screen,
        })
        .unwrap();
        self.streams.lock().unwrap().push((name.clone(), tx));
        Ok(Connected {
            link: Box::new(NullIo),
            events: rx,
            size: Size::new(120, 40),
            name,
            cwd: None,
        })
    }
}

impl Relauncher for Sessions {
    fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
        Ok(())
    }
}

struct Ops;

impl WorkspaceOps for Ops {
    fn details(&self, _project: &str, workspace: &str) -> Result<WorkspaceDetailsInfo, String> {
        Ok(if workspace == "bravo" {
            WorkspaceDetailsInfo {
                branch: Some("main".into()),
                worktree: "/Users/me/src/demo/.shelbi/wt/bravo".into(),
                editor_name: "Vim".into(),
                ..WorkspaceDetailsInfo::default()
            }
        } else {
            WorkspaceDetailsInfo {
                task: Some(("cold-start".into(), "Warm the application cache at startup".into())),
                body: "## Summary\n\nWarm the application cache during startup so the first \
                       request is fast.\n\n## Acceptance criteria\n\n- [x] Cache warms on boot\n\
                       - [ ] Startup stays under 2s\n"
                    .into(),
                branch: Some("jlong/cold-start".into()),
                worktree: "/Users/me/src/demo/.shelbi/wt/alpha".into(),
                editor_name: "Vim".into(),
            }
        })
    }

    fn session_op(&self, _project: &str, _workspace: &str, _op: WorkspaceSessionOp) -> Result<(), String> {
        Ok(())
    }

    fn reveal(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }
}

const AGENT_SCREEN: &str = "\x1b[1m╭──────────────────────────────────────────────╮\x1b[0m\r\n\
\x1b[1m│\x1b[0m \x1b[38;5;208m✻\x1b[0m Welcome to Claude Code                     \x1b[1m│\x1b[0m\r\n\
\x1b[1m╰──────────────────────────────────────────────╯\x1b[0m\r\n\r\n\
> Warm the application cache at startup\r\n\r\n\
\x1b[38;5;208m⏺\x1b[0m Reading src/cache.rs…\r\n\
\x1b[38;5;208m⏺\x1b[0m Update(src/boot.rs)\r\n\
  \x1b[32m+ cache::warm(&config)?;\x1b[0m\r\n\r\n\
\x1b[2m✳ Thinking… (esc to interrupt)\x1b[0m\r\n";

const SHELL_SCREEN: &str = "\x1b[32mme@hub\x1b[0m \x1b[34m~/src/demo/.shelbi/wt/alpha\x1b[0m $ git status -sb\r\n\
## jlong/cold-start\r\n M src/boot.rs\r\n\
\x1b[32mme@hub\x1b[0m \x1b[34m~/src/demo/.shelbi/wt/alpha\x1b[0m $ ";

/// The window's layout in small: the panel in a sidebar-width column, the
/// main area beside it.
struct Host {
    view: Entity<WorkspaceView>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let main = self.view.update(cx, |v, cx| v.render_main(cx));
        div()
            .flex()
            .flex_row()
            .size_full()
            .font_family("Geist")
            .type_style(Type::Base)
            .bg(theme.canvas)
            .child(div().flex_none().w(px(300.0)).h_full().child(self.view.clone()))
            .child(div().w(px(1.0)).h_full().bg(theme.divider))
            .child(div().flex().flex_1().min_w_0().h_full().child(main))
    }
}

struct Run {
    cx: VisualTestAppContext,
    handle: WindowHandle<Host>,
    window: AnyWindowHandle,
    sessions: Arc<Sessions>,
    out: PathBuf,
    failures: usize,
}

/// Run it, saving frames under `out`; exits non-zero if a check failed.
pub fn run(out: PathBuf) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::assets::load_fonts(cx);
    });
    let sessions = Arc::new(Sessions::default());
    sessions.screens.lock().unwrap().extend([
        ("demo/ws/alpha".to_string(), AGENT_SCREEN.as_bytes().to_vec()),
        ("demo/ws/alpha/shell".to_string(), SHELL_SCREEN.as_bytes().to_vec()),
        ("demo/ws/gpu/shell".to_string(), SHELL_SCREEN.as_bytes().to_vec()),
        ("demo/ws/gpu".to_string(), AGENT_SCREEN.as_bytes().to_vec()),
    ]);
    sessions.idle.lock().unwrap().push("demo/ws/bravo".into());
    let deps_sessions = sessions.clone();
    let handle = cx
        .open_offscreen_window(size(px(1200.0), px(700.0)), move |window, cx| {
            let handle = window.window_handle();
            let deps = WorkspaceDeps {
                ops: Arc::new(Ops),
                sessions: Rc::new(move || SessionDeps {
                    connector: deps_sessions.clone(),
                    relauncher: deps_sessions.clone(),
                    retry: RetryPolicy::default(),
                    reconnect: ReconnectPolicy::default(),
                    clock: Rc::new(Instant::now),
                }),
            };
            let view = cx.new(|cx| {
                let mut v = WorkspaceView::new("demo", handle, deps, None, cx);
                v.set_palette_chord("⌃P", cx);
                v
            });
            cx.new(|_| Host { view })
        })
        .expect("open the offscreen window");
    let window: AnyWindowHandle = handle.into();
    let mut run = Run {
        cx,
        handle,
        window,
        sessions,
        out,
        failures: 0,
    };
    run.all();
    println!("\n{} check(s) failed; frames saved under {}", run.failures, run.out.display());
    std::process::exit(if run.failures == 0 { 0 } else { 1 })
}

fn row(name: &str, machine: &str, remote: bool, task: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
    WorkspaceRow {
        name: name.into(),
        machine: machine.into(),
        is_remote: remote,
        current_task: task.map(str::to_string),
        agent: task.map(|_| "developer".into()),
        badge,
    }
}

impl Run {
    fn update<R>(&mut self, f: impl FnOnce(&mut WorkspaceView, &mut gpui::Window, &mut gpui::Context<WorkspaceView>) -> R) -> R {
        let view = self.view();
        let r = self
            .handle
            .update(&mut self.cx, |_, window, cx| view.update(cx, |v, cx| f(v, window, cx)))
            .unwrap();
        self.settle();
        r
    }

    /// Let background connects land and repaint.
    fn settle(&mut self) {
        for _ in 0..20 {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.cx
            .update_window(self.window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        self.cx.run_until_parked();
    }

    fn view(&mut self) -> Entity<WorkspaceView> {
        self.handle.update(&mut self.cx, |host, _, _| host.view.clone()).unwrap()
    }

    fn rows(&mut self) -> Vec<WsRow> {
        self.view().read_with(&self.cx, |v, _| v.panel().rows().to_vec())
    }

    fn expect(&mut self, name: &str, ok: bool) {
        if ok {
            println!("ok    {name}");
        } else {
            self.failures += 1;
            println!("FAIL  {name}");
        }
    }

    fn save(&mut self, name: &str) {
        self.settle();
        let image = self.cx.capture_screenshot(self.window).expect("render to image");
        let path = self.out.join(name);
        image.save(&path).expect("save the frame");
        println!("saved {}", path.display());
    }

    fn all(&mut self) {
        // A working workspace.
        self.update(|v, _, cx| {
            v.show("alpha", Some(row("alpha", "hub", false, Some("cold-start"), WorkspaceBadge::Working)), cx)
        });
        self.settle();
        let header = self.view().read_with(&self.cx, |v, _| v.header());
        self.expect(
            "the panel shows the task, branch and machine",
            header.as_ref().and_then(|h| h.task_line()).as_deref()
                == Some("cold-start  Warm the application cache at startup")
                && header.as_ref().map(|h| h.location_line()).as_deref() == Some("hub · jlong/cold-start"),
        );
        let rows = self.rows();
        self.expect(
            "the panel offers More, the folder, the view switch and the shell",
            rows == [
                WsRow::Back,
                WsRow::More,
                WsRow::Folder,
                WsRow::Switch(WsSwitch::Agent),
                WsRow::Switch(WsSwitch::Diff),
                WsRow::Switch(WsSwitch::Editor),
                WsRow::Shell,
            ],
        );

        self.save("workspace-working.png");

        // The user shell beside it.
        self.update(|v, window, cx| v.toggle_shell(window, cx));
        self.settle();
        let shell = self.view().read_with(&self.cx, |v, _| v.shell_open());
        self.expect("the shell opens beside the agent", shell);
        self.save("workspace-shell.png");

        // The description.
        self.update(|v, _, cx| v.toggle_description_for_check(cx));
        let description = self.view().read_with(&self.cx, |v, _| v.description_open());
        self.expect("More opens the task description", description);
        self.save("workspace-description.png");
        self.update(|v, _, cx| v.toggle_description_for_check(cx));

        // An idle workspace.
        self.update(|v, _, cx| v.show("bravo", Some(row("bravo", "hub", false, None, WorkspaceBadge::Idle)), cx));
        self.settle();
        let closed = !self.view().read_with(&self.cx, |v, _| v.shell_open());
        self.expect("switching workspaces closes the shell", closed);
        let rows = self.rows();
        self.expect("an idle workspace's panel is reduced", rows == [WsRow::Back, WsRow::Folder]);
        self.save("workspace-idle.png");

        // A remote workspace whose SSH connection drops.
        self.update(|v, _, cx| {
            v.show("gpu", Some(row("gpu", "gpu-box", true, Some("cold-start"), WorkspaceBadge::AwaitingInput)), cx)
        });
        self.settle();
        self.sessions.unreachable.lock().unwrap().push("demo/ws/gpu".into());
        self.sessions.streams.lock().unwrap().retain(|(n, _)| n != "demo/ws/gpu");
        self.settle();
        let notice = self.view().read_with(&self.cx, |v, cx| v.content_notice(cx));
        println!("      notice: {notice:?}");
        self.expect(
            "a dropped remote connection shows the reconnect notice",
            notice.is_some_and(|n| n.starts_with("Reconnecting to gpu-box")),
        );
        let rows = self.rows();
        self.expect(
            "a remote workspace's panel has no folder, diff or editor",
            rows == [WsRow::Back, WsRow::More, WsRow::Switch(WsSwitch::Agent), WsRow::Shell],
        );
        self.save("workspace-remote-reconnecting.png");
    }
}
