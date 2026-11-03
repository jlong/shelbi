//! The Harness and Default model pickers, toolkit-free: a runner kind and
//! the model it runs, with the choices each picker offers. One model behind
//! every place that asks for them: the orchestrator onboarding's "Choose
//! your Orchestrator" step ([`crate::onboarding`]) and the new-project flow's
//! "Agent defaults" step ([`crate::add_project_wizard`] on the desktop,
//! [`crate::overlay::add_project`] in the TUI).
//!
//! Harness lists [`runner_options`] (Claude Code · Anthropic, Codex ·
//! OpenAI, Generic). Default model offers "Use runner default" (`None`),
//! then the runner's [`shelbi_core::model_suggestions`], then the chosen
//! model when it's none of those (a name typed in, or one already on disk).
//! A runner with no suggestions takes no model.

use shelbi_core::RunnerKind;
use shelbi_state::agents_config::runner_options;

use crate::settings::agent_editor::RUNNER_DEFAULT;
use crate::settings::agents::runner_label;
use crate::view::{HarnessOption, ModelOption};

/// The company behind a runner, shown beside its name.
pub fn provider(kind: RunnerKind) -> Option<&'static str> {
    match kind {
        RunnerKind::Claude => Some("Anthropic"),
        RunnerKind::Codex => Some("OpenAI"),
        RunnerKind::Generic => None,
    }
}

/// A model's name as the picker shows it: the suggested aliases by their
/// product names, anything else as typed.
pub fn model_label(model: &str) -> String {
    match model {
        "opus" => "Opus".into(),
        "sonnet" => "Sonnet".into(),
        "haiku" => "Haiku".into(),
        "gpt-5" => "GPT-5".into(),
        "gpt-5-codex" => "GPT-5 Codex".into(),
        other => other.into(),
    }
}

/// `kind` takes a model Shelbi can set.
pub fn takes_model(kind: RunnerKind) -> bool {
    !shelbi_core::model_suggestions(kind).is_empty()
}

/// The note under Default model for a runner that takes none.
pub fn no_model_note(kind: RunnerKind) -> String {
    format!("{} runners take no model.", runner_label(kind))
}

/// The two pickers' choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPicker {
    runner: RunnerKind,
    /// `None`: the runner's default.
    model: Option<String>,
}

impl Default for HarnessPicker {
    /// Claude Code on its default model.
    fn default() -> Self {
        HarnessPicker::new(RunnerKind::Claude, None)
    }
}

impl HarnessPicker {
    pub fn new(runner: RunnerKind, model: Option<String>) -> HarnessPicker {
        let mut picker = HarnessPicker { runner, model: None };
        picker.set_model(model.as_deref());
        picker
    }

    /// The runner chosen.
    pub fn runner(&self) -> RunnerKind {
        self.runner
    }

    /// The model chosen (`None`: the runner's default).
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Choose the runner, starting its model on `model` (what that runner
    /// already uses, or `None`). `false` when it was already chosen (the
    /// model is kept).
    pub fn set_runner(&mut self, kind: RunnerKind, model: Option<String>) -> bool {
        if kind == self.runner {
            return false;
        }
        self.runner = kind;
        self.model = None;
        self.set_model(model.as_deref());
        true
    }

    /// Choose the model: a suggestion, any other text (trimmed; blank is the
    /// runner's default), or `None`. A runner that takes no model keeps
    /// `None`.
    pub fn set_model(&mut self, model: Option<&str>) {
        self.model = model
            .map(str::trim)
            .filter(|m| !m.is_empty() && self.takes_model())
            .map(str::to_string);
    }

    /// The chosen runner takes a model Shelbi can set.
    pub fn takes_model(&self) -> bool {
        takes_model(self.runner)
    }

    /// Harness's choices, the chosen one selected.
    pub fn harnesses(&self) -> Vec<HarnessOption> {
        runner_options()
            .into_iter()
            .map(|r| HarnessOption {
                kind: r.kind,
                label: runner_label(r.kind).into(),
                provider: provider(r.kind).map(str::to_string),
                selected: r.kind == self.runner,
            })
            .collect()
    }

    /// Default model's choices: the runner's default, its suggestions, and
    /// the chosen model when it's none of them. Empty for a runner that
    /// takes no model.
    pub fn models(&self) -> Vec<ModelOption> {
        if !self.takes_model() {
            return Vec::new();
        }
        let mut out = vec![ModelOption {
            model: None,
            label: RUNNER_DEFAULT.into(),
            selected: self.model.is_none(),
        }];
        let mut models: Vec<String> =
            shelbi_core::model_suggestions(self.runner).iter().map(|m| m.to_string()).collect();
        if let Some(own) = &self.model {
            if !models.contains(own) {
                models.push(own.clone());
            }
        }
        out.extend(models.into_iter().map(|m| ModelOption {
            label: model_label(&m),
            selected: self.model.as_deref() == Some(m.as_str()),
            model: Some(m),
        }));
        out
    }

    /// What the Default model picker shows.
    pub fn model_label(&self) -> String {
        self.model.as_deref().map_or_else(|| RUNNER_DEFAULT.to_string(), model_label)
    }

    /// The choice as the new-project flow saves it.
    pub fn defaults(&self) -> shelbi_core::AgentDefaults {
        shelbi_core::AgentDefaults { runner: self.runner, model: self.model.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_starts_on_claude_code_and_its_default_model() {
        let p = HarnessPicker::default();
        assert_eq!(p.runner(), RunnerKind::Claude);
        assert_eq!(p.model(), None);
        assert_eq!(p.model_label(), RUNNER_DEFAULT);
        let h = p.harnesses();
        assert_eq!(
            h.iter().map(|h| (h.label.as_str(), h.provider.as_deref())).collect::<Vec<_>>(),
            [("Claude Code", Some("Anthropic")), ("Codex", Some("OpenAI")), ("Generic", None)]
        );
        assert!(h[0].selected && !h[1].selected);
        let m = p.models();
        assert_eq!(m.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), [RUNNER_DEFAULT, "Opus", "Sonnet", "Haiku"]);
        assert!(m[0].selected);
    }

    #[test]
    fn a_new_runner_starts_on_the_model_it_is_handed() {
        let mut p = HarnessPicker::default();
        p.set_model(Some("opus"));
        assert!(p.set_runner(RunnerKind::Codex, Some("gpt-5".into())));
        assert_eq!(p.model(), Some("gpt-5"));
        assert_eq!(p.model_label(), "GPT-5");
        assert!(!p.set_runner(RunnerKind::Codex, None), "the same runner keeps its model");
        assert_eq!(p.model(), Some("gpt-5"));
    }

    #[test]
    fn a_typed_model_is_trimmed_and_listed() {
        let mut p = HarnessPicker::default();
        p.set_model(Some("  claude-opus-5-5 "));
        assert_eq!(p.model(), Some("claude-opus-5-5"));
        let m = p.models();
        assert_eq!(m.last().map(|o| (o.label.as_str(), o.selected)), Some(("claude-opus-5-5", true)));
        p.set_model(Some("   "));
        assert_eq!(p.model(), None, "blank is the runner's default");
    }

    #[test]
    fn generic_takes_no_model() {
        let mut p = HarnessPicker::default();
        p.set_runner(RunnerKind::Generic, Some("x".into()));
        assert_eq!(p.model(), None);
        assert!(!p.takes_model());
        assert!(p.models().is_empty());
        assert_eq!(no_model_note(RunnerKind::Generic), "Generic runners take no model.");
    }
}
