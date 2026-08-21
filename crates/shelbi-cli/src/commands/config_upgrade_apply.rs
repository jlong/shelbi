//! Auto-heal **write-back** for the version-agnostic config validate-and-upgrade
//! pass (slice 2).
//!
//! Slice 1 ([`super::config_upgrade`]) detects deprecations and classifies each
//! as [`Classification::AutoHeal`] (a deterministic, non-lossy rewrite exists)
//! or [`Classification::NeedsJudgment`] (ambiguous — handed to the orchestrator
//! by a later slice). This module applies every `AutoHeal` finding to disk. It
//! runs on hub start and `shelbi reload`, immediately after detection, and
//! discloses each applied change on `~/.shelbi/events.log` as a
//! `config-upgrade` line so the rewrite is visible and auditable.
//!
//! ## Design contract
//!
//! * **Only `AutoHeal` is applied.** `NeedsJudgment` findings are never
//!   rewritten here (slice 3 routes them to the orchestrator).
//! * **Non-lossy.** A healer only touches the specific deprecated construct.
//!   For the project-registration YAML the rewrite is a structural
//!   `serde_yaml::Value` mutation that preserves every *other* key (including
//!   unknown / needs-judgment keys); Shelbi already round-trips those files
//!   through `save_project`, so the normalization is consistent with existing
//!   behavior. For the comment-bearing scaffolded surfaces (`statuses.yaml`,
//!   workflow YAML) the rewrite is a surgical single-line edit so authored
//!   comments survive.
//! * **Best-effort, safe-skip.** A healer that can't apply a finding
//!   deterministically (an unexpected shape, a rename that would clobber an
//!   existing file, a keymap conflict) leaves it untouched — the finding stays
//!   *reported* for the next run rather than risking a bad write. Nothing is
//!   ever lost by a skip.
//! * **Idempotent.** Every healer produces a form the slice-1 sniffers consider
//!   current, so a second pass detects nothing and writes nothing. The caller
//!   ([`super::config_upgrade::run_and_emit`]) re-detects after applying to make
//!   the residual report reflect the post-heal state.
//! * **Reconciled with existing self-heals.** Hook path anchoring/quoting reuse
//!   the orchestrator's deploy-time transforms verbatim; the workspace-settings
//!   template refresh delegates to the state layer's existing self-heal (which
//!   honors a custom-template override); the legacy `zen_toggle` migration
//!   drives the keymap loader's existing one-shot. So a form is never healed
//!   twice or fought over.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde_yaml::{Mapping, Value};
use shelbi_core::StatusCategory;

use super::config_surfaces::{live_entries, InventoryEntry, SurfaceFormat};
use super::config_upgrade::{
    get, is_inert, legacy_status_id, scope_label, Classification, UpgradeFinding, UpgradeReport,
    PR_TEMPLATE_MISSING, REMOVED_PROJECT_KEYS,
};

/// One auto-heal write-back that landed on disk this run. Returned to the
/// caller for summary printing and recorded on `events.log` by [`disclose`].
#[derive(Debug, Clone)]
pub struct AppliedChange {
    /// `global` or `project:<name>` — the finding's scope (for the events.log
    /// `project=` label via [`scope_label`]).
    pub scope: String,
    /// The surface's stable inventory id.
    pub logical_id: String,
    /// The deprecation code that was healed (e.g. `WORKSPACE_RUNNER_REMOVED`).
    pub code: String,
    /// The file the rewrite landed in.
    pub file: PathBuf,
    /// Compact `old->new` description of the change (events.log-token safe).
    pub detail: String,
}

/// Apply every [`Classification::AutoHeal`] finding in `report` to disk and
/// return the changes that were written. `NeedsJudgment` findings are ignored.
///
/// Best-effort: enumeration or per-file IO errors are logged and skipped so a
/// single broken surface can't abort the whole pass (and never blocks the
/// caller's primary job of serving / reloading).
pub fn apply_auto_heal(projects: &[String], report: &UpgradeReport) -> Vec<AppliedChange> {
    let mut applied = Vec::new();
    if report.auto_heal_count() == 0 {
        return applied;
    }
    // Re-enumerate the live surfaces so each finding is matched to its entry
    // (for `format` / `lifecycle_owned` / the canonical path a sibling-rename
    // targets). The report alone doesn't carry those.
    let entries = match live_entries(projects) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("shelbi config-upgrade: could not enumerate surfaces to heal: {e}");
            return applied;
        }
    };
    let by_id: HashMap<&str, &InventoryEntry> =
        entries.iter().map(|e| (e.logical_id.as_str(), e)).collect();

    // Group the auto-heal findings by surface so each file is read/written
    // once. BTreeMap keeps the healing order stable (and thus the events.log
    // disclosure order deterministic).
    let mut groups: BTreeMap<&str, Vec<&UpgradeFinding>> = BTreeMap::new();
    for f in &report.findings {
        if f.classification == Classification::AutoHeal {
            groups.entry(f.logical_id.as_str()).or_default().push(f);
        }
    }

    for (logical_id, findings) in groups {
        let Some(entry) = by_id.get(logical_id) else {
            continue;
        };
        heal_surface(entry, &findings, &mut applied);
    }
    applied
}

/// Needs-judgment codes that have a deterministic resolution the user can
/// approve for a targeted apply. Everything else routes to a human editing the
/// file directly (a genuine ambiguity — a typo vs. data, one agent vs. another),
/// so `apply_single_finding` refuses them with a pointer to the proposed fix.
///
/// `ZEN_DANGER_PATHS_CONFLICT` is the one entry: once the user accepts the
/// safe-superset resolution (keep the built-in defaults plus every listed path),
/// the rewrite is mechanical — see the conflict branch in `rewrite_registration`.
const RESOLVABLE_NEEDS_JUDGMENT: &[&str] = &["ZEN_DANGER_PATHS_CONFLICT"];

