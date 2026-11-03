//! The Orchestrator view's onboarding in the window (`orch-onboarding`): a
//! project whose orchestrator was never set up shows it in place of the
//! chat, starts nothing, and is walked through its three steps by the
//! mouse; Launch Orchestrator marks the project and hands back to the
//! chat. Reads and writes a throwaway `SHELBI_HOME` (the shared Agents
//! fixture, project `alpha`).

use gpui::TestAppContext;
use shelbi_app::settings::agents::fixture::Home;
use shelbi_core::RunnerKind;

use super::smoke::Harness;
use super::*;

fn home() -> Home {
    Home::new("dt-onb", "alpha", crate::test_env::lock())
}

/// The harness's window on the Orchestrator view of `alpha`, opened with
/// the real onboarding check.
fn open(cx: &mut TestAppContext) -> Harness {
    let mut h = Harness::open(cx);
    let window = h.window.clone();
    // Fake sessions: the chat (once it shows) attaches to no real session.
    let (deps, _sessions, _ops) = crate::views::workspace::tests::fake_deps();
    h.cx.update(|win, cx| {
        window.update(cx, |w, cx| {
            w.chat_deps = Some(deps.sessions.clone());
            w.workspace_deps = Some(deps);
            w.needs_onboarding = shelbi_app::onboarding::needs_onboarding;
            w.client.set_view(View::Session("orch".into()));
            w.client.focus_main();
            w.open_project(cx);
            w.sync_chat(win, cx);
        })
    });
    h.cx.run_until_parked();
    h.draw();
    h.draw();
    h
}

fn onboarding(h: &Harness) -> Option<Entity<OnboardingView>> {
    h.window.read_with(&h.cx, |w, _| w.onboarding.clone())
}

#[gpui::test]
fn a_project_never_set_up_shows_the_onboarding_and_starts_nothing(cx: &mut TestAppContext) {
    let _home = home();
    let mut h = open(cx);
    assert!(onboarding(&h).is_some(), "the onboarding stands in for the chat");
    let chat_shown = h.window.read_with(&h.cx, |w, cx| w.chat.as_ref().is_some_and(|c| c.read(cx).is_shown()));
    assert!(!chat_shown, "no chat attaches");
    let status = h.window.read_with(&h.cx, |w, _| w.status.clone());
    assert_eq!(status, None, "nothing is starting");
    for selector in ["onboarding-disc", "onboarding-title", "onboarding-intro", "onboarding-get-started"] {
        assert!(h.cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    // The Figma geometry: the 56pt disc, the 40pt Large button.
    let disc = h.cx.debug_bounds("onboarding-disc").unwrap();
    assert_eq!((f32::from(disc.size.width), f32::from(disc.size.height)), (56.0, 56.0));
    let start = h.cx.debug_bounds("onboarding-get-started").unwrap();
    assert_eq!(f32::from(start.size.height), 40.0);
}

#[gpui::test]
fn the_mouse_walks_the_steps_and_launch_hands_back_to_the_chat(cx: &mut TestAppContext) {
    let home = home();
    let mut h = open(cx);
    h.click_selector("onboarding-get-started");
    h.draw();
    let step = |h: &Harness| onboarding(h).map(|o| o.read_with(&h.cx, |o, _| o.state().step()));
    assert_eq!(step(&h), Some(OnboardingStep::Harness));
    let picker = h.cx.debug_bounds("onboarding-harness").unwrap();
    assert_eq!((f32::from(picker.size.width), f32::from(picker.size.height)), (620.0, 36.0));
    assert!(h.cx.debug_bounds("onboarding-harness-provider").is_some(), "Anthropic beside Claude Code");

    // Harness ▸ Codex, Default model ▸ GPT-5.
    h.click_selector("onboarding-harness");
    h.click_selector("onboarding-harness-codex");
    h.draw();
    h.click_selector("onboarding-model");
    h.click_selector("onboarding-model-1");
    h.draw();
    let o = onboarding(&h).unwrap();
    let (runner, model) = o.read_with(&h.cx, |o, _| (o.state().runner(), o.state().model_choice().map(str::to_string)));
    assert_eq!((runner, model.as_deref()), (RunnerKind::Codex, Some("gpt-5")));
    h.click_selector("onboarding-continue");
    h.draw();
    assert_eq!(step(&h), Some(OnboardingStep::Instructions));
    let editor = h.cx.debug_bounds("onboarding-instructions").unwrap();
    assert_eq!(f32::from(editor.size.height), 400.0);
    let project = shelbi_state::load_project("alpha").unwrap();
    assert_eq!(project.orchestrator.runner, "codex");

    // Back keeps the edit; Launch saves it and marks the project.
    let field = o.read_with(&h.cx, |o, _| o.instructions_field());
    let window = h.window.clone();
    h.cx.update(|win, cx| {
        let _ = window;
        field.update(cx, |s, cx| s.set_value("# You are the Orchestrator\n\nShip it.\n", win, cx));
    });
    h.cx.run_until_parked();
    h.click_selector("onboarding-back");
    h.draw();
    assert_eq!(step(&h), Some(OnboardingStep::Harness));
    h.click_selector("onboarding-continue");
    h.draw();
    assert_eq!(o.read_with(&h.cx, |o, _| o.state().instructions().to_string()), "# You are the Orchestrator\n\nShip it.\n");
    h.click_selector("onboarding-launch");
    h.draw();
    h.draw();
    let text = std::fs::read_to_string(home.agents().join("orchestrator/instructions.md")).unwrap();
    assert_eq!(text, "# You are the Orchestrator\n\nShip it.\n");
    assert!(!shelbi_app::onboarding::needs_onboarding("alpha"), "reopening skips it");
    assert!(onboarding(&h).is_none(), "the chat's place is the chat's again");
    // The usual open ran (the test window's CLI doesn't exist, so it says
    // it couldn't bring the orchestrator up).
    let status = h.window.read_with(&h.cx, |w, _| w.status.clone().map(|s| s.to_string()));
    assert!(status.is_some_and(|s| s.contains("couldn")), "the launch path ran");
}

#[gpui::test]
fn an_existing_project_goes_straight_to_the_chat(cx: &mut TestAppContext) {
    let _home = home();
    shelbi_state::mark_orchestrator_onboarded("alpha").unwrap();
    let h = open(cx);
    assert!(onboarding(&h).is_none());
}
