# Adversarial review: core domain model (shelbi-core)

Reviewed (line counts as checked out on `shelbi/adv-review-core-model`, rebased onto `origin/main` @ `35e78e3`):

- `crates/shelbi-core/src/model.rs` (2487 lines)
- `crates/shelbi-core/src/workflow.rs` (2241 lines)
- `crates/shelbi-core/src/statuses.rs` (250 lines)
- `crates/shelbi-core/src/placeholders.rs` (231 lines)
- `crates/shelbi-core/src/system_memory.rs` (173 lines)
- `crates/shelbi-core/src/workspace_names.rs` (162 lines)
- `crates/shelbi-core/src/error.rs` (79 lines)
- `crates/shelbi-core/src/lib.rs` (26 lines)

> Note on scope: the task brief listed larger line counts (model.rs 3047, etc.) and a `workspace_names.rs` filename. The branch was 175 commits behind `origin/main`; after the required rebase, the tree matches the intended slice (`workspace_names.rs` is the correct filename; `worker_names.rs` was the pre-rebase name). Findings are cited against the post-rebase tree, which is what the review will diff.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | `fires_merge_bar` legacy fallback keys off the hardcoded id `"review"` instead of `StatusCategory::Handoff` — a renamed handoff id in a no-transitions workflow silently loses the Zen merge probe | medium | certain | bug / assumption |
| F2 | Task ids permit uppercase and are stored case-preserving as `<id>.md` and `shelbi/<id>` refs — two ids differing only in case collide on macOS's default case-insensitive FS/git → silent overwrite / data loss | medium | likely | hardening |
| F3 | No model-level validation of category invariants; combined with any-to-any transitions, a status set can omit `Handoff`/`Done`, duplicate a category, and jump categories arbitrarily | medium | likely | assumption |
| F4 | `Column::default_status_name(InProgress)` returns `"InProgress"` but the canonical default workflow's display name is `"In Progress"` — the label is rendered to users at kanban.rs:2233 | low | certain | bug |
| F5 | The transition side-effect layer (`Transition.actions`, `Transition.target`, `actions_for_transition`) has no executor anywhere in the tree; `target` `{{var}}` placeholders are never substituted | low | certain | best-practice / dead-code |
| F6 | `Task.params` uses `#[serde(flatten)]` with no `deny_unknown_fields`, so typos of optional fields are silently captured as params; non-string extra values hard-fail the whole task parse with a confusing type error | low | certain | best-practice |
| F7 | `ZenDangerPaths` accepts only the `{extend:}` / `{override:}` map form; the intuitive `danger_paths: [..]` list form fails to parse and aborts the whole project load | low | certain | best-practice / hardening |
| F8 | `convert_raw_workflow` derives the migrated agent from a sentinel `Backlog` category before `resolve_against` fills the real one, producing a misleading "no category default for `backlog`" error for compact-form legacy workflows | low | speculative | assumption |

---

## F1: `fires_merge_bar` legacy fallback is keyed to the literal id `"review"`, not the `Handoff` category

