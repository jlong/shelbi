# Adversarial review: state persistence (shelbi-state core)

Reviewed:

- crates/shelbi-state/src/lib.rs (2959 lines)
- crates/shelbi-state/src/migrate.rs (858 lines)
- crates/shelbi-state/src/workflows.rs (846 lines)
- crates/shelbi-state/src/resolve.rs (610 lines)
- crates/shelbi-state/src/project_paths.rs (495 lines)

Supporting code read for context (findings are not reported against these):
`crates/shelbi-core/src/model.rs`, `crates/shelbi-cli/src/commands/task.rs`,
`crates/shelbi-cli/src/commands/project.rs`, `crates/shelbi-state/src/root.rs`.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | `migrate-to-in-repo` deletes the only project YAML `load_project` can read — migrated projects can't be opened | high | certain | bug |
| F2 | Crash during migration's shared-YAML write + re-run deletes the last good config copy | high | certain | failure-scenario |
| F3 | No file locking anywhere: read-modify-write on `state.json` loses concurrent updates (heartbeat can silently re-arm Zen Mode the user just turned off) | high | certain | failure-scenario |
| F4 | Interrupted `move_dir` copy-fallback leaves a partial destination that re-planning silently accepts as complete | medium | certain | failure-scenario |
| F5 | Task ids and workflow names are only validated at `task add` — read/move/delete paths allow `../` path traversal | medium | certain | hardening |
| F6 | Older binaries silently destroy fields written by newer binaries in `state.json`; non-string task fields from a newer binary make the whole task vanish | medium | certain | assumption |
| F7 | `move_task` is non-atomic across files and races itself: duplicate priorities, nondeterministic board order | medium | certain | bug |
| F8 | Task frontmatter `id` ≠ filename forks the task into two files on the next move | medium | certain | bug |
| F9 | `atomic_write` temp name is `pid`-keyed only: two threads in one process corrupt each other; `with_extension` collides across same-stem files; stale `.tmp.*` never cleaned | medium | likely | bug |
| F10 | Walk-up resolution trusts `.shelbi/project.yaml` in any ancestor — a cloned repo can redirect commands to another project's state, or brick resolution for a whole subtree | medium | likely | hardening |
| F11 | One malformed workflow YAML (or missing `statuses.yml`) fails `list_workflows` entirely — no per-file isolation, unlike tasks | low | certain | failure-scenario |
| F12 | Duplicate workflow `name:`s undetected; `load_workflow` keys by filename while `list_workflows` keys by declared name | low | certain | bug |
| F13 | `read_state`/`load_project_statuses` use `exists()`-then-read (TOCTOU) and `exists()` conflates "absent" with "unreadable" | low | certain | best-practice |
| F14 | `project_roots` silently skips unreadable/corrupt project YAMLs — a project quietly disappears from cwd resolution | low | certain | hardening |
| F15 | `list_ready` re-reads and re-parses the whole tasks directory twice per call | low | certain | simplification |
| F16 | Dead shim `_projects_dir_still_reachable` exists only to silence an unused import | low | certain | simplification |

---

## F1: `migrate-to-in-repo` deletes the only project YAML `load_project` can read

- **Where:** crates/shelbi-state/src/migrate.rs:249, crates/shelbi-state/src/lib.rs:383
- **Category:** bug
- **Severity / Confidence:** high / certain
- **Evidence:** The migration's final action removes the global YAML:

  ```rust
  MigrationAction::DeleteGlobalYaml { path } => {
      match fs::remove_file(path) { ... }
  ```

  But the loader every runtime caller uses reads only that file (lib.rs:383-387):

  ```rust
  pub fn load_project(project: &str) -> Result<Project> {
      let p = projects_dir()?.join(format!("{project}.yaml"));
      let text = fs::read_to_string(&p)?;
  ```

  There is no mode-aware loader in the tree: `Project::from_split_yaml_str` is called only from migrate.rs itself and from tests (`grep from_split_yaml_str` → migrate.rs:350, migrate.rs:842, core tests). migrate.rs's own test admits this at line 838: *"Reparse via the split reader — mirrors what a future loader would do once it's mode-aware."* Meanwhile the command is live: `shelbi project migrate-to-in-repo` (crates/shelbi-cli/src/commands/project.rs:75-83) calls `apply_migration_plan` unconditionally (no `--force`, no warning).
