//! `shelbi-desktop --visual-views <dir>` (feature `visual-check`): render the
//! Activity feed and the Machines view offscreen against a sandboxed Shelbi
//! home, so the views can be eyeballed (and their data path exercised end to
//! end) without a screen, a daemon, or the user's real state.
//!
//! It points `SHELBI_HOME` at a fresh short `/tmp` directory (unsetting the
//! hub and control socket variables so nothing reaches a live hub), writes a
//! fixture project (a local machine with a dev slot and a review slot, and an
//! SSH machine whose host can't resolve), a few tasks, a workspace status and
//! an `events.log`, then opens each view through the real readers and saves
//! frames: `activity.png`, `activity-zen.png` (the Zen kind filter),
//! `activity-tasks.png`, `activity-workspaces.png`, `activity-system.png`
//! (each other kind alone; the Tasks feed has a title too long for a line),
//! `activity-picker.png` and `activity-workspace-picker.png` (each picker
//! open), `machines.png` once the unreachable remote's probe has landed,
//! and the issues board: `board.png`
//! (a card selected), `board-dropdown.png` and `board-workspace-dropdown.png`
//! (each filter open), `board-empty.png` (columns a filter emptied),
//! `board-drag.png` (a card dragged over another column, the drop target
//! lit) and `board-detail.png` (a card's detail); then `settings.png`, Settings
//! with its font picker open (`settings-font-picker.png`), the other choices
//! selected with the size stepper at each end (`settings-size-min.png`,
//! `settings-size-max.png`, `settings-option-right.png`), the bootstrap
//! screens (`bootstrap-progress.png`, `bootstrap-failure.png`) and the
//! notice bars (`notice-bars.png`: the update and relaunch bars). It prints
//! one line per check and exits non-zero if any failed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{px, size, AnyWindowHandle, AppContext, Entity, VisualTestAppContext};
use shelbi_core::{Column, Issue};

use crate::views::activity::ActivityView;
use crate::views::issues::IssuesView;
use crate::views::machines::MachinesView;
use crate::theme::style::{Type, Typography};

const PROJECT: &str = "demo";
/// A task title too long for one line of the feed (how a row wraps it).
const LONG_TITLE: &str = "Keep the terminal's scroll position anchored while a long-running agent streams output, and follow new output only when the view is already scrolled to the bottom";

