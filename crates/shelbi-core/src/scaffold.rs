//! Self-documenting scaffolds for the config files `shelbi init` writes.
//!
//! Every file shelbi materializes — the project `<name>.yaml`, a project's
//! `workflows/default.yaml`, its `workflows/statuses.yaml`, and the hub-wide
//! `config.yaml` — ships with the required fields populated *and* a
//! commented-out example for every optional feature, each explained inline,
//! under a header comment pointing at the full reference docs. Opening the
//! file you want to edit is enough to see how each knob works; the docs stay
//! the deep reference.
//!
//! ## How the commenting works
//!
//! The active/required fields are rendered by serde (never string
//! interpolation) so a hostile value can't corrupt the file. Optional
//! sections are appended as static, hand-written YAML that is grounded in the
//! real serde structs and then commented with a single leading `#` per line
//! ([`comment_block`]); explanatory prose uses a double `##` so it survives
//! as a comment even after a section is uncommented. Because every optional
//! block is an *additive* key (or list item) that the required file doesn't
//! already carry, uncommenting any subset yields valid, parseable config —
//! the `uncomment`-round-trip tests in this module enforce exactly that.

use crate::statuses::default_project_statuses;
use crate::workflow::default_workflow;
use crate::Result;

/// One optional, commented-out section: a short prose header (rendered as
/// `##` comment lines) followed by grounded example YAML (rendered as `#`
/// comment lines). The `yaml` must be valid on its own so that uncommenting
/// the section — alongside the file's required fields — parses.
struct Section {
    /// Explanatory lines shown above the example, without the `## ` prefix.
    prose: &'static [&'static str],
    /// Example YAML for the feature, exactly as it should read once
    /// uncommented (indentation included). Grounded in the serde structs.
    yaml: &'static str,
}