- **Failure scenario:** User runs `shelbi project migrate-to-in-repo`. It succeeds and prints ✓. Every subsequent `shelbi_state::load_project("name")` — the TUI review pane (shelbi-tui/src/review.rs:176), sidebar refresh (shelbi-tui/src/app.rs:640), kanban (shelbi-tui/src/kanban.rs:648) — fails with file-not-found. The project is unopenable until the user hand-restores `~/.shelbi/projects/<name>.yaml`.
- **Recommendation:** Either make `load_project` mode-aware (fall back to `local.yaml` + `<repo>/.shelbi/project.yaml` via `from_split_yaml_str`) before shipping the command, or gate the command/`DeleteGlobalYaml` action behind a feature flag until the loader lands. At minimum, keep the global YAML (rename to `.yaml.migrated`) instead of deleting it.
- **Effort:** L

## F2: Crash during shared-YAML write + re-run deletes the last good config copy

- **Where:** crates/shelbi-state/src/migrate.rs:261-266, crates/shelbi-state/src/migrate.rs:181-186
- **Category:** failure-scenario
- **Severity / Confidence:** high / certain
- **Evidence:** The module doc (migrate.rs:11) promises *"atomically-ish (write-then-swap for YAMLs)"* — the code does neither:

  ```rust
  fn write_yaml_file(path: &Path, body: &str) -> Result<()> {
      if let Some(parent) = path.parent() {
          fs::create_dir_all(parent).map_err(Error::Io)?;
      }
      fs::write(path, body).map_err(Error::Io)
  }
  ```

  And the planner treats any existing file as complete (migrate.rs:181-186):

  ```rust
  if !shared_yaml_path.is_file() {
      actions.push(MigrationAction::WriteSharedYaml { ... });
  }
  ```

  The crate already has `atomic_write` (lib.rs:1498) — migrate.rs just doesn't use it.