/// Apply exactly ONE finding, identified by its stable [`UpgradeFinding::id`],
/// reusing the same atomic write-back machinery as the bulk auto-heal pass. This
/// is the targeted path the orchestrator invokes once the user approves a
/// needs-judgment fix (`shelbi config upgrade --apply-finding <id>`).
///
/// The finding is re-detected fresh (never trusted from a possibly-stale channel
/// file) and matched by id, then handed to [`apply_auto_heal`] as a
/// single-finding report — so only that finding's code, on that one surface, is
/// touched; every other finding is left untouched. A needs-judgment finding is
/// applied only when its code is in [`RESOLVABLE_NEEDS_JUDGMENT`]; approving it
/// promotes it to `AutoHeal` for this one write so the existing healer resolves
/// it. Returns the change(s) written (empty if a healer safe-skipped).
pub fn apply_single_finding(projects: &[String], id: &str) -> anyhow::Result<Vec<AppliedChange>> {
    let report = super::config_upgrade::detect(projects)?;
    let Some(finding) = report.findings.iter().find(|f| f.id == id) else {
        anyhow::bail!(
            "no config-upgrade finding with id `{id}` (it may already be resolved — \
             rerun `shelbi config upgrade --needs-judgment` for the current channel)"
        );
    };

    if finding.classification == Classification::NeedsJudgment
        && !RESOLVABLE_NEEDS_JUDGMENT.contains(&finding.code.as_str())
    {
        anyhow::bail!(
            "finding `{id}` [{}] has no mechanical resolution — apply its fix by editing \
             {} directly: {}",
            finding.code,
            finding.file.display(),
            finding.proposed_fix
        );
    }

    // A single-finding report, promoted to AutoHeal so the shared write-back
    // applies it. Cloning keeps every other field (surface, location, code) so
    // the healer targets the exact construct this finding describes.
    let mut approved = finding.clone();
    approved.classification = Classification::AutoHeal;
    let single = UpgradeReport {
        schema_version: report.schema_version,
        shelbi_version: report.shelbi_version.clone(),
        findings: vec![approved],
    };

    let applied = apply_auto_heal(projects, &single);
    if applied.is_empty() {
        anyhow::bail!(
            "could not apply finding `{id}` [{}] to {} — its on-disk form wasn't in the \
             expected shape; apply the fix by hand: {}",
            finding.code,
            finding.file.display(),
            finding.proposed_fix
        );
    }
    Ok(applied)
}

/// Dispatch a single surface's auto-heal findings to the right healer, mirroring
/// the detection dispatch in [`super::config_upgrade`].
fn heal_surface(entry: &InventoryEntry, findings: &[&UpgradeFinding], applied: &mut Vec<AppliedChange>) {
    let codes: BTreeSet<&str> = findings.iter().map(|f| f.code.as_str()).collect();

    // File-shape (sibling-rename) findings carry the *legacy* path in `file`;
    // handle them first and independently of the content healers.
    for f in findings {
        if matches!(f.code.as_str(), "STATUSES_YML_EXTENSION" | "WORKER_SETTINGS_TEMPLATE_RENAME") {
            heal_rename(entry, f, applied);
        }
    }

    // Materialize a missing lifecycle-owned shipped default (the per-project
    // `pr-template.md`). Independent of the content healers below.
    if codes.contains(PR_TEMPLATE_MISSING) {
        heal_missing_pr_template(entry, applied);
    }

    let id = entry.logical_id.as_str();
    if id.ends_with(".registration")
        || id.ends_with(".registration.shared")
        || id.ends_with(".registration.local")
    {
        heal_registration(entry, &codes, applied);
    } else if id.ends_with(".statuses") {
        heal_statuses(entry, applied);
    } else if id.contains(".workflow.") {
        heal_workflow(entry, &codes, applied);
    } else if id == "global.preferences" {
        heal_keymap(entry, &codes, applied);
    } else if matches!(entry.format, SurfaceFormat::Json)
        && (id.ends_with(".settings") || id.ends_with("workspace-settings-template"))
    {
        heal_settings(entry, &codes, applied);
    }
}

// ---------------------------------------------------------------------------
// Project registration YAML — structural mutation

fn heal_registration(entry: &InventoryEntry, codes: &BTreeSet<&str>, applied: &mut Vec<AppliedChange>) {
    let path = &entry.canonical_path;
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Some((out, changes)) = rewrite_registration(&text, codes) else {
        return;
    };
    if out == text {
        return;
    }
    if let Err(e) = write_atomic(path, &out) {
        eprintln!("shelbi config-upgrade: writing {} failed: {e}", path.display());
        return;
    }
    for (code, detail) in changes {
        record(applied, entry, code, path, detail);
    }
}

