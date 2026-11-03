//! The TUI's onboarding page against real files in a throwaway
//! `SHELBI_HOME` (the shared Agents fixture): the three steps through the
//! keys alone, through the mouse alone, Back keeping edits, and what it
//! draws.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, Terminal};
use shelbi_app::settings::agents::fixture::Home;
use shelbi_core::RunnerKind;

use super::*;

const PROJECT: &str = "proj";

fn home(tag: &str) -> Home {
    let lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Home::new(&format!("tui-onb-{tag}"), PROJECT, lock)
}

/// Runs each job in place, so a `poll` right after sees it.
struct Inline;

impl Worker for Inline {
    fn run(&self, job: Box<dyn FnOnce() + Send>) {
        job();
    }
}

fn page() -> OnboardingPage {
    let mut page = OnboardingPage::with_worker(PROJECT, Box::new(Inline));
    page.poll();
    assert!(page.state().is_loaded());
    page
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(page: &mut OnboardingPage, code: KeyCode) -> PageAction {
    let action = page.handle_key(key(code));
    let (_, polled) = page.poll();
    if polled == PageAction::Launch {
        return polled;
    }
    action
}

fn draw(page: &mut OnboardingPage) -> String {
    let mut term = Terminal::new(TestBackend::new(100, 44)).unwrap();
    term.draw(|f| page.render(f, f.area())).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first hit `pred` picks, as a click at its rect's middle.
fn click_at(page: &OnboardingPage, pred: impl Fn(&Hit) -> bool) -> MouseEvent {
    let (r, _) = page.hits.iter().find(|(_, h)| pred(h)).expect("a hit");
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + r.width / 2,
        row: r.y + r.height / 2,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(page: &mut OnboardingPage, pred: impl Fn(&Hit) -> bool) -> PageAction {
    draw(page);
    let m = click_at(page, pred);
    let action = page.handle_mouse(m);
    let (_, polled) = page.poll();
    if polled == PageAction::Launch {
        return polled;
    }
    action
}

#[test]
fn the_keys_walk_all_three_steps() {
    let home = home("keys");
    let mut page = page();
    let screen = draw(&mut page);
    for copy in [INTRO_TITLE, GET_STARTED] {
        assert!(screen.contains(copy), "{copy}:\n{screen}");
    }
    assert_eq!(press(&mut page, KeyCode::Enter), PageAction::Stay);
    assert_eq!(page.step(), OnboardingStep::Harness);
    assert_eq!(page.focus(), Item::Harness);
    let screen = draw(&mut page);
    for copy in [HARNESS_TITLE, "Choose the provider and model", HARNESS_LABEL, "Claude Code  Anthropic", MODEL_LABEL, FIELD_HELP, CONTINUE] {
        assert!(screen.contains(copy), "{copy}:\n{screen}");
    }

    // → steps the harness in place; the picker lists every runner.
    press(&mut page, KeyCode::Right);
    assert_eq!(page.state().runner(), RunnerKind::Codex);
    press(&mut page, KeyCode::Enter);
    assert!(draw(&mut page).contains("Generic"), "the picker is open");
    press(&mut page, KeyCode::Up);
    press(&mut page, KeyCode::Enter);
    assert_eq!(page.state().runner(), RunnerKind::Claude);
    // Down to Default model; its picker: Use runner default, Opus, …
    press(&mut page, KeyCode::Down);
    assert_eq!(page.focus(), Item::Model);
    press(&mut page, KeyCode::Right);
    assert_eq!(page.state().model_choice(), Some("opus"));
    assert!(draw(&mut page).contains("Opus"));
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.focus(), Item::Continue);
    press(&mut page, KeyCode::Enter);
    assert_eq!(page.step(), OnboardingStep::Instructions);
    assert_eq!(page.focus(), Item::Instructions);
    let screen = draw(&mut page);
    for copy in [INSTRUCTIONS_TITLE, INSTRUCTIONS_BODY, INSTRUCTIONS_LABEL, INSTRUCTIONS_FORMAT, BACK, LAUNCH] {
        assert!(screen.contains(copy), "{copy}:\n{screen}");
    }
    let manifest = std::fs::read_to_string(home.agents().join("orchestrator/agent.yaml")).unwrap();
    assert!(manifest.contains("model: opus"), "{manifest}");

    // Typing goes into the editor (Ctrl+Home to its start first).
    page.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
    for c in "Hi. ".chars() {
        press(&mut page, KeyCode::Char(c));
    }
    assert!(page.state().instructions().starts_with("Hi. "), "{}", page.state().instructions());
    // Esc is Back, keeping the edit; Continue comes back to it.
    press(&mut page, KeyCode::Esc);
    assert_eq!(page.step(), OnboardingStep::Harness);
    press(&mut page, KeyCode::BackTab);
    press(&mut page, KeyCode::Enter);
    assert_eq!(page.step(), OnboardingStep::Instructions);
    assert!(page.state().instructions().starts_with("Hi. "));

    // Tab to Launch Orchestrator.
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.focus(), Item::Back);
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.focus(), Item::Launch);
    assert_eq!(press(&mut page, KeyCode::Enter), PageAction::Launch);
    let text = std::fs::read_to_string(home.agents().join("orchestrator/instructions.md")).unwrap();
    assert!(text.starts_with("Hi. "));
    assert!(!shelbi_app::onboarding::needs_onboarding(PROJECT));
}