- **Failure scenario:** (1) `apply_migration_plan` starts; `fs::write` of `<repo>/.shelbi/project.yaml` is interrupted (crash, power loss, full disk) leaving a truncated file. (2) User re-runs the migration — the intended self-heal path. (3) `load_project_for_migration` still finds the global YAML (delete never ran) and loads fine; the planner sees `shared_yaml_path.is_file() == true` and *skips rewriting the truncated file*; the plan still includes `DeleteGlobalYaml`, which runs and deletes the only valid copy. (4) A third run now takes the `local.yaml` branch, reads the corrupt shared half, and `from_split_yaml_str` fails. The project config is unrecoverable except from git history (and `local.yaml`'s contents were never committed anywhere).
- **Recommendation:** Route `write_yaml_file` through `crate::atomic_write`, and/or make the planner validate that an existing `project.yaml` actually parses before treating the write step as done (re-queue the write when it doesn't).
- **Effort:** S

## F3: No file locking: read-modify-write on `state.json` loses concurrent updates

- **Where:** crates/shelbi-state/src/lib.rs:885-889, 959-966, 972-976, 991-1008, 642-653
- **Category:** failure-scenario
- **Severity / Confidence:** high / certain
- **Evidence:** Every mutator is an unlocked read-modify-write of the whole file:

  ```rust
  pub fn zen_heartbeat(project: &str) -> Result<()> {
      let mut state = read_state(project)?;
      state.zen_last_crashed_at = Some(Utc::now());
      write_state(project, &state)
  }
  ```

  ```rust
  pub fn set_zen_mode(project: &str, target: ZenModeState, source: &str) -> Result<ZenModeState> {
      let mut state = read_state(project)?;
      let prev = state.zen_mode;
      state.zen_mode = target;
      write_state(project, &state)?;
  ```

  `atomic_write` makes each individual write non-torn, but nothing serializes the read→write window across the known concurrent writers (orchestrator heartbeat loop, CLI `shelbi zen`, TUI Alt+Z handler, palette — the doc comment on `set_zen_mode` itself lists three separate caller processes). Same pattern for `set_workspace_filter`, `set_kanban_column_override`, `toggle_zen_mode` (which even does two reads: lib.rs:1016 then inside `set_zen_mode`), and the global `toggle_sidebar_machine_collapsed` / `mark_zen_intro_seen`.
- **Failure scenario:** Zen Mode is On. The orchestrator's heartbeat tick calls `read_state` (sees `zen_mode: on`). The user runs `shelbi zen off`, which writes `zen_mode: off`. The heartbeat then writes its stale snapshot back with `zen_mode: on` plus a fresh timestamp. The user's *safety-critical* opt-out of auto-merge is silently reverted; the events log even shows `on -> off` while disk says `on`. The reverse direction silently clears `workspace_filter` or a crash timestamp the same way.
- **Recommendation:** Take an advisory lock (`flock` via e.g. `fs2`/`fd-lock` on a sibling `state.json.lock`) around the read-modify-write in one shared helper (`update_state(project, impl FnOnce(&mut State))`), and route all mutators through it. Same for `GlobalState`.
- **Effort:** M

## F4: Interrupted `move_dir` copy-fallback leaves a partial destination that re-planning accepts as complete

- **Where:** crates/shelbi-state/src/migrate.rs:272-284, crates/shelbi-state/src/migrate.rs:193-199
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  match fs::rename(src, dst) {
      Ok(()) => Ok(()),
      Err(_) => {
          copy_dir_recursive(src, dst)?;
          fs::remove_dir_all(src).map_err(Error::Io)?;
          Ok(())
      }
  }
  ```

  with the planner's skip condition:

  ```rust
  if src.is_dir() && !dst.exists() {
      actions.push(MigrationAction::MoveConfigDir { src, dst });
  }
  ```

  A crash mid-`copy_dir_recursive` (cross-device case, EXDEV) leaves `dst` existing but incomplete. The re-run — explicitly advertised as the recovery path in the module docs — sees `dst.exists()` and plans no move. The half-copied `<repo>/.shelbi/agents/` becomes the canonical config; the complete source stays stranded in the state dir where nothing reads it. Note also the `Err(_)` catch-all: *any* rename failure (permissions, not just EXDEV) silently enters the copy path, discarding the original error kind.
- **Failure scenario:** Migration crashes while copying `agents/` across filesystems after copying only `agents/orchestrator/`. Re-run reports success. Workflows referencing `agent: developer` now fail `validate_agent_references` ("unknown agent `developer`") even though the user's customized developer instructions still exist — invisibly — under `~/.shelbi/projects/<name>/agents/developer/`.
- **Recommendation:** Copy to a temp sibling (`dst.with-tmp-suffix`) and rename into place, or have the planner detect the "both `src` and `dst` exist" state and error with instructions rather than silently skipping.
- **Effort:** M

## F5: Task ids / workflow names only validated at `task add` — read/move/delete paths allow path traversal

- **Where:** crates/shelbi-state/src/lib.rs:1097-1099, 1128-1134; crates/shelbi-state/src/workflows.rs:72-74
- **Category:** hardening
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  pub fn task_path(project: &str, id: &str) -> Result<PathBuf> {
      Ok(tasks_dir(project)?.join(format!("{id}.md")))
  }
  ```

  ```rust
  pub fn delete_task(project: &str, id: &str) -> Result<()> {
      let path = task_path(project, id)?;
      if path.exists() {
          fs::remove_file(&path)?;
      }
  ```

  `shelbi_core::validate_task_id` (which would reject `/`, `..`) is called in exactly one place: the `--id` branch of `task add` (shelbi-cli/src/commands/task.rs:190). The CLI's show/edit/move/delete handlers pass the raw argument straight to `load_task`/`move_task`/`delete_task` (task.rs:343, 441, 754-756). The same holds for `workflow_path(project, name)` where `name` comes from task frontmatter (`workflow_or_default()`), and for `agent_path`/`project_dir` with the project name.
- **Failure scenario:** `shelbi task delete '../HANDOFF'` resolves to `~/.shelbi/projects/<p>/tasks/../HANDOFF.md` and deletes the project's handoff file; `'../../other-proj/tasks/foo'` deletes another project's task. The primary caller of this CLI is the orchestrator — an LLM agent whose inputs include repo content, so "the id came from a trusted human" does not hold; a prompt-injected id crosses the project boundary silently.
- **Recommendation:** Call `validate_task_id` / `validate_workflow_name` inside `task_path` / `workflow_path` / `agent_path` (the storage layer chokepoints), not just at creation.
- **Effort:** S

## F6: Serde round-trips drop unknown fields; non-string task fields from a newer binary make tasks vanish

- **Where:** crates/shelbi-state/src/lib.rs:465-503 (`State`), 597-616 (`GlobalState`), 1194-1244 (`list_tasks` skip)
- **Category:** assumption
- **Severity / Confidence:** medium / certain
- **Evidence:** `State` and `GlobalState` are plain derive structs with no `#[serde(flatten)]` catch-all:

  ```rust
  #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
  pub struct State {
      #[serde(default, deserialize_with = "ZenModeState::deserialize_lenient")]
      pub zen_mode: ZenModeState,
      ...
  }
  ```

  serde ignores unknown JSON keys on read, and `write_state` serializes only the known struct — so any read-modify-write (`set_workspace_filter`, `zen_heartbeat`, …) performed by an *older* binary permanently deletes every field a newer binary added. The Zen tri-state widening shows exactly this old-binary/new-state mixing is an anticipated deployment mode (`deserialize_lenient`, lib.rs:548). `Task` is better — `#[serde(flatten)] params: BTreeMap<String, String>` (shelbi-core/src/model.rs:872) round-trips unknown *string* fields — but a newer binary writing a non-string field (number, bool, mapping) makes flatten-deserialization fail for the whole file, and `list_tasks` (lib.rs:1222-1236) then skips the task with only a deduped stderr warning: the card silently disappears from the board on the old binary, and any `renumber_column` run there rewrites priorities as if it doesn't exist.
- **Failure scenario:** v(N+1) adds `state.json::review_rounds: 3` and task field `retries: 2`. The user still has a v(N) daemon running (mixed versions during upgrade — the exact situation `deserialize_lenient` exists for). The v(N) heartbeat rewrites `state.json` without `review_rounds`; the v(N) sidebar drops the `retries: 2` task from the board and renumbers around it.
- **Recommendation:** Add a `#[serde(flatten)] extra: BTreeMap<String, serde_json::Value>` to `State`/`GlobalState` (and consider `serde_yaml::Value` for `Task::params`) so unknown fields survive rewrites; document the compat contract.
- **Effort:** M

## F7: `move_task` is non-atomic across files and races itself

- **Where:** crates/shelbi-state/src/lib.rs:1406-1424, 1447-1460, 1239-1242
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  let new_priority = list_column(project, new_column)?.len() as u32;
  task.column = new_column;
  task.priority = new_priority;
  ...
  save_task(project, &task, &body)?;
  renumber_column(project, old_column)?;
  ```

  Two invariant breaks: (a) two concurrent `move_task` calls into the same column both compute `len()` before either saves → duplicate priorities. Duplicates are never repaired in the *destination* column (renumber only runs on the *old* column), and `list_tasks`'s `sort_by_key((col_idx, priority))` (lib.rs:1239) is stable on the underlying `fs::read_dir` order, which is filesystem-dependent → board order flaps between refreshes. (b) A crash between `save_task` and `renumber_column` leaves the old column with a gap forever (nothing re-runs renumber until the next move out of that column). `write_column_order` (lib.rs:1447) also persists each file from an earlier full-directory snapshot, so it can clobber a concurrent edit (e.g. the daemon setting `assigned_to`) with stale frontmatter — the same lost-update class as F3 but across N task files. Relatedly, `save_task` has no create-exclusive mode, so the CLI's `generate_unique_id` exists-check→save (task.rs:768-779) is a TOCTOU where two concurrent `task add` with the same title silently merge into one file.
- **Failure scenario:** Orchestrator dispatch moves task A to `in_progress` while the user drags task B there in the TUI. Both land with `priority: 2`. The kanban now shows A above B on one refresh and B above A on the next; `set_task_priority` math (position-based remove/insert) partially compensates but persists whichever order `read_dir` happened to return.
- **Recommendation:** Same per-project lock as F3 around the whole move (list → save → renumber); renumber the destination column too, or derive display order with a deterministic tiebreak (`priority, id`).
- **Effort:** M

## F8: Task frontmatter `id` ≠ filename forks the task into two files

- **Where:** crates/shelbi-state/src/lib.rs:1106-1110, 1112-1116, 1194-1244
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:** Reads address by filename, writes address by frontmatter id:

  ```rust
  pub fn save_task(project: &str, task: &Task, body_md: &str) -> Result<()> {
      ...
      let path = task_path(project, &task.id)?;   // ← task.id from frontmatter
  ```

  `load_task(project, id)` reads `<id>.md` but never checks the parsed `task.id` matches. `list_tasks` keys cards purely on frontmatter, ignoring filenames.
- **Failure scenario:** User hand-edits `fix-login.md` (task files are explicitly a hand-editable surface — the parse-warn cache exists because of it) and changes `id: fix-login` to `id: fix-auth`. `move_task("p", "fix-login", InProgress)` loads `fix-login.md`, then `save_task` writes the updated card to **`fix-auth.md`**, leaving `fix-login.md` behind in the old column. The board now shows two cards with id `fix-auth` in different columns; `task_columns` (a `HashMap<String, Column>`) collapses them nondeterministically, so `is_blocked` answers depend on `read_dir` order.
- **Recommendation:** In `load_task`/`parse_task_file` callers, reject (or warn and heal) when parsed `task.id` ≠ file stem; alternatively make the filename authoritative and overwrite the frontmatter id on load.
- **Effort:** S

## F9: `atomic_write` temp naming — same-process collisions, cross-stem collisions, stale temp files

- **Where:** crates/shelbi-state/src/lib.rs:1498-1514
- **Category:** bug
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
  {
      let mut f = fs::File::create(&tmp)?;
      f.write_all(bytes)?;
      f.sync_all()?;
  }
  fs::rename(&tmp, path)?;
  ```

  Three issues. (a) The temp name is keyed only by pid: two threads of the same process writing the same target (TUI event handler + its poller thread both calling `write_state`, or `write_column_order` invoked from two threads) share one temp path; the second `File::create` truncates the first thread's in-flight file and the rename can install interleaved/truncated bytes — defeating the function's entire purpose. (b) `with_extension` *replaces* the final extension, so distinct same-stem files in one directory (`state.json` / hypothetical `state.yaml`, `statuses.yml` / `statuses.yaml`) map to the same temp path — latent today but a trap. (c) A crash between create and rename leaves `*.tmp.<pid>` files forever (they're invisible to the `.md`/`.yaml` extension filters, so nothing ever reports or reaps them). No directory fsync after rename, so the swap itself isn't power-loss durable (acceptable for this data class, but worth a comment).
- **Failure scenario:** Kanban TUI: input thread persists a column-override toggle while the refresh thread persists the workspace filter (both `write_state`, same pid). Thread B's `File::create` truncates thread A's partially-written temp; A's `rename` then installs B's half-written JSON → `read_state` fails with a parse error and the TUI falls back to `State::default()` semantics for the project.
- **Recommendation:** Include a per-call unique component (thread id + counter, or use `tempfile::NamedTempFile::new_in(dir)` + `persist`), and append the suffix (`format!("{}.tmp.{}", file_name, ...)`) instead of `with_extension`.
- **Effort:** S

## F10: Walk-up resolution trusts `.shelbi/project.yaml` from any ancestor / any cloned repo

- **Where:** crates/shelbi-state/src/resolve.rs:106-119, 153-174
- **Category:** hardening
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  while let Some(dir) = cur {
      let candidate = dir.join(".shelbi").join("project.yaml");
      if candidate.is_file() {
          let text = fs::read_to_string(&candidate)?;
          let header: InRepoProjectHeader = serde_yaml::from_str(&text).map_err(...)?;
          return Ok(Some(InRepoHit { name: header.name, ... }));
      }
      cur = dir.parent();
  }
  ```

  The `name:` in a repo-committed file — i.e. content controlled by whoever authored the repo — is resolved directly against the user's local registry with no check that the repo path corresponds to that project's declared `repo:`/`work_dir`. Git grew `safe.directory` for exactly this class of problem. Two concrete consequences: (1) any cloned repo containing `.shelbi/project.yaml` with `name: <existing-project>` makes every shelbi command run from inside it silently operate on that other project's tasks/state (`local.yaml` for the name exists, so the check at resolve.rs:108-115 passes). (2) A *corrupt* `project.yaml` anywhere up the ancestor chain hard-fails resolution (`InRepoProjectParse` propagates from resolve.rs:160-165) for every cwd beneath it, even when the global registry would have matched — one bad file in `~/scratch/.shelbi/` breaks shelbi in every subdirectory of `~/scratch`.
- **Failure scenario:** User clones a third-party repo that ships `.shelbi/project.yaml` naming the user's own `shelbi` project, cds in to poke around, and runs `shelbi task add …` — the task lands on their real project board; a dispatched workspace would then operate with that repo's content in the loop.
- **Recommendation:** After the walk-up hit, cross-check the discovered repo root against the registered project's `repo:`/`work_dir` (canonical-path comparison) before accepting the name; downgrade a corrupt ancestor YAML to a warning + fall through to the global registry.
- **Effort:** M

## F11: `list_workflows` is all-or-nothing — one bad file takes down every workflow

- **Where:** crates/shelbi-state/src/workflows.rs:158-195
- **Category:** failure-scenario
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  for raw in raw_files {
      let inline = Workflow::inline_identity_fields(&raw.text).map_err(|e| annotate(&raw.path, e))?;
      if !inline.is_empty() {
          return Err(mixed_form_error(&raw.path, &inline));
      }
      ...
  ```

  Every `?`/`return Err` aborts the whole list. This is documented as intentional (workflows.rs:154-157, "rather than being silently skipped"), so this is a design note, not an undocumented bug — but it answers the seed question the unfriendly way: the crate isolates a corrupt *task* file (skip + deduped warning, lib.rs:1222-1236) yet a single typo'd *workflow* file, or a deleted `statuses.yml`, makes `list_workflows` error for callers that only wanted the other, valid workflows. The user edits `research.yaml`, fat-fingers YAML, and their default board is gone until they fix it.
- **Failure scenario:** As above — any polling caller of `list_workflows` goes from N workflows to a hard error on one bad file, with no partial rendering.
- **Recommendation:** Return `(Vec<Workflow>, Vec<(PathBuf, Error)>)` (or skip-with-warning through the same dedup cache tasks use) so valid workflows keep loading while broken ones are surfaced loudly.
- **Effort:** M

## F12: Duplicate workflow names undetected; filename vs declared-name split-brain

- **Where:** crates/shelbi-state/src/workflows.rs:113-135, 158-195
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:** `load_workflow(project, name)` resolves `workflows/<name>.yaml` by *filename*; `list_workflows` sorts and exposes workflows by their *declared* `name:` field, and nothing checks the two agree (the doc comment on `load_workflow` even disclaims it: "The file's basename is *not* substituted for the workflow's declared `name:`"). Nor does `list_workflows` reject two files declaring the same `name:`.
- **Failure scenario:** `workflows/review.yaml` declares `name: design-review`. The picker (fed by `list_workflows`) shows `design-review`; a task saved with `workflow: design-review` then fails `load_workflow("p", "design-review")` with a raw `Io(NotFound)` (workflows.rs:115) — no hint that the file exists under another basename. Conversely two files both declaring `name: default` yield two indistinguishable entries.
- **Recommendation:** In `list_workflows`, error (or warn) when `declared name != file stem` and on duplicate names — one cheap loop after loading.
- **Effort:** S

## F13: `exists()`-then-read pattern conflates "absent" with "unreadable"

- **Where:** crates/shelbi-state/src/lib.rs:933-941 (`read_state`), 669-677 (`read_global_state`), 121-128 (`load_shelbi_config`); crates/shelbi-state/src/workflows.rs:86-93
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  if !path.exists() {
      return Ok(State::default());
  }
  let text = fs::read_to_string(&path)?;
  ```

  `Path::exists()` returns `false` for any stat error, including `EACCES`/`ELOOP` — not just `ENOENT`. A `state.json` that exists but is momentarily unstat-able reads as "missing", returns defaults, and the *next mutator write* (`set_zen_mode` etc., which never fails on this path once the dir is writable again) replaces the real state with defaults-plus-one-change. Same shape in `load_project_statuses`, where a vanished-mid-race `statuses.yml` silently resolves workflows against `default_project_statuses()` instead of erroring.
- **Failure scenario:** Transient permission problem on `~/.shelbi/projects/<p>/` (backup tool, chmod mistake) during a Zen toggle → toggle succeeds but every other `State` field (crash timestamp, filter, overrides, divergence notices) is reset to defaults.
- **Recommendation:** Read unconditionally and match `ErrorKind::NotFound` for the default branch (the crate already does exactly this correctly in `render_workspace_settings`, lib.rs:286-292).
- **Effort:** S

## F14: `project_roots` silently drops unreadable/corrupt project YAMLs

- **Where:** crates/shelbi-state/src/resolve.rs:62-69
- **Category:** hardening
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let text = match fs::read_to_string(&path) {
      Ok(t) => t,
      Err(_) => continue,
  };
  let project: Project = match serde_yaml::from_str(&text) {
      Ok(p) => p,
      Err(_) => continue,
  };
  ```

  Documented as intentional (one bad YAML shouldn't break the others' resolution — fair), but the skip is *completely* silent: no `tracing::warn!`, no counterpart to the deduped stderr warnings task files get. `cleanup_legacy_markers` (the `shelbi reload` surface built on `project_roots`, resolve.rs:218-236) also won't report it, because the project never makes it into the list at all.
- **Failure scenario:** User hand-edits `~/.shelbi/projects/myapp.yaml` and breaks the YAML. From inside the repo, `shelbi` now reports "no project specified" with zero indication that a registration exists but is corrupt; the user debugs the wrong thing.
- **Recommendation:** Emit a deduped `tracing::warn!` naming the file and error (mirroring `warn_legacy_workers_key`'s once-per-process approach), or surface skipped files in `MarkerCleanup`.
- **Effort:** S

## F15: `list_ready` parses the entire tasks directory twice

- **Where:** crates/shelbi-state/src/lib.rs:1265-1271
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  let columns = task_columns(project)?;            // ← list_tasks (full dir read+parse)
  Ok(list_column(project, Column::Todo)?           // ← list_tasks again
      .into_iter()
      .filter(|tf| !tf.task.is_blocked(&columns))
      .collect())
  ```

  Two full read+YAML-parse passes over every task file per call, and the two snapshots can disagree if a move lands between them (a task can appear in `Todo` while `columns` reflects its pre-move column, briefly mis-answering `is_blocked`). `move_task` has the same double-scan shape (`list_column` for priority, then `renumber_column` re-lists).
- **Recommendation:** `let all = list_tasks(project)?;` once; derive both the column map and the filtered Todo list from it.
- **Effort:** S

## F16: Dead shim exists only to silence an unused import

- **Where:** crates/shelbi-state/src/project_paths.rs:162-169
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  /// Silence unused-import lint for [`projects_dir`] — ...
  #[allow(dead_code)]
  fn _projects_dir_still_reachable() -> Result<PathBuf> {
      projects_dir()
  }
  ```

  The straightforward fix for an unused import is removing it from the `use` on line 25, not adding a dead function to keep it "reachable". (`_paths_dyn_probe` below it at least asserts a real property — object safety — though a `const _: () = { ... }` or a test would state that intent more idiomatically.)
- **Recommendation:** Delete the function and drop `projects_dir` from the import list.
- **Effort:** S

---

### Seed-question summary

- **Atomic writes:** yes for `state.json`/task files/`statuses.yml` via `atomic_write` (modulo F9's temp-name flaws); **no** for the migration's YAMLs (F2) and `.gitignore` append (plain `fs::write`).
- **Concurrent writers:** no locking anywhere; lost updates are real and user-visible (F3, F7).
- **Corrupt task file:** properly isolated — skipped with a deduped warning (lib.rs:1222-1236, good). Corrupt *workflow* file: not isolated (F11). Corrupt *project* YAML: silently invisible (F14) or resolution-fatal (F10).
- **Migration idempotency:** idempotent on the happy path; the crash-recovery re-run is where it destroys data (F2, F4). Old binary vs migrated state: fatal — the loader isn't mode-aware yet (F1).
- **Unknown-field round-trips:** dropped for `State`/`GlobalState`; task `params` flatten preserves string fields but non-string fields brick the whole file on older binaries (F6).
- **Path resolution:** symlinks handled well (canonicalize on both sides, resolve.rs:154/183); `~` expansion consistent; spaces fine (no shell involved); non-UTF-8 paths are unsupported by design (`repo: String`, `to_str()` filters) — acceptable, undocumented.
- **Slug collisions:** handled at generation (`-2`, `-3` suffixes in shelbi-cli `generate_unique_id`) but with an exists→save TOCTOU and no create-exclusive write in `save_task` (noted in F7).