- **Where:** `crates/shelbi-core/src/workflow.rs:234`–`239` (and the constant at `:720`)
- **Category:** bug / challenged-assumption
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  pub fn fires_merge_bar(&self, from: &str) -> bool {
      match &self.transitions {
          Some(_) => !self.outgoing_merge_transitions(from).is_empty(),
          None => from == LEGACY_REVIEW_STATUS,   // LEGACY_REVIEW_STATUS = "review"
      }
  }
  ```

  The whole point of `StatusCategory` (workflow.rs:630–651) is that "Status *names* are user-customizable; categories are not — generic code keys off the category so a workflow that renames `Review` to `QA` still triggers the auto-merge rule." But the `None` (no-`transitions:` block) branch — which is exactly what the shipped `default_workflow()` uses (`transitions: None`, workflow.rs:526) and what every migrated existing project has on disk — compares the *stable id* to the string `"review"`.

  `statuses.yml` lets a project author any id (statuses.rs:33–44); the default is `"review"` but nothing forces it. The sole consumer, `zen::dry_run_tick`, has already filtered to `category == StatusCategory::Handoff` before calling this (zen.rs:2236–2244):

  ```rust
  if category != StatusCategory::Handoff { continue; }
  let status_id = status.map(|s| s.id.as_str())
      .unwrap_or_else(|| tf.task.column.default_status_id());
  let fires_bar = workflow_ref.map(|w| w.fires_merge_bar(status_id)).unwrap_or(true);
  if !fires_bar { continue; }
  ```

  The module doc there (zen.rs:2207–2209) explicitly promises: *"a custom workflow whose handoff status is named `QA` … (instead of `Review`) trips the same bar."* That promise is false whenever the handoff status *id* ≠ `"review"` and the workflow has no `transitions:` block.
- **Failure scenario:** A project customizes `statuses.yml` to `{ id: qa, name: QA, category: handoff }` and keeps a default-style workflow with no `transitions:` block. A task sits in `qa`. `dry_run_tick` passes the `category == Handoff` gate, then `fires_merge_bar("qa")` returns `"qa" == "review"` → `false` → the task is silently skipped from the merge-probe preview. The user believes Zen is watching a handoff it is not.
- **Recommendation:** The caller already guarantees `Handoff`; the `None` branch should not re-derive the trigger from an id. Either pass the category into `fires_merge_bar` and fall back on `category == StatusCategory::Handoff`, or drop the id comparison and return `true` in the `None` case (the caller's category gate is the real filter). At minimum, add a `Workflow::validate` / `resolve_against` check that warns when a no-transitions workflow's handoff status id ≠ `LEGACY_REVIEW_STATUS`.
- **Effort:** S

## F2: Case-preserving task ids collide on macOS's default case-insensitive filesystem and git refs

- **Where:** `crates/shelbi-core/src/model.rs:1103`–`1119` (`validate_agent_id`) and `:702`–`712` (`validate_task_id`)
- **Category:** hardening (data loss)
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  pub fn validate_agent_id(s: &str) -> crate::Result<()> {
      ...
      let ok = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
      let starts_ok = s.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false);
      ...
  }
  ```

  `is_ascii_alphanumeric()` accepts `A`–`Z`, and the validator neither lowercases nor rejects uppercase. Model-level uniqueness is byte-exact `String` equality (e.g. statuses.rs:72 `seen.insert(st.id.as_str())`), so `"Fix-Bug"` and `"fix-bug"` are treated as two distinct tasks. But the state layer persists each task at `tasks_dir(project)?.join(format!("{id}.md"))` (`shelbi-state/src/lib.rs:1030`) and derives the branch `shelbi/<id>` (e.g. `shelbi-cli/.../spawn.rs:69`). macOS's default APFS/HFS+ volumes and git's default `core.ignorecase=true` are case-insensitive.
- **Failure scenario:** On a macOS hub (this is a macOS-first tool), a user creates task `Fix-Login`, then later `fix-login`. Both pass `validate_task_id`. `save_task` writes `Fix-Login.md` then `fix-login.md` — the second silently overwrites the first on a case-insensitive volume, and the two `shelbi/Fix-Login` / `shelbi/fix-login` branches collide. One task's frontmatter/body is lost with no error.
- **Recommendation:** Normalize ids to lowercase in `validate_agent_id`/`validate_task_id` (reject uppercase, or canonicalize), or add a case-insensitive uniqueness check at the point ids are minted. Rejecting uppercase is the smallest safe change and matches the "conventional form is lowercase kebab-case" comment already in the code (statuses.rs:35).
- **Effort:** S

## F3: Category invariants are enforced only by convention; any-to-any transitions allow illegal category jumps

- **Where:** `crates/shelbi-core/src/workflow.rs:246`–`334` (`validate`) and `:174`–`176` (`transition_allowed`)
- **Category:** challenged-assumption
- **Severity / Confidence:** medium / likely
- **Evidence:** `Workflow::validate` checks non-empty/unique ids, a resolvable `initial_status`, and transition endpoints — but never inspects the *set* of `StatusCategory` values. A workflow (or the `statuses.yml` it resolves against) may legally declare zero `Handoff` statuses, zero `Done`, two `Ready`, or two `Handoff`. `transition_allowed` is pure existence:

  ```rust
  pub fn transition_allowed(&self, from: &str, to: &str) -> bool {
      self.status(from).is_some() && self.status(to).is_some()
  }
  ```

  So a direct status set can jump `backlog -> done` skipping all work (tests at workflow.rs:1585–1592 assert this is intentional per §11). The problem is not the any-to-any policy itself but that *nothing else validates the categories are coherent*, while generic code assumes they are: F1 depends on a `Handoff` existing with a specific id; `Task::is_blocked` / column mapping assume a terminal `Done` exists; the TUI maps `StatusCategory::Handoff => Column::Review` (kanban.rs:2131) assuming a single handoff.
- **Failure scenario:** A hand-authored `statuses.yml` omits any `handoff` category (goes straight `active -> done`). Every `Handoff`-gated feature (the Zen merge probe iteration in `dry_run_tick`, the TUI handoff column) silently has nothing to act on, with no load-time diagnostic telling the author their status set is degenerate.
- **Recommendation:** Add a soft-validation pass (warn, or hard-error for the terminal case) in `ProjectStatuses::validate` / `Workflow::resolve_against`: require at least one terminal (`Done`/`Archived`) category, warn on missing `Handoff`, and warn on duplicate single-instance categories the UI/orchestrator assume are unique.
- **Effort:** M