#[test]
fn the_mouse_walks_all_three_steps() {
    let _home = home("mouse");
    let mut page = page();
    click(&mut page, |h| *h == Hit::Item(Item::GetStarted));
    assert_eq!(page.step(), OnboardingStep::Harness);
    // Open the harness picker and pick Codex.
    click(&mut page, |h| *h == Hit::Item(Item::Harness));
    click(&mut page, |h| *h == Hit::Choice(1));
    assert_eq!(page.state().runner(), RunnerKind::Codex);
    // Default model: GPT-5.
    click(&mut page, |h| *h == Hit::Item(Item::Model));
    click(&mut page, |h| *h == Hit::Choice(1));
    assert_eq!(page.state().model_choice(), Some("gpt-5"));
    click(&mut page, |h| *h == Hit::Item(Item::Continue));
    assert_eq!(page.step(), OnboardingStep::Instructions, "{:?}", page.state().model().error);
    // Back, then Continue again, then Launch.
    click(&mut page, |h| *h == Hit::Item(Item::Back));
    assert_eq!(page.step(), OnboardingStep::Harness);
    assert_eq!(page.state().runner(), RunnerKind::Codex, "Back kept the choice");
    click(&mut page, |h| *h == Hit::Item(Item::Continue));
    assert_eq!(click(&mut page, |h| *h == Hit::Item(Item::Launch)), PageAction::Launch);
    let project = shelbi_state::load_project(PROJECT).unwrap();
    assert_eq!(project.orchestrator.runner, "codex");
}

#[test]
fn a_custom_model_is_typed_in_the_box() {
    let _home = home("custom");
    let mut page = page();
    press(&mut page, KeyCode::Enter);
    press(&mut page, KeyCode::Down);
    press(&mut page, KeyCode::Enter);
    // Custom model… is the last choice.
    let n = page.choices(Item::Model).0.len();
    for _ in 0..n - 1 {
        press(&mut page, KeyCode::Down);
    }
    press(&mut page, KeyCode::Enter);
    for c in "claude-opus-4-8".chars() {
        press(&mut page, KeyCode::Char(c));
    }
    press(&mut page, KeyCode::Enter);
    assert_eq!(page.state().model_choice(), Some("claude-opus-4-8"));
    assert!(draw(&mut page).contains("claude-opus-4-8"));
}

#[test]
fn a_generic_harness_disables_default_model() {
    let _home = home("generic");
    let mut page = page();
    press(&mut page, KeyCode::Enter);
    press(&mut page, KeyCode::Right);
    press(&mut page, KeyCode::Right);
    assert_eq!(page.state().runner(), RunnerKind::Generic);
    // Model leaves the Tab order.
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.focus(), Item::Continue);
    let screen = draw(&mut page);
    assert!(screen.contains("Generic runners take no model."), "{screen}");
    press(&mut page, KeyCode::Enter);
    assert_eq!(page.step(), OnboardingStep::Harness);
    assert!(draw(&mut page).contains("No generic runner is declared"));
}

#[test]
fn it_draws_in_a_small_main_area() {
    let _home = home("small");
    let mut page = page();
    for (w, h) in [(30, 10), (60, 20), (8, 4), (3, 2)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| page.render(f, f.area())).unwrap();
    }
    press(&mut page, KeyCode::Enter);
    press(&mut page, KeyCode::Tab);
    press(&mut page, KeyCode::Tab);
    press(&mut page, KeyCode::Enter);
    for (w, h) in [(30, 10), (60, 20), (8, 4)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| page.render(f, f.area())).unwrap();
    }
}