/// Apply the registration auto-heals present in `codes` to a parsed YAML value,
/// returning the re-serialized text and the per-code change details. Returns
/// `None` if the file doesn't parse or nothing changed.
fn rewrite_registration(text: &str, codes: &BTreeSet<&str>) -> Option<(String, Vec<(&'static str, String)>)> {
    let mut value: Value = serde_yaml::from_str(text).ok()?;
    let map = value.as_mapping_mut()?;
    let mut changes: Vec<(&'static str, String)> = Vec::new();

    // Top-level `workers:` -> `workspaces:` (position-preserving). Skip if a
    // `workspaces:` already exists — merging the two is a judgment call.
    if codes.contains("PROJECT_WORKERS_KEY_DEPRECATED")
        && map.contains_key(vs("workers"))
        && !map.contains_key(vs("workspaces"))
    {
        rename_top_key(map, "workers", "workspaces");
        changes.push(("PROJECT_WORKERS_KEY_DEPRECATED", "workers->workspaces".into()));
    }

    // `display_name:` -> `name:`. Every auto-heal case converges on the same
    // end state: the label lives on `name:` and `display_name:` is dropped.
    if codes.contains("PROJECT_DISPLAY_NAME_DEPRECATED") {
        if let Some(display) = map.get(vs("display_name")).cloned() {
            map.shift_remove(vs("display_name"));
            map.insert(vs("name"), display); // updates in place, or appends
            changes.push(("PROJECT_DISPLAY_NAME_DEPRECATED", "display_name->name".into()));
        }
    }

    // Removed top-level keys — only the *inert* ones are auto-heal (detection
    // routes a removed key that still holds data to needs-judgment). Re-check
    // inertness here so a needs-judgment removed key is never dropped.
    if codes.contains("PROJECT_REMOVED_KEY") {
        for key in REMOVED_PROJECT_KEYS {
            let present_inert = map.get(vs(key)).map(is_inert).unwrap_or(false);
            if present_inert {
                map.shift_remove(vs(key));
                changes.push(("PROJECT_REMOVED_KEY", format!("drop:{key}")));
            }
        }
    }

    // Workspace-item edits: scoped to the workspaces/workers sequence so a
    // top-level `orchestrator.runner` (a *current* field) is never touched.
    for ws_key in ["workspaces", "workers"] {
        if let Some(seq) = map.get_mut(vs(ws_key)).and_then(Value::as_sequence_mut) {
            for item in seq.iter_mut() {
                let Some(m) = item.as_mapping_mut() else {
                    continue;
                };
                if codes.contains("WORKSPACE_RUNNER_REMOVED") && m.shift_remove(vs("runner")).is_some() {
                    changes.push(("WORKSPACE_RUNNER_REMOVED", "drop:runner".into()));
                }
                if codes.contains("WORKSPACE_ROLE_DEPRECATED") {
                    apply_role(m, &mut changes);
                }
                if codes.contains("TAG_SHORTHAND_DEPRECATED") {
                    apply_tag_shorthand(m, &mut changes);
                }
            }
        }
    }

    // Machine-item tag shorthand.
    if codes.contains("TAG_SHORTHAND_DEPRECATED") {
        if let Some(seq) = map.get_mut(vs("machines")).and_then(Value::as_sequence_mut) {
            for item in seq.iter_mut() {
                if let Some(m) = item.as_mapping_mut() {
                    apply_tag_shorthand(m, &mut changes);
                }
            }
        }
    }

    // `zen.danger_paths: [..]` (bare seq) -> `{ extend: [..] }`.
    if codes.contains("ZEN_DANGER_PATHS_BARE_SEQ") {
        if let Some(zen) = map.get_mut(vs("zen")).and_then(Value::as_mapping_mut) {
            if let Some(dp) = zen.get(vs("danger_paths")).cloned() {
                if dp.is_sequence() {
                    let mut wrapper = Mapping::new();
                    wrapper.insert(vs("extend"), dp);
                    zen.insert(vs("danger_paths"), Value::Mapping(wrapper));
                    changes.push(("ZEN_DANGER_PATHS_BARE_SEQ", "danger_paths->extend".into()));
                }
            }
        }
    }

    // `zen.danger_paths: { extend: A, override: B }` -> `{ extend: A ∪ B }`.
    // Detection classifies this as NeedsJudgment (the extend-vs-override intent
    // can't be inferred), so this branch NEVER fires on the unattended auto-heal
    // pass — `apply_auto_heal` filters to AutoHeal findings and this code is not
    // one. It runs only through the targeted `apply_single_finding` path, where
    // the user has explicitly approved the documented safe-superset resolution:
    // keep the built-in defaults (extend) plus every path from both lists.
    if codes.contains("ZEN_DANGER_PATHS_CONFLICT") {
        if let Some(zen) = map.get_mut(vs("zen")).and_then(Value::as_mapping_mut) {
            if let Some(dp) = zen.get_mut(vs("danger_paths")).and_then(Value::as_mapping_mut) {
                let extend = dp.get(vs("extend")).and_then(Value::as_sequence).cloned();
                let overridden = dp.get(vs("override")).and_then(Value::as_sequence).cloned();
                if let (Some(ext), Some(ovr)) = (extend, overridden) {
                    let mut merged: Vec<Value> = Vec::new();
                    for v in ext.into_iter().chain(ovr) {
                        if !merged.contains(&v) {
                            merged.push(v);
                        }
                    }
                    let mut wrapper = Mapping::new();
                    wrapper.insert(vs("extend"), Value::Sequence(merged));
                    zen.insert(vs("danger_paths"), Value::Mapping(wrapper));
                    changes
                        .push(("ZEN_DANGER_PATHS_CONFLICT", "extend+override->extend".into()));
                }
            }
        }
    }

    if changes.is_empty() {
        return None;
    }
    let out = serde_yaml::to_string(&value).ok()?;
    Some((out, changes))
}

/// Drop the legacy `role:` from a workspace map. `role: Review` folds into
/// `tags: [review]`; any other role carried no tag and is simply removed.
fn apply_role(m: &mut Mapping, changes: &mut Vec<(&'static str, String)>) {
    let Some(role) = m.get(vs("role")).and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    m.shift_remove(vs("role"));
    if role.eq_ignore_ascii_case("review") {
        merge_tag(m, "review");
        changes.push(("WORKSPACE_ROLE_DEPRECATED", "role:review->tags".into()));
    } else {
        changes.push(("WORKSPACE_ROLE_DEPRECATED", format!("drop:role:{role}")));
    }
}

/// Normalize the scalar `tag:` alias and the bare-string `tags:` shorthand into
/// the canonical `tags: [..]` list on a workspace/machine map.
fn apply_tag_shorthand(m: &mut Mapping, changes: &mut Vec<(&'static str, String)>) {
    if let Some(tag) = m.get(vs("tag")).and_then(Value::as_str).map(str::to_string) {
        m.shift_remove(vs("tag"));
        merge_tag(m, &tag);
        changes.push(("TAG_SHORTHAND_DEPRECATED", "tag->tags".into()));
    }
    // A bare-string `tags: x` (only if `tags` is still a scalar after any
    // `tag:` merge above folded it into a list).
    if let Some(Value::String(s)) = m.get(vs("tags")).cloned() {
        m.insert(vs("tags"), Value::Sequence(vec![Value::String(s)]));
        changes.push(("TAG_SHORTHAND_DEPRECATED", "tags-string->list".into()));
    }
}

/// Ensure `m["tags"]` is a list containing `tag`, folding a prior scalar/string
/// form in without dropping any existing entry.
fn merge_tag(m: &mut Mapping, tag: &str) {
    let existing = m.get(vs("tags")).cloned();
    let mut seq = match existing {
        Some(Value::Sequence(s)) => s,
        Some(Value::String(s)) => vec![Value::String(s)],
        _ => Vec::new(),
    };
    if !seq.iter().any(|v| v.as_str() == Some(tag)) {
        seq.push(Value::String(tag.to_string()));
    }
    m.insert(vs("tags"), Value::Sequence(seq));
}

/// Rename a top-level mapping key while preserving every entry's position.
fn rename_top_key(map: &mut Mapping, from: &str, to: &str) {
    let taken = std::mem::take(map);
    for (k, v) in taken {
        if k == vs(from) {
            map.insert(vs(to), v);
        } else {
            map.insert(k, v);
        }
    }
}

// ---------------------------------------------------------------------------
// statuses.yaml — surgical id rewrite (comment-preserving)

fn heal_statuses(entry: &InventoryEntry, applied: &mut Vec<AppliedChange>) {
    let path = &entry.canonical_path;
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_yaml::from_str::<Value>(&text) else {
        return;
    };
    let Some(seq) = get(&value, "statuses").and_then(Value::as_sequence) else {
        return;
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for item in seq {
        if let Some(raw) = get(item, "id").and_then(Value::as_str) {
            if let Some(canonical) = legacy_status_id(raw) {
                pairs.push((raw.to_string(), canonical));
            }
        }
    }
    if pairs.is_empty() {
        return;
    }
    let mut current = text.clone();
    let mut changes: Vec<(&'static str, String)> = Vec::new();
    for (legacy, canonical) in pairs {
        if let Some(next) = swap_scalar_value(&current, "id", &legacy, &canonical) {
            current = next;
            changes.push(("STATUS_ID_LEGACY", format!("{legacy}->{canonical}")));
        }
    }
    if changes.is_empty() || current == text {
        return;
    }
    if let Err(e) = write_atomic(path, &current) {
        eprintln!("shelbi config-upgrade: writing {} failed: {e}", path.display());
        return;
    }
    for (code, detail) in changes {
        record(applied, entry, code, path, detail);
    }
}

// ---------------------------------------------------------------------------
// workflow YAML — surgical name/agent edits + file rename

fn heal_workflow(entry: &InventoryEntry, codes: &BTreeSet<&str>, applied: &mut Vec<AppliedChange>) {
    let src = entry.canonical_path.clone();
    let Ok(original) = fs::read_to_string(&src) else {
        return;
    };
    let mut text = original.clone();
    let mut changes: Vec<(&'static str, String)> = Vec::new();

    // Fill the derived `agent:` for each bare `owner: agent` status.
    if codes.contains("WORKFLOW_BARE_OWNER") {
        let categories = sibling_status_categories(&src);
        if let Some((next, ids)) = insert_bare_owner_agents(&text, &categories) {
            text = next;
            for id in ids {
                changes.push(("WORKFLOW_BARE_OWNER", format!("agent@{id}")));
            }
        }
    }

    // Rename the legacy `default` workflow to `task`. This is all-or-nothing so
    // we never leave a half-healed file (e.g. a `default.yaml` whose declared
    // name is `task`, which would then trip the loader's name/file mismatch
    // check). Two shapes:
    //   * a `default.yaml` on disk: rename the file to `task.yaml` and swap any
    //     `name: default` inside it. Skipped entirely if a `task.yaml` already
    //     exists (the existing default-workflow migration owns that
    //     reconciliation) so the finding stays reported rather than clashing.
    //   * a differently-named file that merely declares `name: default`: swap
    //     the name in place (no rename).
    let mut dest = src.clone();
    let mut renamed = false;
    if codes.contains("WORKFLOW_DEFAULT_NAME") {
        let is_default_file = src.file_stem().and_then(|s| s.to_str()) == Some("default");
        if is_default_file {
            let target = src.with_file_name("task.yaml");
            if !target.exists() {
                dest = target;
                renamed = true;
                if let Some(next) = swap_scalar_value(&text, "name", "default", "task") {
                    text = next;
                    changes.push(("WORKFLOW_DEFAULT_NAME", "name:default->task".into()));
                }
            }
            // else: `task.yaml` already present — leave `default.yaml` untouched.
        } else if let Some(next) = swap_scalar_value(&text, "name", "default", "task") {
            text = next;
            changes.push(("WORKFLOW_DEFAULT_NAME", "name:default->task".into()));
        }
    }

    if text == original && !renamed {
        return;
    }
    if let Err(e) = write_atomic(&dest, &text) {
        eprintln!("shelbi config-upgrade: writing {} failed: {e}", dest.display());
        return;
    }
    if renamed {
        let _ = fs::remove_file(&src);
        changes.push(("WORKFLOW_DEFAULT_NAME", "file:default.yaml->task.yaml".into()));
    }
    for (code, detail) in changes {
        record(applied, entry, code, &dest, detail);
    }
}

/// Load `id -> category` from the workflow file's sibling `statuses.yaml`, used
/// to derive the `agent:` for a bare `owner: agent` status when the workflow
/// entry doesn't carry an inline `category:`.
fn sibling_status_categories(workflow_path: &Path) -> HashMap<String, StatusCategory> {
    let mut map = HashMap::new();
    let Some(dir) = workflow_path.parent() else {
        return map;
    };
    let Ok(text) = fs::read_to_string(dir.join("statuses.yaml")) else {
        return map;
    };
    let Ok(statuses) = shelbi_core::ProjectStatuses::from_yaml_str(&text) else {
        return map;
    };
    for id in statuses.ids() {
        if let Some(status) = statuses.get(id) {
            map.insert(id.to_string(), status.category);
        }
    }
    map
}

/// For each `owner: agent` status with no `agent:`, insert the derived agent
/// line after its owner line. Returns the rewritten text and the healed status
/// ids, or `None` if nothing applied.
fn insert_bare_owner_agents(
    text: &str,
    categories: &HashMap<String, StatusCategory>,
) -> Option<(String, Vec<String>)> {
    let targets = bare_owner_targets(text, categories);
    if targets.is_empty() {
        return None;
    }
    let target_agent: HashMap<&str, &str> =
        targets.iter().map(|(id, agent)| (id.as_str(), *agent)).collect();

    let mut out: Vec<String> = Vec::new();
    let mut inserted: Vec<String> = Vec::new();
    let mut in_statuses = false;
    let mut current_id: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if indent == 0 && trimmed.starts_with("statuses:") {
            in_statuses = true;
            out.push(line.to_string());
            continue;
        }
        // The statuses block ends at the next indent-0 *mapping key* — a
        // zero-indented `- ` sequence item is still inside the block (block
        // sequences may sit at the parent key's indent in YAML).
        if in_statuses
            && indent == 0
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("- ")
            && trimmed != "-"
        {
            in_statuses = false;
        }

        if in_statuses {
            // A new sequence item resets the item-scoped id tracking.
            if trimmed.starts_with("- ") || trimmed == "-" {
                current_id = None;
            }
            if let Some(id) = kv(trimmed, "id") {
                current_id = Some(id);
            }
            if let Some(owner) = kv(trimmed, "owner") {
                if owner.eq_ignore_ascii_case("agent") {
                    if let Some(id) = current_id.clone() {
                        if let Some(agent) = target_agent.get(id.as_str()) {
                            out.push(line.to_string());
                            out.push(owner_agent_line(line, agent));
                            inserted.push(id);
                            current_id = None; // one insert per status
                            continue;
                        }
                    }
                }
            }
        }
        out.push(line.to_string());
    }

    if inserted.is_empty() {
        return None;
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    Some((joined, inserted))
}

/// Ordered `(status_id, derived_agent)` for every bare `owner: agent` status.
/// The agent is derived from the status category — `ready -> orchestrator`,
/// `active -> developer` (the only categories with a default; see
/// `shelbi_core::workflow`'s `resolve_owner_agent`). A category with no default
/// is skipped (left reported) rather than guessed.
fn bare_owner_targets(text: &str, categories: &HashMap<String, StatusCategory>) -> Vec<(String, &'static str)> {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(seq) = get(&value, "statuses").and_then(Value::as_sequence) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in seq {
        let Some(m) = item.as_mapping() else {
            continue;
        };
        let is_bare_agent = m
            .get(vs("owner"))
            .and_then(Value::as_str)
            .is_some_and(|o| o.eq_ignore_ascii_case("agent"))
            && m.get(vs("agent")).and_then(Value::as_str).is_none();
        if !is_bare_agent {
            continue;
        }
        let Some(id) = m.get(vs("id")).and_then(Value::as_str) else {
            continue;
        };
        // Prefer an inline `category:`, else the sibling statuses.yaml.
        let inline = m.get(vs("category")).and_then(Value::as_str);
        let derived = match inline {
            Some("ready") => Some("orchestrator"),
            Some("active") => Some("developer"),
            Some(_) => None,
            None => match categories.get(id) {
                Some(StatusCategory::Ready) => Some("orchestrator"),
                Some(StatusCategory::Active) => Some("developer"),
                _ => None,
            },
        };
        if let Some(agent) = derived {
            out.push((id.to_string(), agent));
        }
    }
    out
}

/// Build the inserted `agent: <name>` line at the same key indent as the owner
/// line it follows (accounting for a `- owner:` dash-prefixed form).
fn owner_agent_line(owner_line: &str, agent: &str) -> String {
    let trimmed = owner_line.trim_start();
    let indent = owner_line.len() - trimmed.len();
    let extra = if trimmed.starts_with("- ") { 2 } else { 0 };
    format!("{}agent: {}", " ".repeat(indent + extra), agent)
}

// ---------------------------------------------------------------------------
// settings JSON — refresh / anchor / quote

fn heal_settings(entry: &InventoryEntry, codes: &BTreeSet<&str>, applied: &mut Vec<AppliedChange>) {
    let path = &entry.canonical_path;
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let is_template = entry.logical_id.ends_with("workspace-settings-template");
    let refresh_needed = codes.contains("SETTINGS_LEGACY_WORKER_PLACEHOLDER")
        || codes.contains("SETTINGS_STALE_CLAUDE_HOOK");

    if refresh_needed {
        // The workspace-settings template: drive the state layer's existing
        // self-heal, which refreshes it to the shipped default and honors a
        // custom-template override (SkippedOverride) — no double-heal, no fight.
        if is_template {
            let project = scope_label(&entry.scope);
            if let Ok(loaded) = shelbi_state::load_project(project) {
                if let Ok(outcome) = shelbi_state::self_heal_workspace_settings_template(&loaded) {
                    if matches!(
                        outcome,
                        shelbi_state::WorkspaceSettingsTemplateOutcome::Overwritten { .. }
                            | shelbi_state::WorkspaceSettingsTemplateOutcome::Created
                    ) {
                        record_refresh(entry, codes, path, applied);
                    }
                }
            }
            return; // a full refresh subsumes any anchor/quote fix
        }
        // A lifecycle-owned default-agent settings.json is materialized from the
        // same shipped template, so refreshing to it is correct.
        if entry.lifecycle_owned {
            if write_atomic(path, shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE).is_ok() {
                record_refresh(entry, codes, path, applied);
            }
            return;
        }
        // A custom (non-lifecycle-owned) agent settings.json with a legacy
        // placeholder has no shipped default to refresh from — leave that
        // finding reported, but still apply the lossless anchor/quote heals
        // below if they're also present.
    }

    // Lossless, `.shelbi/hooks/`-scoped path anchoring + shell-quoting. Reuse
    // the orchestrator's deploy-time transforms verbatim so the two paths can
    // never disagree, and both are idempotent.
    let healed = shelbi_orchestrator::workspace::quote_shelbi_hook_commands(
        &shelbi_orchestrator::workspace::anchor_shelbi_hook_paths(&text),
    );
    if healed == text {
        return;
    }
    if let Err(e) = write_atomic(path, &healed) {
        eprintln!("shelbi config-upgrade: writing {} failed: {e}", path.display());
        return;
    }
    if codes.contains("SETTINGS_HOOK_PATH_UNANCHORED") {
        record(applied, entry, "SETTINGS_HOOK_PATH_UNANCHORED", path, "anchor:$CLAUDE_PROJECT_DIR".into());
    }
    if codes.contains("SETTINGS_HOOK_CMD_UNQUOTED") {
        record(applied, entry, "SETTINGS_HOOK_CMD_UNQUOTED", path, "shell-quote".into());
    }
}

/// Record a disclosure for each refresh-class code that was present.
fn record_refresh(entry: &InventoryEntry, codes: &BTreeSet<&str>, path: &Path, applied: &mut Vec<AppliedChange>) {
    for code in ["SETTINGS_LEGACY_WORKER_PLACEHOLDER", "SETTINGS_STALE_CLAUDE_HOOK"] {
        if codes.contains(code) {
            record(applied, entry, code, path, "refresh-shipped-default".into());
        }
    }
}

// ---------------------------------------------------------------------------
// global config.yaml keymap — drive the existing loader migration

fn heal_keymap(entry: &InventoryEntry, codes: &BTreeSet<&str>, applied: &mut Vec<AppliedChange>) {
    if !codes.contains("KEYMAP_ZEN_TOGGLE_LEGACY") {
        return;
    }
    // The keymap loader owns the migration (copy into keys.yaml, clear the
    // legacy field). It safe-skips a conflict (keys.yaml already sets an
    // explicit zen_toggle), returning None — leave that reported.
    if let Some(chord) = shelbi_state::keymap::heal_legacy_zen_toggle() {
        record(
            applied,
            entry,
            "KEYMAP_ZEN_TOGGLE_LEGACY",
            &entry.canonical_path,
            format!("zen_toggle:{chord}->keys.yaml"),
        );
    }
}

// ---------------------------------------------------------------------------
// File-shape (sibling) renames

fn heal_rename(entry: &InventoryEntry, finding: &UpgradeFinding, applied: &mut Vec<AppliedChange>) {
    let legacy = &finding.file; // sibling-finding stores the legacy path here
    let target = &entry.canonical_path;
    if !legacy.is_file() || target.exists() {
        // Missing source, or canonical already present — don't clobber.
        return;
    }
    // For the settings-template rename, only ever produce the canonical default
    // filename; a project pointing at a custom template path is left alone.
    if finding.code == "WORKER_SETTINGS_TEMPLATE_RENAME"
        && target.file_name().and_then(|f| f.to_str()) != Some("workspace-settings.json.template")
    {
        return;
    }
    if fs::rename(legacy, target).is_ok() {
        let detail = format!("{}->{}", file_label(legacy), file_label(target));
        record(applied, entry, &finding.code, target, detail);
    }
}

// ---------------------------------------------------------------------------
// Missing lifecycle-owned shipped default

/// Materialize the shipped default `pr-template.md` at the entry's canonical
/// path. Guarded on non-existence so a file created between detection and apply
/// (or a custom one) is never clobbered — the finding stays reported instead.
/// The canonical path was resolved config-mode-aware by the inventory, so this
/// lands in `<repo>/.shelbi/` or `~/.shelbi/projects/<name>/` as appropriate.
fn heal_missing_pr_template(entry: &InventoryEntry, applied: &mut Vec<AppliedChange>) {
    let path = &entry.canonical_path;
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!(
                "shelbi config-upgrade: creating {} failed: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = write_atomic(path, shelbi_state::DEFAULT_PR_TEMPLATE) {
        eprintln!("shelbi config-upgrade: writing {} failed: {e}", path.display());
        return;
    }
    record(applied, entry, PR_TEMPLATE_MISSING, path, "materialize-shipped-default".into());
}

// ---------------------------------------------------------------------------
// Shared helpers

/// A YAML string [`Value`] for use as a mapping key/index.
fn vs(key: &str) -> Value {
    Value::String(key.to_string())
}

/// Parse a `key: value` line whose leading indentation has already been
/// stripped, tolerating a leading `- ` sequence-item dash. Returns the raw
/// value token (which may be empty). The trailing `:` guard keeps `owner:` from
/// matching `owners:`.
fn kv(trimmed: &str, key: &str) -> Option<String> {
    if let Some(v) = parse_key_value(trimmed, key) {
        return Some(v);
    }
    let after_dash = trimmed.strip_prefix('-')?;
    parse_key_value(after_dash.trim_start(), key)
}

fn parse_key_value(s: &str, key: &str) -> Option<String> {
    let rest = s.strip_prefix(key)?.strip_prefix(':')?;
    Some(rest.trim().to_string())
}

/// Replace the scalar value of `key` on the single line where it currently
/// equals `expected`, preserving indentation and any trailing comment. Returns
/// `None` if no such line exists.
fn swap_scalar_value(text: &str, key: &str, expected: &str, replacement: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    for line in text.lines() {
        if !changed {
            if let Some(value) = kv(line.trim_start(), key) {
                if value.split_whitespace().next() == Some(expected) {
                    out.push(line.replacen(expected, replacement, 1));
                    changed = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !changed {
        return None;
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    Some(joined)
}

/// Atomic write via a sibling temp file + rename, so a crash mid-write never
/// leaves a truncated config on disk.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("config");
    let tmp = path.with_file_name(format!(".{name}.configupgrade.tmp"));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("<file>")
        .to_string()
}

/// Push an applied change and disclose it on events.log.
fn record(applied: &mut Vec<AppliedChange>, entry: &InventoryEntry, code: &str, file: &Path, detail: String) {
    let change = AppliedChange {
        scope: entry.scope.clone(),
        logical_id: entry.logical_id.clone(),
        code: code.to_string(),
        file: file.to_path_buf(),
        detail,
    };
    disclose(&change);
    applied.push(change);
}

/// Disclose one applied auto-heal on `~/.shelbi/events.log` as a
/// `config-upgrade` line: `project=<label> config-upgrade code=<CODE>
/// surface=<logical_id> file=<path> change=<old->new>`. All values are
/// collapsed to single tokens so the space-separated `key=value` record stays
/// one line.
fn disclose(change: &AppliedChange) {
    let label = scope_label(&change.scope);
    let file = token(&change.file.to_string_lossy());
    let detail = token(&change.detail);
    let body = format!(
        "project={label} config-upgrade code={} surface={} file={file} change={detail}",
        change.code, change.logical_id
    );
    if let Err(e) = shelbi_state::emit_event_body(&body) {
        eprintln!("shelbi config-upgrade: events.log disclosure failed: {e}");
    }
}

/// Collapse whitespace to `_` so a value is a single events.log token.
fn token(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join("_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::config_upgrade::detect;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};

    fn code_set(codes: &[&'static str]) -> BTreeSet<&'static str> {
        codes.iter().copied().collect()
    }

    // ---- pure registration rewrite ---------------------------------------

    #[test]
    fn rewrite_registration_heals_every_form_and_preserves_orchestrator_runner() {
        let legacy = "\
name: demo
display_name: My Demo
repo: /tmp/demo
contextstore_sync: false
machines:
- name: hub
  kind: local
  work_dir: /tmp/demo
  tags: review
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
workspaces:
- name: w1
  machine: hub
  runner: claude
  role: Review
zen:
  danger_paths:
  - .env
";
        let codes = code_set(&[
            "PROJECT_DISPLAY_NAME_DEPRECATED",
            "PROJECT_REMOVED_KEY",
            "TAG_SHORTHAND_DEPRECATED",
            "WORKSPACE_RUNNER_REMOVED",
            "WORKSPACE_ROLE_DEPRECATED",
            "ZEN_DANGER_PATHS_BARE_SEQ",
        ]);
        let (out, changes) = rewrite_registration(legacy, &codes).expect("rewrite");
        let value: Value = serde_yaml::from_str(&out).unwrap();
        let map = value.as_mapping().unwrap();

        // display_name -> name (the label wins).
        assert!(!map.contains_key(vs("display_name")));
        assert_eq!(map.get(vs("name")).and_then(Value::as_str), Some("My Demo"));
        // Inert removed key dropped.
        assert!(!map.contains_key(vs("contextstore_sync")));
        // Orchestrator's runner is a CURRENT field and must survive.
        let orch = map.get(vs("orchestrator")).and_then(Value::as_mapping).unwrap();
        assert_eq!(
            orch.get(vs("runner")).and_then(Value::as_str).unwrap(),
            "claude"
        );
        // Workspace runner dropped, role folded into tags.
        let ws = map.get(vs("workspaces")).and_then(Value::as_sequence).unwrap()[0]
            .as_mapping()
            .unwrap();
        assert!(!ws.contains_key(vs("runner")));
        assert!(!ws.contains_key(vs("role")));
        assert_eq!(
            ws.get(vs("tags")).and_then(Value::as_sequence).unwrap()[0].as_str(),
            Some("review")
        );
        // Machine bare-string tags normalized to a list.
        let m = map.get(vs("machines")).and_then(Value::as_sequence).unwrap()[0]
            .as_mapping()
            .unwrap();
        assert!(m.get(vs("tags")).unwrap().is_sequence());
        // zen.danger_paths wrapped.
        let dp = map
            .get(vs("zen"))
            .and_then(Value::as_mapping)
            .unwrap()
            .get(vs("danger_paths"))
            .and_then(Value::as_mapping)
            .unwrap();
        assert!(dp.contains_key(vs("extend")));

        assert!(!changes.is_empty());
        // Idempotent: re-running over the healed text changes nothing.
        assert!(rewrite_registration(&out, &codes).is_none());
    }

    // ---- surgical scalar swap --------------------------------------------

    #[test]
    fn swap_scalar_value_replaces_only_the_matching_value_and_keeps_comments() {
        let text = "name: default # the legacy name\nstatuses: []\n";
        let out = swap_scalar_value(text, "name", "default", "task").unwrap();
        assert_eq!(out, "name: task # the legacy name\nstatuses: []\n");
        // No match -> None.
        assert!(swap_scalar_value("name: task\n", "name", "default", "task").is_none());
    }

    // ---- bare-owner agent insertion --------------------------------------

    #[test]
    fn insert_bare_owner_agents_inline_and_sibling_category() {
        // Inline category on the workflow status.
        let inline = "\
name: task
statuses:
- id: in-progress
  category: active
  owner: agent
";
        let (out, ids) = insert_bare_owner_agents(inline, &HashMap::new()).unwrap();
        assert_eq!(ids, vec!["in-progress".to_string()]);
        assert!(out.contains("  owner: agent\n  agent: developer\n"));

        // No inline category -> derive from the sibling statuses map.
        let no_inline = "\
name: task
statuses:
- id: todo
  owner: agent
";
        let mut cats = HashMap::new();
        cats.insert("todo".to_string(), StatusCategory::Ready);
        let (out2, ids2) = insert_bare_owner_agents(no_inline, &cats).unwrap();
        assert_eq!(ids2, vec!["todo".to_string()]);
        assert!(out2.contains("  owner: agent\n  agent: orchestrator\n"));

        // A category with no default is left alone.
        let mut cats2 = HashMap::new();
        cats2.insert("todo".to_string(), StatusCategory::Handoff);
        assert!(insert_bare_owner_agents(no_inline, &cats2).is_none());
    }

    // ---- integration: detect -> apply -> idempotency + disclosure --------

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-config-apply-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("projects")).unwrap();
        p
    }

    #[test]
    fn registration_auto_heals_on_start_idempotently_and_discloses() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        // Isolate the event sink: unset SHELBI_HUB_SOCK so emit falls back to
        // this test home's events.log instead of a live hub socket (this test
        // may run inside a Shelbi-managed pane where that var points at the
        // real hub).
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        let legacy = "\
name: demo
display_name: My Demo
repo: /tmp/demo
contextstore_sync: false
machines:
- name: hub
  kind: local
  work_dir: /tmp/demo
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
workspaces:
- name: w1
  machine: hub
  runner: claude
  role: Review
";
        let path = home.join("projects/demo.yaml");
        std::fs::write(&path, legacy).unwrap();

        let report = detect(&["demo".to_string()]).unwrap();
        assert!(report.auto_heal_count() > 0);

        let applied = apply_auto_heal(&["demo".to_string()], &report);
        assert!(!applied.is_empty(), "nothing applied");
        assert!(applied.iter().any(|c| c.code == "WORKSPACE_RUNNER_REMOVED"));

        // Disk is healed: workspace runner gone, orchestrator runner kept.
        let healed: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hm = healed.as_mapping().unwrap();
        assert!(!hm.contains_key(vs("display_name")));
        assert_eq!(
            hm.get(vs("orchestrator"))
                .and_then(Value::as_mapping)
                .unwrap()
                .get(vs("runner"))
                .and_then(Value::as_str)
                .unwrap(),
            "claude"
        );

        // Disclosed on events.log.
        let events = shelbi_state::events_log_path().unwrap();
        let log = std::fs::read_to_string(&events).unwrap_or_default();
        assert!(log.contains("config-upgrade"), "no disclosure line: {log}");
        assert!(log.contains("WORKSPACE_RUNNER_REMOVED"));

        // Idempotent: a second pass detects no auto-heal and writes nothing.
        let report2 = detect(&["demo".to_string()]).unwrap();
        assert_eq!(report2.auto_heal_count(), 0, "residual auto-heal: {report2:?}");
        let applied2 = apply_auto_heal(&["demo".to_string()], &report2);
        assert!(applied2.is_empty(), "second pass wrote: {applied2:?}");
    }

    #[test]
    fn workflow_default_and_statuses_ids_heal_on_start() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        std::fs::write(home.join("projects/demo.yaml"), "repo: /tmp/demo\n").unwrap();
        let workflows = home.join("projects/demo/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("statuses.yaml"),
            "statuses:\n- id: wip\n  name: WIP\n  category: active\n",
        )
        .unwrap();
        std::fs::write(
            workflows.join("default.yaml"),
            "name: default\nstatuses:\n- id: in-progress\n  category: active\n  owner: agent\n",
        )
        .unwrap();

        let report = detect(&["demo".to_string()]).unwrap();
        let applied = apply_auto_heal(&["demo".to_string()], &report);
        let healed_codes: BTreeSet<&str> = applied.iter().map(|c| c.code.as_str()).collect();
        assert!(healed_codes.contains("STATUS_ID_LEGACY"), "{healed_codes:?}");
        assert!(healed_codes.contains("WORKFLOW_DEFAULT_NAME"), "{healed_codes:?}");
        assert!(healed_codes.contains("WORKFLOW_BARE_OWNER"), "{healed_codes:?}");

        // default.yaml renamed to task.yaml with the healed name + agent.
        assert!(!workflows.join("default.yaml").exists());
        let task = std::fs::read_to_string(workflows.join("task.yaml")).unwrap();
        assert!(task.contains("name: task"));
        assert!(task.contains("agent: developer"));
        // statuses.yaml id canonicalized.
        let statuses = std::fs::read_to_string(workflows.join("statuses.yaml")).unwrap();
        assert!(statuses.contains("id: in-progress"));
        assert!(!statuses.contains("id: wip"));

        // Idempotent.
        let report2 = detect(&["demo".to_string()]).unwrap();
        assert_eq!(report2.auto_heal_count(), 0, "residual: {report2:?}");
    }

    #[test]
    fn default_workflow_left_reported_when_task_yaml_already_exists() {
        // Reconciliation with the existing default-workflow migration: if it
        // already created task.yaml, we must not rename default.yaml onto it or
        // rewrite its name into a file/name mismatch — leave it reported.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        std::fs::write(home.join("projects/demo.yaml"), "repo: /tmp/demo\n").unwrap();
        let workflows = home.join("projects/demo/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("task.yaml"), "name: task\nstatuses: []\n").unwrap();
        let default_body = "name: default\nstatuses: []\n";
        std::fs::write(workflows.join("default.yaml"), default_body).unwrap();

        let report = detect(&["demo".to_string()]).unwrap();
        let applied = apply_auto_heal(&["demo".to_string()], &report);
        assert!(
            !applied.iter().any(|c| c.code == "WORKFLOW_DEFAULT_NAME"),
            "should not have healed the default name into a collision"
        );
        // default.yaml is untouched (still named default, name still default).
        assert_eq!(
            std::fs::read_to_string(workflows.join("default.yaml")).unwrap(),
            default_body
        );
        // Still reported for the user / a later reconciliation.
        let report2 = detect(&["demo".to_string()]).unwrap();
        assert!(report2
            .findings
            .iter()
            .any(|f| f.code == "WORKFLOW_DEFAULT_NAME"));
    }

    #[test]
    fn settings_hook_paths_are_anchored_and_quoted_losslessly() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        std::fs::write(home.join("projects/demo.yaml"), "repo: /tmp/demo\n").unwrap();
        let agent = home.join("projects/demo/agents/developer");
        std::fs::create_dir_all(&agent).unwrap();
        // A relative, unquoted Shelbi hook command plus a user-authored key that
        // must survive verbatim.
        let settings = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [{\"hooks\": [{\"command\": \".shelbi/hooks/stop.sh\"}]}]\n  }\n}\n";
        let path = agent.join("settings.json");
        std::fs::write(&path, settings).unwrap();

        let report = detect(&["demo".to_string()]).unwrap();
        let applied = apply_auto_heal(&["demo".to_string()], &report);
        assert!(applied.iter().any(|c| c.code == "SETTINGS_HOOK_PATH_UNANCHORED"));

        let healed = std::fs::read_to_string(&path).unwrap();
        // Anchored and shell-quoted, user key preserved.
        assert!(healed.contains("\\\"$CLAUDE_PROJECT_DIR/.shelbi/hooks/stop.sh\\\""));
        assert!(healed.contains("\"model\": \"opus\""));

        // Idempotent.
        let report2 = detect(&["demo".to_string()]).unwrap();
        assert_eq!(report2.auto_heal_count(), 0, "residual: {report2:?}");
    }

    // ---- slice 3: targeted single-finding apply --------------------------

    #[test]
    fn apply_single_finding_resolves_one_nj_and_leaves_the_rest() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        // Two needs-judgment findings: a resolvable danger_paths conflict and an
        // unknown top-level key (no mechanical resolution).
        let legacy = "\
name: demo
repo: /tmp/demo
mystery_field: 3
machines:
- name: hub
  kind: local
  work_dir: /tmp/demo
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
workspaces:
- name: w1
  machine: hub
zen:
  danger_paths:
    extend:
    - .env
    override:
    - secrets/**
";
        let path = home.join("projects/demo.yaml");
        std::fs::write(&path, legacy).unwrap();

        let report = detect(&["demo".to_string()]).unwrap();
        let conflict = report
            .findings
            .iter()
            .find(|f| f.code == "ZEN_DANGER_PATHS_CONFLICT")
            .expect("conflict finding");
        let unknown = report
            .findings
            .iter()
            .find(|f| f.code == "PROJECT_UNKNOWN_KEY")
            .expect("unknown-key finding");

        // Apply exactly the conflict finding.
        let applied =
            apply_single_finding(&["demo".to_string()], &conflict.id).expect("apply conflict");
        assert_eq!(applied.len(), 1, "expected one change: {applied:?}");
        assert_eq!(applied[0].code, "ZEN_DANGER_PATHS_CONFLICT");

        // danger_paths is now a single `extend:` = union of both lists; the
        // `override:` is gone. The unknown key is untouched.
        let healed: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let dp = get(get(&healed, "zen").unwrap(), "danger_paths").unwrap();
        let dpm = dp.as_mapping().unwrap();
        assert!(!dpm.contains_key(vs("override")), "override not dropped");
        let ext = dpm.get(vs("extend")).and_then(Value::as_sequence).unwrap();
        let paths: Vec<&str> = ext.iter().filter_map(Value::as_str).collect();
        assert_eq!(paths, vec![".env", "secrets/**"], "union not as expected");
        assert!(
            healed.as_mapping().unwrap().contains_key(vs("mystery_field")),
            "unrelated needs-judgment finding was touched"
        );

        // The conflict is resolved, so its id no longer detects.
        let report2 = detect(&["demo".to_string()]).unwrap();
        assert!(
            !report2.findings.iter().any(|f| f.code == "ZEN_DANGER_PATHS_CONFLICT"),
            "conflict still present after apply"
        );

        // A needs-judgment finding with no mechanical resolution is refused.
        let err = apply_single_finding(&["demo".to_string()], &unknown.id).unwrap_err();
        assert!(
            err.to_string().contains("no mechanical resolution"),
            "expected refusal, got: {err}"
        );

        // An unknown id is a clean error, not a panic.
        let err = apply_single_finding(&["demo".to_string()], "cfg-deadbeef").unwrap_err();
        assert!(err.to_string().contains("no config-upgrade finding"), "got: {err}");
    }
}