/// Prefix every line of `yaml` with a single `#` so the block reads as a
/// commented-out example. Blank lines become a bare `#`. Uncommenting is the
/// inverse: strip exactly one leading `#` (see the test helper).
fn comment_block(yaml: &str) -> String {
    let mut out = String::new();
    for line in yaml.lines() {
        if line.is_empty() {
            out.push('#');
        } else {
            out.push('#');
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Render `## prose` header lines for a section.
fn prose_block(prose: &[&str]) -> String {
    let mut out = String::new();
    for line in prose {
        if line.is_empty() {
            out.push_str("##");
        } else {
            out.push_str("## ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Render a list of optional sections into a single trailing block:
/// each section is its prose header, its commented example, and a blank
/// separator line.
fn render_sections(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections {
        out.push_str(&prose_block(s.prose));
        out.push_str(&comment_block(s.yaml));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Project YAML

const PROJECT_HEADER: &str = "\
## Shelbi project config — full reference: https://shelbi.dev/docs/configuration/project
## Required fields are populated below. Every commented block beneath them is an
## optional feature: uncomment and edit it to turn the feature on.

";

/// Commented example of a second, SSH machine — inserted into the required
/// `machines:` list so uncommenting it adds a sibling entry.
const MACHINES_SSH: Section = Section {
    prose: &[
        "Add more machines to the list above — e.g. a remote box reached over",
        "SSH. `host` is the ssh hostname (falls back to `name` when omitted);",
        "`work_dir` is where the checkout and worktrees live on that box.",
    ],
    yaml: "\
- name: devbox
  kind: ssh
  host: devbox.local
  work_dir: ~/work/myapp
",
};

/// Commented example of per-runner `dialog_signatures`, indented so it nests
/// under the last runner (`codex`) in the required `agent_runners:` map.
const RUNNER_DIALOG: Section = Section {
    prose: &[
        "Per-runner extras (add under a runner above). `flags:` appends args to",
        "every launch (e.g. flags: [\"--full-auto\"]); `dialog_signatures:` teaches",
        "the hub poller a new blocking-prompt string so a frozen pane surfaces.",
    ],
    // NB: written with explicit `\n` (not a `"\` block) so the leading
    // indent on the first line survives — a trailing backslash would strip it.
    yaml: "    dialog_signatures:\n      - { kind: trust, pattern: \"Do you trust the files\" }\n",
};

/// Optional top-level project keys, none of which the required file emits, so
/// each is additive and independently uncommentable.
const PROJECT_SECTIONS: &[Section] = &[
    Section {
        prose: &[
            "Workspace pool: long-lived slots that pick up tasks from the board.",
            "Each owns a worktree at <machine.work_dir>/.shelbi/wt/<name>. A slot",
            "with role: review is reserved for loading & serving a branch for a human.",
        ],
        yaml: "\
workspaces:
  - { name: alice, machine: hub, runner: claude }
  - { name: rev, machine: hub, runner: claude, role: review }
",
    },
    Section {
        prose: &["Informational GitHub URL, recorded by the setup wizard."],
        yaml: "github_url: git@github.com:me/myapp.git\n",
    },
    Section {
        prose: &["How often (seconds) the hub poller samples each workspace pane. Default 5."],
        yaml: "workspace_poll_interval_secs: 5\n",
    },
    Section {
        prose: &[
            "Permissions posture rendered into the workspace settings template.",
            "`auto` maps to claude's acceptEdits. Default auto.",
        ],
        yaml: "workspace_permissions_mode: auto\n",
    },
    Section {
        prose: &[
            "Recurring hub heartbeat written to events.log so the orchestrator's",
            "watch fires on a quiet board. A duration (45s, 3m, 1h) or `off`. Default 3m.",
        ],
        yaml: "heartbeat: 3m\n",
    },
    Section {
        prose: &["Base branch and merge strategy for `shelbi merge` and Zen auto-merge."],
        yaml: "\
git:
  base_branch: main          # defaults to default_branch when unset
  merge_strategy: squash     # squash | merge | rebase
",
    },
    Section {
        prose: &["Zen Mode: local checks that gate a promotion, CI wait, and danger globs."],
        yaml: "\
zen:
  checks:
    local:
      - cargo test --workspace
  ci_timeout: 900            # seconds Zen waits for CI. Default 900 (15m)
  danger_paths:
    extend: [\".env\", \"infra/**\"]   # or `override: [...]`, or a bare list
",
    },
    Section {
        prose: &["Review workspaces: how a task's branch is loaded and served for review."],
        yaml: "\
review:
  base_port: 3000            # first review dev-server port
  port_stride: 10            # spacing between concurrent review servers
  setup:
    - npm install            # commands before the server (auto-detected if omitted)
  serve: npm run dev -- --port $PORT
  ready_probe:
    http: http://localhost:$PORT
    timeout: 90              # seconds to wait for readiness. Default 90
",
    },
    Section {
        prose: &[
            "ContextStore spaces rsynced from a remote workspace's machine back to",
            "the hub after it hands a task off for review.",
        ],
        yaml: "\
contextstore_sync:
  - { space: Memories, path: ~/.contextstore/Memories }
",
    },
];

/// Decorate the serde-rendered required project YAML (`active`) with a header
/// comment and commented-out examples for every optional feature. The SSH
/// machine example is spliced into the `machines:` list (before the
/// `orchestrator:` key); the runner-dialog example is appended so it nests
/// under the last runner; the remaining optional keys follow as an additive
/// trailing block.
pub fn decorate_project_yaml(active: &str) -> String {
    let mut out = String::new();
    out.push_str(PROJECT_HEADER);

    // Splice the SSH-machine example into the machines list, immediately
    // before the top-level `orchestrator:` key that serde always emits next.
    let machines_hint = {
        let mut s = prose_block(MACHINES_SSH.prose);
        s.push_str(&comment_block(MACHINES_SSH.yaml));
        s
    };
    let mut spliced = false;
    for line in active.lines() {
        if !spliced && line == "orchestrator:" {
            out.push_str(&machines_hint);
            spliced = true;
        }
        out.push_str(line);
        out.push('\n');
    }

    // Runner-level extras nest under the last runner rendered above.
    out.push_str(&prose_block(RUNNER_DIALOG.prose));
    out.push_str(&comment_block(RUNNER_DIALOG.yaml));
    out.push('\n');

    out.push_str(&render_sections(PROJECT_SECTIONS));
    out
}

// ---------------------------------------------------------------------------
// Workflow YAML

const WORKFLOW_HEADER: &str = "\
## Shelbi workflow — full reference: https://shelbi.dev/docs/configuration/workflow
## Statuses are reference-only (id + owner + optional agent); each status's
## display name & category live in workflows/statuses.yaml. Commented blocks
## below are optional: uncomment to enable.

";

const WORKFLOW_SECTIONS: &[Section] = &[
    Section {
        prose: &["Stable id a new task lands in. Defaults to the first status when unset."],
        yaml: "initial_status: backlog\n",
    },
    Section {
        prose: &[
            "Hub-side side-effects fired when a task crosses an edge. Moves are",
            "any-to-any; unlisted edges are pure status changes. Actions: push_branch,",
            "open_pr, merge, close_pr, delete_branch, restack.",
        ],
        yaml: "\
transitions:
  - { from: in-progress, to: review, actions: [push_branch, open_pr] }
  - { from: review, to: done, actions: [merge, delete_branch] }
",
    },
    Section {
        prose: &["Per-workflow git override (inherits the project git: block when unset)."],
        yaml: "\
git:
  base_branch: main
  merge_strategy: squash
",
    },
    Section {
        prose: &[
            "Per-workflow Zen override — each subfield independently optional. The",
            "canonical use is a research workflow opting out of the project's checks.",
        ],
        yaml: "\
zen:
  checks:
    local: []                # replace the project's checks for this workflow
  ci_timeout: 900
",
    },
];

/// The self-documenting `workflows/default.yaml` written for a fresh project:
/// the serialized [`default_workflow`] plus a header and commented optional
/// blocks (initial_status, transitions, per-workflow git/zen).
pub fn default_workflow_yaml() -> Result<String> {
    let active = serde_yaml::to_string(&default_workflow())?;
    Ok(format!(
        "{WORKFLOW_HEADER}{active}\n{}",
        render_sections(WORKFLOW_SECTIONS)
    ))
}

// ---------------------------------------------------------------------------
// statuses.yaml

const STATUSES_HEADER: &str = "\
## Shelbi statuses — full reference: https://shelbi.dev/docs/configuration/statuses
## The project-wide catalog of status identity (id, display name, category).
## Workflows reference these by id. Add your own below the defaults.

";

const STATUSES_SECTIONS: &[Section] = &[Section {
    prose: &[
        "Add custom statuses to the list above. `category` is a fixed vocabulary:",
        "backlog | ready | active | handoff | done | archived. Keep at least one",
        "terminal (done/archived) status so tasks can complete.",
    ],
    yaml: "\
- id: qa
  name: QA
  category: handoff
- id: blocked
  name: Blocked
  category: active
",
}];

/// The self-documenting `workflows/statuses.yaml` written for a fresh project:
/// the serialized [`default_project_statuses`] plus a header and a commented
/// example of adding custom statuses.
pub fn default_statuses_yaml() -> Result<String> {
    let active = serde_yaml::to_string(&default_project_statuses())?;
    Ok(format!(
        "{STATUSES_HEADER}{active}\n{}",
        render_sections(STATUSES_SECTIONS)
    ))
}

// ---------------------------------------------------------------------------
// Global config.yaml

/// The self-documenting hub-wide `~/.shelbi/config.yaml`. Everything here is
/// optional (absent/partial files fall back to defaults), so the file ships
/// the one current knob at its default value with the alternatives documented
/// inline and a pointer to the sibling `keys.yaml`.
pub const CONFIG_YAML: &str = "\
## Shelbi global config — full reference: https://shelbi.dev/docs/configuration/global
## Per-user UI preferences, applied across every project on this machine.
## Absent or partial files fall back to built-in defaults.

keymap:
  ## Chord that toggles Zen Mode. Legacy — prefer setting this in keys.yaml
  ## (defaults.global.zen_toggle). One of: alt-z (default), ctrl-backslash,
  ## ctrl-g, ctrl-shift-z, none.
  zen_toggle: alt-z

## Keybinding overrides live in the sibling file ~/.shelbi/keys.yaml.
## See https://shelbi.dev/docs/configuration/global
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, ProjectStatuses, Workflow};

    /// Strip exactly one leading `#` from every line — the inverse of
    /// [`comment_block`]. A `##` prose line becomes a plain `#` comment
    /// (still ignored by YAML); a `#`-commented example line becomes active.
    fn uncomment(s: &str) -> String {
        let mut out = String::new();
        for line in s.lines() {
            out.push_str(line.strip_prefix('#').unwrap_or(line));
            out.push('\n');
        }
        out
    }

    /// A minimal required project YAML mirroring what `shelbi init`'s serde
    /// renderer emits (name/repo/default_branch/machines/orchestrator/
    /// agent_runners), used to exercise the decoration + uncomment round-trip.
    const REQUIRED_PROJECT: &str = "\
name: myapp
repo: ''
default_branch: main
machines:
- name: hub
  kind: local
  work_dir: /tmp/myapp
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
  codex:
    command: codex
    flags: []
";

    #[test]
    fn decorated_project_parses_as_is_with_only_required_fields() {
        let decorated = decorate_project_yaml(REQUIRED_PROJECT);
        // Header link is present.
        assert!(decorated.contains("https://shelbi.dev/docs/configuration/project"));
        // As written, the optional blocks are inert comments.
        let p = Project::from_yaml_str(&decorated).expect("decorated project parses");
        assert_eq!(p.name, "myapp");
        assert_eq!(p.machines.len(), 1, "ssh example must stay commented");
        assert!(p.workspaces.is_empty(), "workspaces example must stay commented");
        assert!(p.contextstore_sync.is_empty());
    }

    #[test]
    fn uncommenting_every_project_section_yields_valid_config() {
        let decorated = decorate_project_yaml(REQUIRED_PROJECT);
        let enabled = uncomment(&decorated);
        let p = Project::from_yaml_str(&enabled)
            .expect("every optional project section parses when uncommented");
        p.validate_workspaces()
            .expect("uncommented workspaces reference real machines/runners");
        // Spot-check that representative optional features actually turned on.
        assert_eq!(p.machines.len(), 2, "ssh machine example uncommented");
        assert!(p.machines.iter().any(|m| m.host.as_deref() == Some("devbox.local")));
        assert_eq!(p.workspaces.len(), 2);
        assert!(p.workspaces.iter().any(|w| w.is_review()));
        assert!(!p.contextstore_sync.is_empty());
        assert_eq!(p.github_url.as_deref(), Some("git@github.com:me/myapp.git"));
        // dialog_signatures nested under the codex runner.
        let codex = p.agent_runners.get("codex").expect("codex runner");
        assert!(!codex.dialog_signatures.is_empty());
    }

    #[test]
    fn decorated_workflow_parses_and_uncomments_cleanly() {
        let yaml = default_workflow_yaml().unwrap();
        assert!(yaml.contains("https://shelbi.dev/docs/configuration/workflow"));
        // As written: default six-status flow, no transitions.
        let wf = Workflow::from_yaml_str(&yaml).expect("decorated workflow parses");
        assert_eq!(wf.name, "default");
        assert!(wf.transitions.is_none(), "transitions example stays commented");

        // Uncommented: transitions/git/zen/initial_status all turn on and validate.
        let wf = Workflow::from_yaml_str(&uncomment(&yaml))
            .expect("every optional workflow section parses when uncommented");
        let transitions = wf.transitions.expect("transitions uncommented");
        assert_eq!(transitions.len(), 2);
        assert!(wf.git.is_some());
        assert!(wf.zen.is_some());
        assert_eq!(wf.initial_status.as_deref(), Some("backlog"));
    }

    #[test]
    fn decorated_statuses_parses_and_uncomments_cleanly() {
        let yaml = default_statuses_yaml().unwrap();
        assert!(yaml.contains("https://shelbi.dev/docs/configuration/statuses"));
        // As written: exactly the canonical six.
        let st = ProjectStatuses::from_yaml_str(&yaml).expect("decorated statuses parse");
        assert_eq!(st, default_project_statuses());

        // Uncommented: the custom-status example is appended and still valid.
        let st = ProjectStatuses::from_yaml_str(&uncomment(&yaml))
            .expect("custom-status example parses when uncommented");
        assert!(st.get("qa").is_some());
        assert!(st.get("blocked").is_some());
    }

    #[test]
    fn config_yaml_scaffold_parses_as_user_config_shape() {
        // The global config is owned by shelbi-state; here we only assert the
        // scaffold is valid YAML carrying the documented keymap key, plus the
        // docs pointer. shelbi-state's own test round-trips it through
        // UserConfig.
        assert!(CONFIG_YAML.contains("https://shelbi.dev/docs/configuration/global"));
        let v: serde_yaml::Value = serde_yaml::from_str(CONFIG_YAML).expect("config.yaml parses");
        let zen = v
            .get("keymap")
            .and_then(|k| k.get("zen_toggle"))
            .and_then(|z| z.as_str());
        assert_eq!(zen, Some("alt-z"));
    }
}