## F4: `Column::default_status_name(InProgress)` disagrees with the default workflow's display name

- **Where:** `crates/shelbi-core/src/model.rs:548`–`556`
- **Category:** bug (cosmetic / label)
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  pub fn default_status_name(self) -> &'static str {
      match self {
          ...
          Column::InProgress => "InProgress",   // no space
          ...
      }
  }
  ```

  The canonical default workflow's `in-progress` status name is `"In Progress"` (with a space) — see `default_workflow()` at workflow.rs:493 and `default_project_statuses()` at statuses.rs:118–120, and the test that pins it at workflow.rs:1279 (`"In Progress"`). This method is not dead: it feeds the rendered label at `crates/shelbi-tui/src/kanban.rs:2233` (`column_label(task.column.default_status_name())`) for tasks whose only known position is the legacy `Column`.
- **Failure scenario:** A legacy/pre-workflow task in the `InProgress` column renders as `InProgress` in the TUI while workflow-resolved tasks in the same lane render as `In Progress`. Same column, two labels.
- **Recommendation:** Change the arm to `"In Progress"` to match `default_workflow()` and `default_project_statuses()`. (The doc comment on this method claims it returns "the PascalCase status *display name* … under the canonical default workflow" — but the canonical name is not PascalCase-without-space, so the comment is also wrong.)
- **Effort:** S

## F5: The transition side-effect layer is declared, validated, and serialized — but never executed; `target` placeholders are never resolved

- **Where:** `crates/shelbi-core/src/workflow.rs:750`–`814` (`Transition`, `TransitionAction`), `:191`–`195` (`actions_for_transition`), `:421`–`443` (`resolve_git`)
- **Category:** best-practice / dead-code
- **Severity / Confidence:** low / certain
- **Evidence:** A repo-wide grep for consumers of the transition API (`\.transitions`, `actions_for_transition`, `\.transition(`) outside `workflow.rs` returns nothing. The only external consumer of *any* transition method is `fires_merge_bar` (zen.rs:2243), which reads only *whether* an outgoing edge contains `Merge` — it never reads `Transition.actions` to run them, nor `Transition.target`. The live action primitives in `shelbi-orchestrator::actions` (`merge`, `open_pr`, …) are driven by CLI flags (`ActionCmd::Merge { target }`, `ActionCmd::OpenPr { target }` — actions.rs:98/119, `target_override`), *not* by the workflow transition table.

  Separately, `resolve_git` (the documented `{{var}}` substitution entry point, workflow.rs:421) only substitutes `git.base_branch`; `Transition.target` is documented as a branch override (workflow.rs:772–778) but no code path ever runs `substitute_placeholders` over it. So a `target: release/{{version}}` would reach any future consumer with the literal `{{version}}` intact.
- **Failure scenario:** A user authors `transitions: [{ from: review, to: done, actions: [merge, delete_branch], target: release/{{version}} }]`. `Workflow::validate` passes, the file round-trips, and the user reasonably expects moving a task `review -> done` to merge into `release/<version>` and delete the branch. Nothing happens: no executor consumes the actions, and even if one did, `{{version}}` is never substituted.
- **Recommendation:** If the transition executor is intentionally not-yet-wired (Phase N), document that `transitions:` `actions`/`target` are currently declarative-only in the module docs, or gate authoring behind a "not yet enforced" warning. If it is meant to be live, add a `resolve_transition_target(&self, from, to, params)` that runs `substitute_placeholders` and wire `actions_for_transition` into the orchestrator's action dispatch.
- **Effort:** M

## F6: `Task.params` flatten silently swallows typos of optional fields; numeric extras hard-fail the whole task

- **Where:** `crates/shelbi-core/src/model.rs:665`–`666` (and the `Task` struct at `:614`)
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
  pub params: BTreeMap<String, String>,
  ```

  There is no `#[serde(deny_unknown_fields)]` (and `flatten` is incompatible with it). Any frontmatter key that isn't a typed `Task` field lands in `params`. Optional fields (`assigned_to`, `branch`, `workflow`, `prefers_machine`) therefore fail *silently* on typo: `assigned_too: bob` becomes `params["assigned_too"] = "bob"` and `assigned_to` stays `None`. Required fields still error (they have no default), so the failure is specifically for optional-field typos.

  Second edge: because the map value type is `String`, a numeric or boolean extra key (`version: 2`) fails deserialization of the *entire task* with a low-level "invalid type: integer" error rather than a task-scoped message. This is intentional per the doc comment (avoids `{{feature}}` coercing numbers), but the error is opaque to the user who just added a param.
- **Failure scenario:** A user writes `prefer_machine: devbox` (missing the `s`). The task loads without error, is never routed to `devbox`, and the stray key sits invisibly in `params`. Or a user adds `retries: 3` as a param and the whole task file becomes unloadable with a serde type error that names no field.
- **Recommendation:** After deserialization, validate `params` keys against the set of known-but-misspelled field names (Levenshtein-1 to `assigned_to`/`branch`/`workflow`/`prefers_machine`/`zen`) and warn. For the numeric case, deserialize extras as `serde_yaml::Value` and stringify with a clear per-field error, or document the string-only constraint in the user-facing task schema docs.
- **Effort:** M

## F7: `ZenDangerPaths` rejects the natural `danger_paths: [..]` list form, aborting project load

- **Where:** `crates/shelbi-core/src/model.rs:775`–`810`
- **Category:** best-practice / hardening
- **Severity / Confidence:** low / certain
- **Evidence:** `ZenDangerPaths` deserializes `try_from = "ZenDangerPathsRepr"`, and `ZenDangerPathsRepr` (model.rs:790–796) is a struct with `extend` / `override` keys. There is no arm for a bare sequence. A user who writes the obvious

  ```yaml
  zen:
    danger_paths:
      - migrations/**
  ```

  hits a serde type error ("invalid type: sequence, expected struct ZenDangerPathsRepr"), which fails the *entire* `Project` deserialize in `load_project`. The map form (`extend:`/`override:`) is only discoverable from the docs, and the `(None, None)` arm (model.rs:807) means an empty map silently becomes `Extend([])`, so the failure is specific to the list shorthand.
- **Failure scenario:** A user extends danger paths with the intuitive YAML list and their project stops loading with an error that points at serde internals, not at "use `extend:` or `override:`".
- **Recommendation:** Accept a bare sequence as `Extend(seq)` in a custom `Deserialize`/`TryFrom` (untagged: sequence → `Extend`, map → the current struct). Failing that, produce a domain error (`Error::Other`) with the "set either `extend:` or `override:`" hint when a non-map is seen, instead of the raw serde type error.
- **Effort:** S

## F8: Legacy owner migration derives the agent from a sentinel category before the real one is resolved

- **Where:** `crates/shelbi-core/src/workflow.rs:978` and `resolve_owner_agent` at `:1031`–`1067`
- **Category:** challenged-assumption
- **Severity / Confidence:** low / speculative
- **Evidence:** In the compact (reference-only) workflow form, `category:` is absent on the wire, so `convert_raw_workflow` assigns a sentinel:

  ```rust
  let category = st.category.unwrap_or(StatusCategory::Backlog);   // sentinel
  let (owner, agent, migration) = resolve_owner_agent(&id, &st.owner, st.agent, category)?;
  ```

  `resolve_owner_agent` then uses that category to migrate a bare `owner: agent` (workflow.rs:1046–1054), erroring `"no category default for backlog"` for anything but `ready`/`active`. But the *real* category is only filled later by `resolve_against` (workflow.rs:349–366), which runs after conversion. So a compact-form status whose true category is `ready` but that uses the legacy bare `owner: agent` is judged against `Backlog` and rejected with a message naming the wrong category.
- **Failure scenario:** A workflow file mixing the new compact form (`id` + `owner`, no inline `category`) with a legacy bare `owner: agent` fails to load with `"status \`x\` has owner: agent but no agent: field … (no category default for \`backlog\`)"`, even though `statuses.yml` would have given `x` the `ready` category (which *does* have a default). The error misleads the author about why it failed.
- **Recommendation:** This is a narrow, unlikely mixed-form combination, so the pragmatic fix is to improve the message (note the category is a pre-resolution sentinel) or defer the bare-owner-agent migration until after `resolve_against` has filled real categories. Low priority.
- **Effort:** S

---

## Notes on areas that held up

- **`placeholders.rs`** — single-pass, non-recursive substitution with explicit handling of unterminated `{{`, empty/whitespace bodies, and internal whitespace. No infinite-loop path exists (a substituted value containing `{{x}}` is emitted verbatim, test at placeholders.rs:217). Solid.
- **`HeartbeatConfig` parsing** (model.rs:187–225) — the `.last().unwrap()` is safe (empty string rejected first), overflow is `checked_mul`, zero/unitless/negative all rejected. Well guarded.
- **`statuses.rs` / `workspace_names.rs`** — thorough validation and round-trip tests; presets are verified to be valid agent ids.
- **serde defaults** — `#[serde(default)]` usage is disciplined; unknown enum variants (merge strategy, status category) correctly hard-fail rather than defaulting (tests at model.rs:2277 and workflow.rs:1707).