pub fn run(out: PathBuf) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let home = PathBuf::from(format!("/tmp/shbvv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the sandbox home");
    // SAFETY: single-threaded here, before gpui or any reader thread starts.
    unsafe {
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_CONTROL_SOCK");
        std::env::remove_var("SHELBI_HUB_ADDR");
    }
    write_fixtures(&home);

    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::assets::load_fonts(cx);
    });
    let mut failures = 0;
    let mut expect = |name: &str, ok: bool| {
        println!("{}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // ---- Activity ----
    let (activity, window) = open(&mut cx, |cx| cx.new(|cx| ActivityView::new(PROJECT.into(), cx)));
    let loaded = wait(&mut cx, window, Duration::from_secs(10), |cx| {
        activity.update(cx, |v, cx| v.poll(cx));
        activity.read_with(cx, |v, _| v.state().feed().rows().count() >= 6)
    });
    expect("activity: the fixture events load into rows", loaded);
    let verbs: Vec<String> =
        activity.read_with(&cx, |v, _| v.state().feed().rows().map(|r| r.verb.clone()).collect());
    println!("      rows: {verbs:?}");
    expect(
        "activity: newest first, ssh failures folded, heartbeat hidden",
        verbs.first().map(String::as_str) == Some("merged")
            && verbs.iter().filter(|v| *v == "unreachable").count() == 1,
    );
    save(&mut cx, window, &out, "activity.png");
    activity.update(&mut cx, |v, cx| {
        v.state_mut()
            .toggle_kind(shelbi_app::activity::ActivityKind::Zen, Utc::now());
        cx.notify();
    });
    let zen: usize = activity.read_with(&cx, |v, _| v.state().feed().rows().count());
    expect("activity: the Zen filter keeps the zen rows only", zen == 2);
    // Let the selected chip's slide finish.
    wait(&mut cx, window, Duration::from_millis(300), |_| false);
    save(&mut cx, window, &out, "activity-zen.png");
    // Every other kind picked alone, for its selected segment.
    for (kind, name) in [
        (shelbi_app::activity::ActivityKind::Tasks, "activity-tasks.png"),
        (shelbi_app::activity::ActivityKind::Workspaces, "activity-workspaces.png"),
        (shelbi_app::activity::ActivityKind::System, "activity-system.png"),
    ] {
        activity.update(&mut cx, |v, cx| {
            v.state_mut().clear_kinds(Utc::now());
            v.state_mut().toggle_kind(kind, Utc::now());
            cx.notify();
        });
        wait(&mut cx, window, Duration::from_millis(300), |_| false);
        if kind == shelbi_app::activity::ActivityKind::Tasks {
            let long = activity.read_with(&cx, |v, _| {
                v.state().feed().rows().any(|r| r.title == LONG_TITLE)
            });
            expect("activity: the long title's row is in the Tasks feed", long);
        }
        save(&mut cx, window, &out, name);
    }
    for (picker, name) in [
        (crate::views::activity_state::Picker::Project, "activity-picker.png"),
        (crate::views::activity_state::Picker::Workspace, "activity-workspace-picker.png"),
    ] {
        activity.update(&mut cx, |v, cx| {
            v.state_mut().clear_kinds(Utc::now());
            v.state_mut().picker = None;
            v.state_mut().toggle_picker(picker);
            cx.notify();
        });
        // Let the menu's slide-and-fade finish.
        wait(&mut cx, window, Duration::from_millis(300), |_| false);
        save(&mut cx, window, &out, name);
    }

    // ---- Machines ----
    let (machines, window) = open(&mut cx, |cx| cx.new(|cx| MachinesView::new(PROJECT.into(), cx)));
    let loaded = wait(&mut cx, window, Duration::from_secs(10), |cx| {
        machines.update(cx, |v, cx| v.poll(cx));
        machines.read_with(cx, |v, _| v.state().model().workspace_count() == 3)
    });
    expect("machines: every workspace under its machine", loaded);
    if let Some(e) = machines.read_with(&cx, |v, _| v.state().error().map(str::to_string)) {
        println!("      read error: {e}");
    }
    let probed = wait(&mut cx, window, Duration::from_secs(30), |cx| {
        machines.update(cx, |v, cx| v.poll(cx));
        machines.read_with(cx, |v, _| {
            matches!(
                v.state().model().machines.get(1).map(|m| &m.reachability),
                Some(shelbi_core::Reachability::Unreachable { .. })
            )
        })
    });
    expect("machines: the unresolvable remote reads unreachable", probed);
    let cell = machines.read_with(&cx, |v, _| {
        v.state().model().machines[0].workspaces[0].assignment.cell()
    });
    expect("machines: the workspace-list state cell", cell == "in_progress: t1");
    save(&mut cx, window, &out, "machines.png");

    // ---- Issues board ----
    let (board, window) = open(&mut cx, |cx| cx.new(|cx| IssuesView::new(PROJECT.into(), cx)));
    let loaded = wait(&mut cx, window, Duration::from_secs(10), |cx| {
        board.update(cx, |v, cx| v.poll(cx));
        board.update(cx, |v, _| {
            v.state_mut().model().columns.iter().map(|c| c.cards.len()).sum::<usize>() >= 5
        })
    });
    expect("board: the fixture tasks load into columns", loaded);
    board.update(&mut cx, |v, cx| {
        v.state_mut().select(1, 0);
        cx.notify();
    });
    save(&mut cx, window, &out, "board.png");
    board.update(&mut cx, |v, cx| {
        v.state_mut().open_workflow_dropdown();
        cx.notify();
    });
    wait(&mut cx, window, Duration::from_millis(300), |_| false);
    save(&mut cx, window, &out, "board-dropdown.png");
    board.update(&mut cx, |v, cx| {
        v.state_mut().close_overlay();
        v.state_mut().open_workspace_dropdown();
        cx.notify();
    });
    wait(&mut cx, window, Duration::from_millis(300), |_| false);
    save(&mut cx, window, &out, "board-workspace-dropdown.png");
    // States the Figma frame doesn't draw: a column emptied by a filter, and
    // a card dragged over another column (the drop target lit).
    board.update(&mut cx, |v, cx| {
        v.state_mut().close_overlay();
        let _ = v
            .state_mut()
            .set_workspace_filter(Some(shelbi_app::board::WorkspaceFilter::Workspace("alpha".into())));
        cx.notify();
    });
    save(&mut cx, window, &out, "board-empty.png");
    board.update(&mut cx, |v, cx| {
        let _ = v.state_mut().set_workspace_filter(None);
        cx.notify();
    });
    draw(&mut cx, window);
    let (none, left) = (gpui::Modifiers::default(), gpui::MouseButton::Left);
    // The first backlog card, then over the In progress column.
    cx.simulate_mouse_down(window, gpui::point(px(148.0), px(140.0)), left, none);
    cx.simulate_mouse_move(window, gpui::point(px(160.0), px(150.0)), left, none);
    cx.simulate_mouse_move(window, gpui::point(px(716.0), px(300.0)), left, none);
    save(&mut cx, window, &out, "board-drag.png");
    // Let go over the toolbar, no drop target, so nothing moves.
    cx.simulate_mouse_move(window, gpui::point(px(500.0), px(20.0)), left, none);
    cx.simulate_mouse_up(window, gpui::point(px(500.0), px(20.0)), left, none);
    draw(&mut cx, window);
    board.update(&mut cx, |v, cx| {
        v.state_mut().open_detail("t1");
        cx.notify();
    });
    save(&mut cx, window, &out, "board-detail.png");

    // ---- Settings, the bootstrap screens and the notice bars ----
    let window = open_with_window(&mut cx, |window, cx| {
        cx.new(|cx| crate::views::settings::SettingsView::new(PROJECT, window, cx))
    });
    save(&mut cx, window, &out, "settings.png");
    // The font picker open, once the installed families are read.
    let settings = window
        .update(&mut cx, |root, _, _| root.downcast::<crate::views::settings::SettingsView>())
        .expect("the settings window")
        .expect("the settings view");
    window
        .update(&mut cx, |_, window, cx| {
            settings.update(cx, |v, cx| v.offscreen_open_font_picker(window, cx))
        })
        .expect("open the font picker");
    let listed = wait(&mut cx, window, Duration::from_secs(10), |cx| {
        settings.read_with(cx, |v, cx| v.offscreen_font_picker_ready(cx))
    });
    expect("settings: the font picker lists the installed fonts", listed);
    save(&mut cx, window, &out, "settings-font-picker.png");
    // The states the Figma frame doesn't draw: every other choice selected,
    // and the size stepper at each end of its range (that end's button dim).
    {
        use shelbi_state::{OptionAsMeta, ThemePreference};
        let states = [
            ("settings-size-min.png", ThemePreference::Dark, OptionAsMeta::Both, 8),
            ("settings-size-max.png", ThemePreference::Light, OptionAsMeta::Left, 32),
            ("settings-option-right.png", ThemePreference::Auto, OptionAsMeta::Right, 14),
        ];
        for (name, theme, option, size) in states {
            let window = open_with_window(&mut cx, |window, cx| {
                cx.new(|cx| crate::views::settings::SettingsView::new(PROJECT, window, cx))
            });
            let settings = window
                .update(&mut cx, |root, _, _| root.downcast::<crate::views::settings::SettingsView>())
                .expect("the settings window")
                .expect("the settings view");
            // Once config.yaml is read (it would put the saved values back).
            wait(&mut cx, window, Duration::from_secs(5), |cx| {
                settings.read_with(cx, |v, _| v.offscreen_loaded())
            });
            window
                .update(&mut cx, |_, window, cx| {
                    settings.update(cx, |v, cx| v.offscreen_show(theme, option, size, window, cx))
                })
                .expect("show the state");
            // The selection chips slide to their choices.
            wait(&mut cx, window, Duration::from_millis(400), |_| false);
            save(&mut cx, window, &out, name);
        }
    }
    let (_, window) = open(&mut cx, |cx| {
        cx.new(|_| Screen(Box::new(|theme, _| {
            crate::views::bootstrap::render_progress(
                &crate::platform::bootstrap::BootstrapState::FindingCli,
                theme,
            )
        })))
    });
    save(&mut cx, window, &out, "bootstrap-progress.png");
    let (_, window) = open(&mut cx, |cx| {
        cx.new(|_| Screen(Box::new(|theme, _| {
            use crate::platform::bootstrap::Failure;
            use crate::platform::discovery::CliNotFound;
            let failure = Failure::CliNotFound(CliNotFound {
                searched: vec![
                    PathBuf::from("/opt/homebrew/bin/shelbi"),
                    PathBuf::from("/usr/local/bin/shelbi"),
                    PathBuf::from("~/.cargo/bin/shelbi"),
                ],
                setting: None,
                config_error: None,
            });
            crate::views::bootstrap::render_failure(
                &crate::views::bootstrap::failure_screen(&failure),
                theme,
                |_, _, _| {},
            )
        })))
    });
    save(&mut cx, window, &out, "bootstrap-failure.png");
    let (_, window) = open(&mut cx, |cx| {
        cx.new(|_| Screen(Box::new(|theme, _| {
            use gpui::{div, IntoElement, ParentElement, Styled};
            let notice = crate::platform::update::UpdateNotice {
                current: "0.9.0".into(),
                latest: "0.9.2".into(),
            };
            let relaunch = crate::platform::control::RelaunchNotice {
                app_version: "0.9.0".into(),
                daemon_version: Some("0.9.2".into()),
                reason: "The daemon was upgraded.".into(),
            };
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(crate::views::update::render_update_bar(&notice, theme, |_, _| {}))
                .child(crate::views::bootstrap::render_relaunch_bar(&relaunch, theme, |_, _| {}))
                .into_any_element()
        })))
    });
    save(&mut cx, window, &out, "notice-bars.png");

    let _ = std::fs::remove_dir_all(&home);
    println!("\n{failures} check(s) failed; frames saved under {}", out.display());
    std::process::exit(if failures == 0 { 0 } else { 1 })
}

fn open<V: gpui::Render + 'static>(
    cx: &mut VisualTestAppContext,
    build: impl FnOnce(&mut gpui::App) -> Entity<V>,
) -> (Entity<V>, AnyWindowHandle) {
    let mut entity = None;
    let handle = cx
        .open_offscreen_window(size(px(1100.0), px(640.0)), |_, cx| {
            let view = build(cx);
            entity = Some(view.clone());
            view
        })
        .expect("open the offscreen window");
    (entity.expect("the view"), handle.into())
}

/// [`open`] for a view that needs the window to build.
fn open_with_window<V: gpui::Render + 'static>(
    cx: &mut VisualTestAppContext,
    build: impl FnOnce(&mut gpui::Window, &mut gpui::App) -> Entity<V>,
) -> AnyWindowHandle {
    cx.open_offscreen_window(size(px(1100.0), px(640.0)), build)
        .expect("open the offscreen window")
        .into()
}

/// A frame of free-standing pieces (a bootstrap screen, the bars) on the
/// main surface.
struct Screen(Box<PaintScreen>);

type PaintScreen = dyn Fn(&crate::theme::Theme, &mut gpui::Window) -> gpui::AnyElement;

impl gpui::Render for Screen {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, ParentElement, Styled};
        let theme = *cx.global::<crate::theme::Theme>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .text_color(theme.foreground)
            .font_family("Geist")
            .type_style(Type::Base)
            .child((self.0)(&theme, window))
    }
}

/// Pump frames until `done` or the deadline.
fn wait(
    cx: &mut VisualTestAppContext,
    window: AnyWindowHandle,
    deadline: Duration,
    mut done: impl FnMut(&mut VisualTestAppContext) -> bool,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        draw(cx, window);
        if done(cx) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn draw(cx: &mut VisualTestAppContext, window: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
        .expect("draw");
}

fn save(cx: &mut VisualTestAppContext, window: AnyWindowHandle, out: &std::path::Path, name: &str) {
    draw(cx, window);
    let image = cx.capture_screenshot(window).expect("render to image");
    let path = out.join(name);
    image.save(&path).expect("save the frame");
    println!("saved {}", path.display());
}

fn write_fixtures(home: &std::path::Path) {
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: {PROJECT}
display_name: Demo Board
repo: ''
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: {work}
  - name: gpu
    kind: ssh
    host: gpu.invalid
    work_dir: /home/me
    tags: [gpu, linux]
workspaces:
  - name: alpha
    machine: local
  - name: review
    machine: local
    tags: [review]
    slot: 1
  - name: gpu-1
    machine: gpu
",
            work = home.join("work").display()
        ),
    )
    .unwrap();
    let now = Utc::now();
    let task = |id: &str, title: &str, column: Column, assigned: Option<&str>| {
        let mut t = Issue {
            id: id.into(),
            title: title.into(),
            column,
            priority: 0,
            assigned_to: assigned.map(str::to_string),
            workflow: None,
            branch: Some(format!("jlong/{id}")),
            depends_on: vec![],
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        };
        if id == "t1" {
            t.params.insert("agent".into(), "developer".into());
        }
        shelbi_state::save_task(PROJECT, &t, "").expect("save a fixture task");
    };
    task("t1", "Fix the login redirect loop", Column::in_progress(), Some("alpha"));
    task("r9", "Add the activity feed to the desktop app", Column::review(), Some("review"));
    task("d1", "Ship the board", Column::done(), None);
    task("b1", "Write the theme docs for the desktop palette", Column::backlog(), None);
    task("b2", "Sync the board's column widths with the TUI", Column::backlog(), None);
    task("c1", "Restyle the command palette", Column::todo(), None);
    task("t2", LONG_TITLE, Column::in_progress(), None);
    shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
        workspace: "alpha".into(),
        current_task: Some("t1".into()),
        state: shelbi_state::WorkspaceState::AwaitingInput,
        last_transition: now,
        last_seen: now,
    })
    .unwrap();
    let at = |mins: i64| (now - chrono::Duration::minutes(mins)).to_rfc3339();
    let lines = [
        format!("{} project=demo task=t1 workflow=default todo -> in_progress reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer", at(1500)),
        format!("{} project=demo task=r9 workflow=default todo -> in_progress reason=user:cli", at(95)),
        format!("{} project=demo task=r9 workflow=default in_progress -> review reason=workspace:ready-marker from_category=active to_category=handoff", at(52)),
        format!("{} project=demo workspace=alpha working -> awaiting_input", at(40)),
        format!("{} project=demo task=t2 workflow=default todo -> in_progress reason=user:cli", at(35)),
        format!("{} ssh reverse-forward host=gpu.invalid mode=tcp status=failed detail=master_open_failed", at(30)),
        format!("{} ssh reverse-forward host=gpu.invalid mode=tcp status=failed detail=master_open_failed", at(25)),
        format!("{} project=demo task=d1 workflow=default review -> done reason=zen:failed-checks cmd=\"cargo test\" exit=101", at(20)),
        format!("{} project=demo heartbeat zen_eligible=1 idle_workspaces=2", at(10)),
        format!("{} project=web task=w9 workflow=default backlog -> todo reason=user:board", at(6)),
        format!("{} project=demo task=d1 workflow=default review -> done reason=orchestrator:zen-merge_pr=1694", at(2)),
    ];
    std::fs::write(home.join("events.log"), lines.join("\n") + "\n").unwrap();
}
