//! Version-agnostic config **validate-and-upgrade** pass (detection +
//! classification).
//!
//! Users may be sitting on a config authored by *any* past release, and no
//! Shelbi config carries a schema version — so every finding here is decided
//! by **content sniffing**, never by a version number. The pass reuses the
//! surface enumeration in [`super::config_surfaces`] (the single place that
//! knows what a Shelbi-owned config file is) and runs deprecation-specific
//! sniffers over each live file.
//!
//! Each finding is classified under the project directive
//! (2026-08-12): *anything that can self-heal, should — automatically;
//! anything that requires judgment is handed to the orchestrator*:
//!
//! * [`Classification::AutoHeal`] — a deterministic, non-lossy, unambiguous
//!   rewrite exists. The on-start pass applies these itself (the write-back
//!   lives in [`super::config_upgrade_apply`]).
//! * [`Classification::NeedsJudgment`] — ambiguous, potentially lossy, or the
//!   correct target isn't mechanically determinable. Never auto-applied;
//!   written to the orchestrator channel to surface to the user. When unsure, a
//!   finding lands here rather than risk a lossy auto-heal.
//!
//! ## Where this module sits in the pass
//!
//! This module owns **detection + classification** and the **emit** step. The
//! full on-start pass ([`run_and_emit`]) runs on hub start and `shelbi reload`:
//! it detects, applies every auto-heal via [`super::config_upgrade_apply`],
//! re-detects the residual, discloses a `config-upgrade` line per project on
//! `events.log`, and writes the residual to the structured findings file
//! ([`FINDINGS_FILE`]) the orchestrator ingests at boot. The needs-judgment
//! half of that file is the channel the orchestrator surfaces to the user; a
//! single approved finding is then applied via
//! [`super::config_upgrade_apply::apply_single_finding`] (the targeted
//! `shelbi config upgrade --apply-finding <id>` path).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use shelbi_core::{validate_project_name, Column, LOCAL_PROJECT_FIELDS, SHARED_PROJECT_FIELDS};

use super::config_surfaces::{live_entries, locate_key, InventoryEntry, Location, SurfaceFormat};

/// Bumped when the on-disk findings file (`FINDINGS_FILE`) changes shape so a
/// mismatched orchestrator can refuse an unknown schema instead of
/// mis-parsing it. Bumped to 2 in slice 3: each finding now carries a stable
/// `id` (for the targeted `--apply-finding` path) and a `rationale`.
pub const FINDINGS_SCHEMA_VERSION: u32 = 2;

/// Findings file the orchestrator ingests at boot. Lives at the top of
/// `~/.shelbi/` next to `events.log`; removed by the pass when a project set is
/// fully clean so a stale file never lingers.
pub const FINDINGS_FILE: &str = "config-upgrade-findings.json";

/// Whether a finding can be healed automatically or needs a human decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// A deterministic, non-lossy rewrite exists — a later slice applies it.
    AutoHeal,
    /// Ambiguous / potentially lossy — routed to the orchestrator for the
    /// user to resolve.
    NeedsJudgment,
}

impl Classification {
    fn as_str(self) -> &'static str {
        match self {
            Classification::AutoHeal => "auto_heal",
            Classification::NeedsJudgment => "needs_judgment",
        }
    }
}

/// One deprecation detected on one config surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeFinding {
    /// Stable content-derived handle for this finding, unique within a report
    /// and identical across restarts for the same still-unresolved finding.
    /// The token the orchestrator passes to `shelbi config upgrade
    /// --apply-finding <id>` once the user approves a needs-judgment fix.
    pub id: String,
    /// The surface's stable id from the inventory (`project.<name>.workflow.task`).
    pub logical_id: String,
    /// `global` or `project:<name>` — lets the orchestrator group per project.
    pub scope: String,
    /// Live canonical path of the file the finding is in.
    pub file: PathBuf,
    pub location: Location,
    pub classification: Classification,
    /// Stable machine token for the deprecation kind.
    pub code: String,
    /// What is deprecated (the legacy form), in one line.
    pub message: String,
    /// The rewrite an auto-heal would apply, or the decision a human must make.
    pub proposed_fix: String,
    /// Why this finding needs judgment — the reason it can't self-heal.
    /// Empty for [`Classification::AutoHeal`] findings (they carry no such
    /// question); populated for [`Classification::NeedsJudgment`].
    pub rationale: String,
}

/// The whole pass's result over a selected project set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeReport {
    pub schema_version: u32,
    pub shelbi_version: String,
    pub findings: Vec<UpgradeFinding>,
}

impl UpgradeReport {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn auto_heal_count(&self) -> usize {
        self.count(Classification::AutoHeal)
    }

    pub fn needs_judgment_count(&self) -> usize {
        self.count(Classification::NeedsJudgment)
    }

    fn count(&self, class: Classification) -> usize {
        self.findings
            .iter()
            .filter(|f| f.classification == class)
            .count()
    }
}

// ---------------------------------------------------------------------------
// Public entry points

/// Absolute path of the orchestrator-handoff findings file.
pub fn findings_path() -> Result<PathBuf> {
    Ok(shelbi_state::shelbi_home()?.join(FINDINGS_FILE))
}

/// Read the persisted orchestrator-handoff channel written by the on-start pass
/// ([`emit`]). Returns `Ok(None)` when no channel is present (a clean config
/// left no file), and errors only on a present-but-unreadable / schema-mismatched
/// file so a stale format never silently mis-parses. This is the boot-ingestion
/// entry point: the same JSON [`emit`] wrote is read back verbatim.
pub fn load_findings() -> Result<Option<UpgradeReport>> {
    let path = findings_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let report: UpgradeReport = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", path.display()))?;
    if report.schema_version != FINDINGS_SCHEMA_VERSION {
        anyhow::bail!(
            "{} is schema v{} but this shelbi understands v{}; rerun `shelbi reload` \
             (or hub start) to regenerate it",
            path.display(),
            report.schema_version,
            FINDINGS_SCHEMA_VERSION
        );
    }
    Ok(Some(report))
}

/// Retain only findings in scope for `projects`: the shared `global` surfaces
/// plus any `project:<name>` whose `<name>` is selected. The channel file is
/// hub-global (one file for every scanned project), so a single project's
/// orchestrator filters it down to what's actually its own.
pub fn filter_to_projects(report: &UpgradeReport, projects: &[String]) -> UpgradeReport {
    let selected: BTreeSet<&str> = projects.iter().map(String::as_str).collect();
    UpgradeReport {
        schema_version: report.schema_version,
        shelbi_version: report.shelbi_version.clone(),
        findings: report
            .findings
            .iter()
            .filter(|f| match f.scope.strip_prefix("project:") {
                Some(p) => selected.contains(p),
                None => f.scope == "global",
            })
            .cloned()
            .collect(),
    }
}

/// Project a report down to only its [`Classification::NeedsJudgment`] findings —
/// the half of the channel the orchestrator surfaces to the user. Order is
/// preserved from the source report (already sorted by file/location/code).
pub fn needs_judgment_report(report: &UpgradeReport) -> UpgradeReport {
    UpgradeReport {
        schema_version: report.schema_version,
        shelbi_version: report.shelbi_version.clone(),
        findings: report
            .findings
            .iter()
            .filter(|f| f.classification == Classification::NeedsJudgment)
            .cloned()
            .collect(),
    }
}

/// Detect (and classify) every deprecation across `projects` plus the shared
/// global surfaces. Read-only.
pub fn detect(projects: &[String]) -> Result<UpgradeReport> {
    let entries = live_entries(projects)?;
    let mut findings = Vec::new();
    for entry in &entries {
        sniff_entry(entry, &mut findings);
    }
    // File-shape (sibling-rename) sniffers work off the entry set as a whole.
    sniff_renamed_siblings(&entries, &mut findings);

    findings.sort_by(|a, b| {
        (&a.file, a.location.line, a.location.column, &a.code).cmp(&(
            &b.file,
            b.location.line,
            b.location.column,
            &b.code,
        ))
    });
    Ok(UpgradeReport {
        schema_version: FINDINGS_SCHEMA_VERSION,
        shelbi_version: env!("CARGO_PKG_VERSION").to_string(),
        findings,
    })
}

/// Outcome of a full validate-and-upgrade pass: the auto-heal write-backs that
/// were applied this run, plus the *residual* report (what still needs
/// attention after healing — needs-judgment findings and any auto-heal a
/// best-effort apply had to skip). On a clean config both are empty.
#[derive(Debug)]
pub struct UpgradeOutcome {
    pub applied: Vec<super::config_upgrade_apply::AppliedChange>,
    pub residual: UpgradeReport,
}

impl Default for UpgradeOutcome {
    fn default() -> Self {
        Self {
            applied: Vec::new(),
            residual: empty_report(),
        }
    }
}

/// Run the pass for one project (plus the shared global surfaces): detect,
/// apply every auto-heal, and emit the residual findings. Best-effort: a
/// detection or emission error is logged and swallowed so the caller's primary
/// job (reload, serve) is never blocked.
pub fn run_for_project(project: &str) -> UpgradeOutcome {
    run_and_emit(&[project.to_string()])
}

/// Whole-hub startup pass: detect + auto-heal across every registered project,
/// then emit. Best-effort — never propagates, so a broken config on one
/// project can't stop the daemon from serving.
pub fn run_startup_pass() -> UpgradeOutcome {
    let projects = match super::config_surfaces::discover_project_names() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("shelbi config-upgrade: could not enumerate projects: {e}");
            return UpgradeOutcome::default();
        }
    };
    run_and_emit(&projects)
}

/// Detect, apply auto-heals, re-detect the residual, and emit it.
///
/// Ordering is deliberate: **detect → apply → re-detect → emit**. Applying
/// before the emit means the orchestrator-handoff findings file and the
/// events.log summary reflect the *post-heal* state — a form that auto-healed
/// this run is not re-surfaced to the orchestrator. Only `AutoHeal` findings
/// are applied here; `NeedsJudgment` findings are left for slice 3's
/// orchestrator channel. The re-detect doubles as the idempotency contract: on
/// a config that's already current (or a second consecutive run) `apply`
/// writes nothing and the residual equals the initial detection.
fn run_and_emit(projects: &[String]) -> UpgradeOutcome {
    let before = match detect(projects) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("shelbi config-upgrade: detection failed: {e}");
            return UpgradeOutcome::default();
        }
    };
    let applied = super::config_upgrade_apply::apply_auto_heal(projects, &before);
    // Re-detect only when something actually changed; otherwise the initial
    // report already is the residual (and re-reading disk would be wasted work).
    let residual = if applied.is_empty() {
        before
    } else {
        match detect(projects) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("shelbi config-upgrade: post-heal re-detection failed: {e}");
                before
            }
        }
    };
    if let Err(e) = emit(&residual) {
        eprintln!("shelbi config-upgrade: emitting findings failed: {e}");
    }
    UpgradeOutcome { applied, residual }
}

fn empty_report() -> UpgradeReport {
    UpgradeReport {
        schema_version: FINDINGS_SCHEMA_VERSION,
        shelbi_version: env!("CARGO_PKG_VERSION").to_string(),
        findings: Vec::new(),
    }
}

/// Persist the findings for orchestrator ingestion and disclose a
/// `config-upgrade` line per affected project on `events.log`.
///
/// The findings file is written whenever there are findings and **removed**
/// when the set is clean, so a run that fixes the last deprecation clears the
/// stale file the orchestrator would otherwise keep re-surfacing.
pub fn emit(report: &UpgradeReport) -> Result<()> {
    let path = findings_path()?;
    if report.is_empty() {
        // Clean: drop any stale findings file so the orchestrator stops
        // surfacing already-resolved deprecations.
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing stale {}", path.display()))?;
        }
        return Ok(());
    }
    let json = serde_json::to_vec_pretty(report).context("serializing upgrade findings")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;

    for (scope_project, auto, judg) in per_project_counts(report) {
        // `scope_project` is either "global" or a validated project id; both
        // are events.log-token-safe. Build the body directly so we can carry
        // two count fields (append_project_event only carries one `reason=`).
        let body = format!(
            "project={scope_project} config-upgrade auto_heal={auto} needs_judgment={judg}"
        );
        if let Err(e) = shelbi_state::emit_event_body(&body) {
            eprintln!("shelbi config-upgrade: events.log disclosure failed: {e}");
        }
    }
    Ok(())
}

/// Collapse findings to `(scope_label, auto_heal_count, needs_judgment_count)`
/// rows, one per scope, in stable order. `global` findings disclose under the
/// literal `global` scope label.
fn per_project_counts(report: &UpgradeReport) -> Vec<(String, usize, usize)> {
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    for f in &report.findings {
        scopes.insert(scope_label(&f.scope).to_string());
    }
    scopes
        .into_iter()
        .map(|label| {
            let (mut auto, mut judg) = (0, 0);
            for f in &report.findings {
                if scope_label(&f.scope) == label {
                    match f.classification {
                        Classification::AutoHeal => auto += 1,
                        Classification::NeedsJudgment => judg += 1,
                    }
                }
            }
            (label, auto, judg)
        })
        .collect()
}

/// `project:demo` -> `demo`; `global` -> `global`.
pub(crate) fn scope_label(scope: &str) -> &str {
    scope.strip_prefix("project:").unwrap_or(scope)
}

// ---------------------------------------------------------------------------
// Dispatch

fn sniff_entry(entry: &InventoryEntry, out: &mut Vec<UpgradeFinding>) {
    if !entry.exists {
        return;
    }
    let text = match std::fs::read_to_string(&entry.canonical_path) {
        Ok(t) => t,
        Err(_) => return, // unreadable is the lint's problem, not the upgrade's
    };
    let id = entry.logical_id.as_str();

    if id.ends_with(".registration")
        || id.ends_with(".registration.shared")
        || id.ends_with(".registration.local")
    {
        let project = scope_label(&entry.scope);
        sniff_project_registration(project, entry, &text, out);
    } else if id.ends_with(".statuses") {
        sniff_statuses(entry, &text, out);
    } else if id.contains(".workflow.") {
        sniff_workflow(entry, &text, out);
    } else if id == "global.preferences" {
        sniff_user_config(entry, &text, out);
    } else if matches!(entry.format, SurfaceFormat::Json)
        && (id.ends_with(".settings") || id.ends_with("workspace-settings-template"))
    {
        sniff_settings_json(entry, &text, out);
    }
}

// ---------------------------------------------------------------------------
// Project registration YAML

/// Known-removed top-level project keys: fields that shipped in a past release
/// and have since been deleted from the model. Dropping one is an auto-heal
/// *only* when it carries no real data (an inert default) — a removed key that
/// still holds user data routes to needs_judgment so nothing is silently lost.
pub(crate) const REMOVED_PROJECT_KEYS: &[&str] = &["contextstore_sync", "assistant_name", "review"];

fn sniff_project_registration(
    project: &str,
    entry: &InventoryEntry,
    text: &str,
    out: &mut Vec<UpgradeFinding>,
) {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return; // a YAML syntax error is the lint's job
    };
    let Some(map) = value.as_mapping() else {
        return;
    };

    let allowed: BTreeSet<&str> = SHARED_PROJECT_FIELDS
        .iter()
        .chain(LOCAL_PROJECT_FIELDS.iter())
        .copied()
        .collect();

    for (k, v) in map {
        let Some(key) = k.as_str() else { continue };
        match key {
            "display_name" => sniff_display_name(project, entry, text, map, v, out),
            "workers" => out.push(finding(
                entry,
                Classification::AutoHeal,
                "PROJECT_WORKERS_KEY_DEPRECATED",
                "top-level `workers:` is the pre-rename name for `workspaces:`",
                "Rename the key to `workspaces:` (same value).",
                locate_key(text, "workers"),
            )),
            k if REMOVED_PROJECT_KEYS.contains(&k) => {
                sniff_removed_key(entry, text, key, v, out)
            }
            k if !allowed.contains(k) => out.push(finding(
                entry,
                // Not a known field and not a known-removed one: could be a
                // typo or user data — never auto-drop it.
                Classification::NeedsJudgment,
                "PROJECT_UNKNOWN_KEY",
                &format!("unknown top-level project key `{key}`"),
                "Confirm whether this key is a typo, stale, or data to keep, then remove or fix it.",
                locate_key(text, key),
            )),
            _ => {}
        }
    }

    // `name:` that looks like a slug id rather than a free-form label — under
    // the new model the id is filename-derived, so a slug `name:` that differs
    // from the file stem is a rename decision, not a mechanical drop.
    if let Some(name) = get(&value, "name").and_then(Value::as_str) {
        let looks_like_id = name != project && validate_project_name(name).is_ok();
        if looks_like_id {
            out.push(finding(
                entry,
                Classification::NeedsJudgment,
                "PROJECT_NAME_LOOKS_LIKE_ID",
                &format!("`name: {name}` looks like an id but the id is `{project}` (from the filename)"),
                "Decide whether this is the human label (keep) or an intended rename of the project id.",
                locate_key(text, "name"),
            ));
        }
    }

    // Nested surfaces.
    for ws_key in ["workspaces", "workers"] {
        if let Some(seq) = get(&value, ws_key).and_then(Value::as_sequence) {
            for item in seq {
                sniff_workspace_item(entry, text, item, out);
            }
        }
    }
    if let Some(seq) = get(&value, "machines").and_then(Value::as_sequence) {
        for item in seq {
            sniff_tag_shorthand(entry, text, item, "machine", out);
        }
    }
    if let Some(zen) = get(&value, "zen") {
        sniff_zen_danger_paths(entry, text, zen, out);
    }
}

fn sniff_display_name(
    project: &str,
    entry: &InventoryEntry,
    text: &str,
    map: &serde_yaml::Mapping,
    display_value: &Value,
    out: &mut Vec<UpgradeFinding>,
) {
    let display = display_value.as_str().unwrap_or_default();
    let name = map
        .get(Value::String("name".into()))
        .and_then(Value::as_str);
    // Under the old two-key shape `name:` was the id and `display_name:` the
    // label; under the new shape `name:` *is* the label and the id is the
    // filename stem. The migration is deterministic — the label becomes
    // `display_name`'s value — whenever the current `name:` is absent, already
    // equals `display_name`, or is just the now-redundant old id (it equals the
    // filename stem `project`). Only a `name:` that disagrees with *both*
    // `display_name` and the stem is genuinely ambiguous.
    let (class, fix) = match name {
        None => (
            Classification::AutoHeal,
            format!("Move the value to `name: {display}` and drop `display_name:`."),
        ),
        Some(n) if n == display => (
            Classification::AutoHeal,
            "Drop the redundant `display_name:` (it equals `name:`).".to_string(),
        ),
        Some(n) if n == project => (
            Classification::AutoHeal,
            format!(
                "`name: {n}` is the now-redundant old id (it equals the filename); set `name: {display}` and drop `display_name:`."
            ),
        ),
        Some(n) => (
            Classification::NeedsJudgment,
            format!(
                "`display_name: {display}` and `name: {n}` disagree (and `name:` isn't the id) — pick which is the label before dropping `display_name:`."
            ),
        ),
    };
    out.push(finding(
        entry,
        class,
        "PROJECT_DISPLAY_NAME_DEPRECATED",
        "`display_name:` is the pre-rename label key; the label is now `name:`",
        &fix,
        locate_key(text, "display_name"),
    ));
}

fn sniff_removed_key(
    entry: &InventoryEntry,
    text: &str,
    key: &str,
    v: &Value,
    out: &mut Vec<UpgradeFinding>,
) {
    // Inert (null / empty map / empty seq / false) → a safe drop. Anything
    // else may be data the user still wants, so hand it to a human.
    let inert = is_inert(v);
    let (class, fix) = if inert {
        (
            Classification::AutoHeal,
            format!("`{key}:` is a removed key with no data — drop it."),
        )
    } else {
        (
            Classification::NeedsJudgment,
            format!("`{key}:` is a removed key that still holds data — confirm it's safe to drop."),
        )
    };
    out.push(finding(
        entry,
        class,
        "PROJECT_REMOVED_KEY",
        &format!("`{key}:` is no longer a Shelbi configuration key"),
        &fix,
        locate_key(text, key),
    ));
}

fn sniff_workspace_item(
    entry: &InventoryEntry,
    text: &str,
    item: &Value,
    out: &mut Vec<UpgradeFinding>,
) {
    let Some(map) = item.as_mapping() else { return };
    let ws_name = map
        .get(Value::String("name".into()))
        .and_then(Value::as_str)
        .unwrap_or("<workspace>");

    if map.contains_key(Value::String("runner".into())) {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "WORKSPACE_RUNNER_REMOVED",
            &format!("workspace `{ws_name}` carries a `runner:` — workspaces no longer select a runner"),
            "Drop the per-workspace `runner:` (the dispatched agent owns runner selection).",
            locate_key(text, "runner"),
        ));
    }
    if let Some(role) = map.get(Value::String("role".into())).and_then(Value::as_str) {
        let fix = if role.eq_ignore_ascii_case("review") {
            "Replace `role: Review` with `tags: [review]`."
        } else {
            "Drop `role:` — it maps to no tag (only `review` carried meaning)."
        };
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "WORKSPACE_ROLE_DEPRECATED",
            &format!("workspace `{ws_name}` uses the legacy `role:` key"),
            fix,
            locate_key(text, "role"),
        ));
    }
    sniff_tag_shorthand(entry, text, item, "workspace", out);
}

/// Flag the scalar `tag:` alias and the bare-string `tags:` shorthand on a
/// workspace or machine — both fold to a `tags: [..]` list on load, so
/// rewriting them is a lossless normalization.
fn sniff_tag_shorthand(
    entry: &InventoryEntry,
    text: &str,
    item: &Value,
    kind: &str,
    out: &mut Vec<UpgradeFinding>,
) {
    let Some(map) = item.as_mapping() else { return };
    let name = map
        .get(Value::String("name".into()))
        .and_then(Value::as_str)
        .unwrap_or("<item>");

    if map.contains_key(Value::String("tag".into())) {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "TAG_SHORTHAND_DEPRECATED",
            &format!("{kind} `{name}` uses the scalar `tag:` alias"),
            "Rewrite `tag: x` as `tags: [x]`.",
            locate_key(text, "tag"),
        ));
    }
    if let Some(tags) = map.get(Value::String("tags".into())) {
        if tags.is_string() {
            out.push(finding(
                entry,
                Classification::AutoHeal,
                "TAG_SHORTHAND_DEPRECATED",
                &format!("{kind} `{name}` uses the bare-string `tags:` shorthand"),
                "Rewrite `tags: x` as `tags: [x]`.",
                locate_key(text, "tags"),
            ));
        }
    }
}

fn sniff_zen_danger_paths(
    entry: &InventoryEntry,
    text: &str,
    zen: &Value,
    out: &mut Vec<UpgradeFinding>,
) {
    let Some(dp) = get(zen, "danger_paths") else {
        return;
    };
    if dp.is_sequence() {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "ZEN_DANGER_PATHS_BARE_SEQ",
            "`zen.danger_paths` is a bare sequence — the current form is `{ extend: [...] }`",
            "Wrap the list: `danger_paths: { extend: [...] }`.",
            locate_key(text, "danger_paths"),
        ));
    } else if let Some(m) = dp.as_mapping() {
        let has_extend = m.contains_key(Value::String("extend".into()));
        let has_override = m.contains_key(Value::String("override".into()));
        if has_extend && has_override {
            out.push(finding(
                entry,
                Classification::NeedsJudgment,
                "ZEN_DANGER_PATHS_CONFLICT",
                "`zen.danger_paths` sets both `extend:` and `override:` — only one is allowed",
                "Merge both into a single `extend:` (the built-in danger paths plus every path \
                 from both lists — the safe superset). If you meant `override:` (drop the \
                 built-in defaults), edit it by hand instead.",
                locate_key(text, "danger_paths"),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// statuses.yaml

fn sniff_statuses(entry: &InventoryEntry, text: &str, out: &mut Vec<UpgradeFinding>) {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return;
    };
    let Some(seq) = get(&value, "statuses").and_then(Value::as_sequence) else {
        return;
    };
    for item in seq {
        if let Some(raw_id) = get(item, "id").and_then(Value::as_str) {
            if let Some(canonical) = legacy_status_id(raw_id) {
                out.push(finding(
                    entry,
                    Classification::AutoHeal,
                    "STATUS_ID_LEGACY",
                    &format!("status id `{raw_id}` is a legacy spelling of `{canonical}`"),
                    &format!("Rewrite the id to the canonical `{canonical}`."),
                    locate_key(text, "id"),
                ));
            }
        }
    }
}

/// `Some(canonical)` when `raw` is a legacy status-id spelling that
/// canonicalizes to a *different* string. Delegates to [`Column::from_status_id`]
/// (the single normalization table) so this never drifts from the loader; the
/// extra `wire_str` guard keeps the legitimate on-disk snake_case wire form
/// (`in_progress`) from being mistaken for a deprecation.
pub(crate) fn legacy_status_id(raw: &str) -> Option<String> {
    let canonical = Column::from_status_id(raw);
    let trimmed = raw.trim();
    if canonical.as_str() != trimmed && canonical.wire_str() != trimmed {
        Some(canonical.as_str().to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// workflow YAML

fn sniff_workflow(entry: &InventoryEntry, text: &str, out: &mut Vec<UpgradeFinding>) {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return;
    };

    // `name: default` (or a `default.yaml` filename) is the legacy name for the
    // built-in `task` workflow.
    let stem = entry
        .canonical_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let declared_name = get(&value, "name").and_then(Value::as_str);
    if declared_name == Some("default") || (declared_name.is_none() && stem == "default") {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "WORKFLOW_DEFAULT_NAME",
            "`default` is the legacy name for the built-in `task` workflow",
            "Rename the workflow (and its file) to `task`.",
            locate_key(text, "name"),
        ));
    }

    // Per-status owner/agent shape.
    if let Some(statuses) = get(&value, "statuses").and_then(Value::as_sequence) {
        for item in statuses {
            sniff_workflow_status(entry, text, item, out);
        }
    }
}

fn sniff_workflow_status(
    entry: &InventoryEntry,
    text: &str,
    item: &Value,
    out: &mut Vec<UpgradeFinding>,
) {
    let Some(map) = item.as_mapping() else { return };
    let owner = map
        .get(Value::String("owner".into()))
        .and_then(Value::as_str);
    let agent = map
        .get(Value::String("agent".into()))
        .and_then(Value::as_str);
    let status_id = map
        .get(Value::String("id".into()))
        .and_then(Value::as_str)
        .unwrap_or("<status>");

    match owner {
        // Bare `owner: agent` with no `agent:` — the runner is derived from the
        // status category on load. A deterministic fill, so an auto-heal.
        Some(o) if o.eq_ignore_ascii_case("agent") && agent.is_none() => {
            out.push(finding(
                entry,
                Classification::AutoHeal,
                "WORKFLOW_BARE_OWNER",
                &format!("status `{status_id}` has `owner: agent` but no `agent:` — the agent is derived from its category"),
                "Add an explicit `agent:` (or let the pass fill it from the status category).",
                locate_key(text, "owner"),
            ));
        }
        // A named-owner form (`owner: <name>`, neither `user` nor `agent`)
        // alongside a *different* explicit `agent:` is a genuine conflict a
        // human must resolve.
        Some(o)
            if !o.eq_ignore_ascii_case("agent")
                && !o.eq_ignore_ascii_case("user")
                && agent.is_some()
                && agent != Some(o) =>
        {
            out.push(finding(
                entry,
                Classification::NeedsJudgment,
                "WORKFLOW_OWNER_AGENT_CONFLICT",
                &format!(
                    "status `{status_id}` has a legacy named `owner: {o}` conflicting with `agent: {}`",
                    agent.unwrap_or_default()
                ),
                "Decide which agent owns this status, then set `owner: agent` + a single `agent:`.",
                locate_key(text, "owner"),
            ));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// global config.yaml (UserConfig)

fn sniff_user_config(entry: &InventoryEntry, text: &str, out: &mut Vec<UpgradeFinding>) {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return;
    };
    // Legacy keybinding home: `config.yaml::keymap.zen_toggle`. The current
    // home is `keys.yaml::defaults.global.zen_toggle`; the loader already
    // migrates this on start, so surfacing it as an auto-heal is consistent.
    if let Some(keymap) = get(&value, "keymap") {
        if get(keymap, "zen_toggle").is_some() {
            out.push(finding(
                entry,
                Classification::AutoHeal,
                "KEYMAP_ZEN_TOGGLE_LEGACY",
                "`keymap.zen_toggle` in config.yaml is the pre-keys.yaml binding home",
                "Move the binding to `keys.yaml::defaults.global.zen_toggle`.",
                locate_key(text, "zen_toggle"),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// settings JSON (workspace-settings template + per-agent settings.json)

fn sniff_settings_json(entry: &InventoryEntry, text: &str, out: &mut Vec<UpgradeFinding>) {
    // A finding here means "the corresponding deploy-time self-heal's
    // idempotent transform is not yet applied" — detection mirrors the heal
    // exactly (see workspace.rs `anchor_shelbi_hook_paths` /
    // `quote_shelbi_hook_commands` and the `{{worker_` template check).
    if text.contains("{{worker_") {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "SETTINGS_LEGACY_WORKER_PLACEHOLDER",
            "settings template uses an unsupported `{{worker_*}}` placeholder",
            "Refresh from the shipped default template.",
            Location { line: 1, column: 1 },
        ));
        // The placeholder makes the remaining (JSON-shaped) checks unreliable.
        return;
    }
    let rendered = text.replace("{{workspace_permissions_mode}}", "default");
    if rendered.contains(".shelbi/hooks/claude.") {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "SETTINGS_STALE_CLAUDE_HOOK",
            "settings reference pre-rename `.shelbi/hooks/claude.*` hook scripts",
            "Point the hooks at the current script names.",
            Location { line: 1, column: 1 },
        ));
    }
    if rendered.contains("\".shelbi/hooks/") {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "SETTINGS_HOOK_PATH_UNANCHORED",
            "a hook command uses a relative `.shelbi/hooks/` path",
            "Anchor it with `$CLAUDE_PROJECT_DIR/` so a spaced worktree path works.",
            Location { line: 1, column: 1 },
        ));
    }
    // An anchored-but-unquoted command reads `"$CLAUDE_PROJECT_DIR/...` in the
    // raw JSON. The *quoted* form is `"\"$CLAUDE_PROJECT_DIR/...` — the same
    // substring but with a backslash before the inner quote — so we only flag
    // occurrences NOT preceded by a backslash, mirroring the quote heal's
    // idempotency skip.
    if contains_unescaped(&rendered, "\"$CLAUDE_PROJECT_DIR/.shelbi/hooks/") {
        out.push(finding(
            entry,
            Classification::AutoHeal,
            "SETTINGS_HOOK_CMD_UNQUOTED",
            "a `$CLAUDE_PROJECT_DIR`-anchored hook command isn't shell-quoted",
            "Shell-quote the command so a spaced worktree path is passed as one argument.",
            Location { line: 1, column: 1 },
        ));
    }
}

// ---------------------------------------------------------------------------
// Renamed-sibling (file-shape) sniffers

/// Flag legacy on-disk *filenames* sitting beside their current name: a
/// `statuses.yml` where `statuses.yaml` is expected, and a `worker-settings*`
/// beside the `workspace-settings.json.template`. Both are unambiguous renames.
fn sniff_renamed_siblings(entries: &[InventoryEntry], out: &mut Vec<UpgradeFinding>) {
    for entry in entries {
        if entry.logical_id.ends_with(".statuses") {
            let yml = entry.canonical_path.with_extension("yml");
            if yml.is_file() {
                out.push(sibling_finding(
                    entry,
                    &yml,
                    "STATUSES_YML_EXTENSION",
                    "`workflows/statuses.yml` uses the legacy `.yml` extension",
                    "Rename it to `statuses.yaml`.",
                ));
            }
        }
        if entry.logical_id.ends_with("workspace-settings-template") {
            if let Some(dir) = entry.canonical_path.parent() {
                for legacy in [
                    "worker-settings.json.template",
                    "worker-settings.json",
                    "workspace-settings.json",
                ] {
                    let p = dir.join(legacy);
                    if p.is_file() {
                        out.push(sibling_finding(
                            entry,
                            &p,
                            "WORKER_SETTINGS_TEMPLATE_RENAME",
                            &format!("`{legacy}` is a pre-rename workspace-settings template"),
                            "Rename it to `workspace-settings.json.template`.",
                        ));
                    }
                }
            }
        }
    }
}

fn sibling_finding(
    entry: &InventoryEntry,
    file: &Path,
    code: &str,
    message: &str,
    fix: &str,
) -> UpgradeFinding {
    UpgradeFinding {
        id: finding_id(&entry.scope, &entry.logical_id, code, message, fix),
        logical_id: entry.logical_id.clone(),
        scope: entry.scope.clone(),
        file: file.to_path_buf(),
        location: Location { line: 1, column: 1 },
        classification: Classification::AutoHeal,
        code: code.to_string(),
        message: message.to_string(),
        proposed_fix: fix.to_string(),
        rationale: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn finding(
    entry: &InventoryEntry,
    classification: Classification,
    code: &str,
    message: &str,
    proposed_fix: &str,
    location: Location,
) -> UpgradeFinding {
    let rationale = match classification {
        Classification::NeedsJudgment => needs_judgment_rationale(code).to_string(),
        Classification::AutoHeal => String::new(),
    };
    UpgradeFinding {
        id: finding_id(&entry.scope, &entry.logical_id, code, message, proposed_fix),
        logical_id: entry.logical_id.clone(),
        scope: entry.scope.clone(),
        file: entry.canonical_path.clone(),
        location,
        classification,
        code: code.to_string(),
        message: message.to_string(),
        proposed_fix: proposed_fix.to_string(),
        rationale,
    }
}

/// A stable, content-derived id for a finding. It hashes the identifying tuple
/// — scope, surface, code, and the specific message/fix (which embed the exact
/// key/name the finding is about) — so the *same* unresolved finding hashes to
/// the *same* id on every boot (the channel never accumulates duplicates), while
/// two distinct findings never collide. FNV-1a keeps it dependency-free and
/// deterministic across runs of the same binary.
fn finding_id(scope: &str, logical_id: &str, code: &str, message: &str, proposed_fix: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in [scope, "\x1f", logical_id, "\x1f", code, "\x1f", message, "\x1f", proposed_fix] {
        for b in part.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("cfg-{h:016x}")
}

/// Why a needs-judgment finding of `code` can't self-heal — the reason handed to
/// the orchestrator (and through it, the user) alongside the proposed fix. When
/// a code has no specific entry, a generic "ambiguous / potentially lossy" note
/// is used so nothing surfaces without a rationale.
fn needs_judgment_rationale(code: &str) -> &'static str {
    match code {
        "PROJECT_UNKNOWN_KEY" => {
            "An unrecognized key could be a typo, a stale field, or data you still \
             want — dropping or renaming it mechanically risks losing intent."
        }
        "PROJECT_NAME_LOOKS_LIKE_ID" => {
            "Whether this is the human label to keep or an intended rename of the \
             project id can't be inferred — the two have opposite effects."
        }
        "PROJECT_DISPLAY_NAME_DEPRECATED" => {
            "`display_name:` and `name:` disagree and `name:` isn't the old id, so \
             which one is the real label is a choice only you can make."
        }
        "PROJECT_REMOVED_KEY" => {
            "This removed key still holds data; dropping it could discard something \
             you meant to keep, so it needs confirmation rather than a silent drop."
        }
        "ZEN_DANGER_PATHS_CONFLICT" => {
            "`extend:` adds to the built-in danger paths; `override:` replaces them. \
             Setting both is contradictory, and resolving it depends on whether you \
             meant to keep or drop the defaults — a security-relevant choice."
        }
        "WORKFLOW_OWNER_AGENT_CONFLICT" => {
            "A legacy named `owner:` conflicts with an explicit `agent:`; which agent \
             should own the status is a decision, not a mechanical rewrite."
        }
        _ => "This form is ambiguous or potentially lossy, so a human should confirm the fix.",
    }
}

/// True when `needle` occurs in `haystack` at least once *not* immediately
/// preceded by a backslash. Lets the settings sniffer tell an unquoted hook
/// command (`"$CLAUDE_PROJECT_DIR/...`) from the already-shell-quoted form
/// (`\"$CLAUDE_PROJECT_DIR/...`), matching the quote heal's idempotency.
fn contains_unescaped(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let idx = from + rel;
        if idx == 0 || bytes[idx - 1] != b'\\' {
            return true;
        }
        from = idx + 1;
    }
    false
}

/// Mapping lookup by string key on a [`Value`], returning the child value.
pub(crate) fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_mapping()?.get(Value::String(key.to_string()))
}

/// A value that carries no real data: null, empty map, empty sequence, or a
/// `false` flag. Used to tell an inert removed-key default (safe drop) from one
/// that still holds user data (needs judgment).
pub(crate) fn is_inert(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Sequence(s) => s.is_empty(),
        Value::Mapping(m) => m.is_empty(),
        _ => false,
    }
}

/// Human-readable render of a report, mirroring `config lint`'s format so the
/// `config upgrade` CLI reads familiarly.
pub fn render_human(report: &UpgradeReport) -> String {
    let mut out = String::new();
    for f in &report.findings {
        out.push_str(&format!(
            "{}:{}:{}: {}[{}] {} ({})\n",
            f.file.display(),
            f.location.line,
            f.location.column,
            f.classification.as_str(),
            f.code,
            f.message,
            f.logical_id,
        ));
        out.push_str(&format!("  fix: {}\n", f.proposed_fix));
    }
    if report.is_empty() {
        out.push_str("configuration up to date: 0 findings\n");
    } else {
        out.push_str(&format!(
            "{} auto-heal, {} needs-judgment\n",
            report.auto_heal_count(),
            report.needs_judgment_count()
        ));
    }
    out
}

/// Human render of the needs-judgment channel: one block per finding with the
/// stable `id` (for `--apply-finding`), surface + location, legacy form, proposed
/// fix, and the rationale. Mirrors the shape the orchestrator surfaces to the
/// user. `report` should already be a needs-judgment projection.
pub fn render_needs_judgment(report: &UpgradeReport) -> String {
    if report.findings.is_empty() {
        return "no needs-judgment config findings: nothing to resolve\n".to_string();
    }
    let mut out = String::new();
    for f in &report.findings {
        out.push_str(&format!(
            "[{}] {} ({})\n",
            f.id,
            f.code,
            scope_label(&f.scope),
        ));
        out.push_str(&format!(
            "  where: {}:{}:{}\n",
            f.file.display(),
            f.location.line,
            f.location.column,
        ));
        out.push_str(&format!("  legacy form: {}\n", f.message));
        out.push_str(&format!("  proposed fix: {}\n", f.proposed_fix));
        out.push_str(&format!("  why it needs judgment: {}\n", f.rationale));
    }
    out.push_str(&format!(
        "{} needs-judgment finding(s) — apply one you've approved with \
         `shelbi config upgrade --apply-finding <id>`\n",
        report.findings.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};

    fn entry(id: &str, scope: &str, format: SurfaceFormat) -> InventoryEntry {
        InventoryEntry {
            logical_id: id.to_string(),
            scope: scope.to_string(),
            canonical_path: PathBuf::from("/tmp/config.yaml"),
            candidate_path: PathBuf::from("config.yaml"),
            format,
            exists: true,
            lifecycle_owned: false,
        }
    }

    fn reg_entry() -> InventoryEntry {
        entry(
            "project.demo.registration",
            "project:demo",
            SurfaceFormat::Yaml,
        )
    }

    fn project_findings(text: &str) -> Vec<UpgradeFinding> {
        let mut out = Vec::new();
        sniff_project_registration("demo", &reg_entry(), text, &mut out);
        out
    }

    fn find<'a>(fs: &'a [UpgradeFinding], code: &str) -> Option<&'a UpgradeFinding> {
        fs.iter().find(|f| f.code == code)
    }

    fn codes(fs: &[UpgradeFinding]) -> Vec<&str> {
        fs.iter().map(|f| f.code.as_str()).collect()
    }

    // ---- project registration -------------------------------------------

    #[test]
    fn display_name_without_name_is_auto_heal() {
        let fs = project_findings("display_name: My Demo\n");
        let f = find(&fs, "PROJECT_DISPLAY_NAME_DEPRECATED").expect("finding");
        assert_eq!(f.classification, Classification::AutoHeal);
    }

    #[test]
    fn display_name_disagreeing_with_name_and_stem_is_needs_judgment() {
        // `name:` (relabel) differs from BOTH the label and the stem `demo`.
        let fs = project_findings("name: Renamed Thing\ndisplay_name: Different Label\n");
        let f = find(&fs, "PROJECT_DISPLAY_NAME_DEPRECATED").expect("finding");
        assert_eq!(f.classification, Classification::NeedsJudgment);
    }

    #[test]
    fn display_name_with_name_equal_to_stem_is_auto_heal() {
        // The common legacy shape: `name:` was the old id (== filename stem),
        // `display_name:` the real label. Deterministic migration -> auto-heal.
        let fs = project_findings("name: demo\ndisplay_name: My Demo\n");
        let f = find(&fs, "PROJECT_DISPLAY_NAME_DEPRECATED").expect("finding");
        assert_eq!(f.classification, Classification::AutoHeal);
    }

    #[test]
    fn display_name_equal_to_name_is_auto_heal() {
        let fs = project_findings("name: Same\ndisplay_name: Same\n");
        let f = find(&fs, "PROJECT_DISPLAY_NAME_DEPRECATED").expect("finding");
        assert_eq!(f.classification, Classification::AutoHeal);
    }

    #[test]
    fn workers_key_is_auto_heal() {
        let fs = project_findings("workers:\n- name: w1\n  machine: hub\n");
        assert_eq!(
            find(&fs, "PROJECT_WORKERS_KEY_DEPRECATED").unwrap().classification,
            Classification::AutoHeal
        );
    }

    #[test]
    fn removed_key_inert_is_auto_heal_but_with_data_is_needs_judgment() {
        let inert = project_findings("contextstore_sync: false\n");
        assert_eq!(
            find(&inert, "PROJECT_REMOVED_KEY").unwrap().classification,
            Classification::AutoHeal
        );
        let with_data = project_findings("review:\n  reviewers: [a, b]\n");
        assert_eq!(
            find(&with_data, "PROJECT_REMOVED_KEY").unwrap().classification,
            Classification::NeedsJudgment
        );
    }

    #[test]
    fn unknown_top_level_key_is_needs_judgment() {
        let fs = project_findings("mystery_field: 3\n");
        assert_eq!(
            find(&fs, "PROJECT_UNKNOWN_KEY").unwrap().classification,
            Classification::NeedsJudgment
        );
    }

    #[test]
    fn name_that_looks_like_an_id_is_needs_judgment() {
        // A slug-shaped name differing from the file stem `demo`.
        let fs = project_findings("name: other-slug\n");
        assert_eq!(
            find(&fs, "PROJECT_NAME_LOOKS_LIKE_ID").unwrap().classification,
            Classification::NeedsJudgment
        );
        // A free-form label (spaces/caps) is not mistaken for an id.
        let label = project_findings("name: My Nice Project\n");
        assert!(find(&label, "PROJECT_NAME_LOOKS_LIKE_ID").is_none());
    }

    #[test]
    fn workspace_runner_and_role_are_auto_heal() {
        let fs = project_findings(
            "workspaces:\n- name: w1\n  machine: hub\n  runner: claude\n  role: Review\n",
        );
        assert_eq!(
            find(&fs, "WORKSPACE_RUNNER_REMOVED").unwrap().classification,
            Classification::AutoHeal
        );
        assert_eq!(
            find(&fs, "WORKSPACE_ROLE_DEPRECATED").unwrap().classification,
            Classification::AutoHeal
        );
    }

    #[test]
    fn tag_shorthands_on_workspace_and_machine_are_auto_heal() {
        let scalar = project_findings("workspaces:\n- name: w1\n  machine: hub\n  tag: review\n");
        assert!(find(&scalar, "TAG_SHORTHAND_DEPRECATED").is_some());
        let bare = project_findings("machines:\n- name: hub\n  kind: local\n  tags: review\n");
        assert_eq!(
            find(&bare, "TAG_SHORTHAND_DEPRECATED").unwrap().classification,
            Classification::AutoHeal
        );
        // A proper list is not flagged.
        let ok = project_findings("machines:\n- name: hub\n  kind: local\n  tags: [review]\n");
        assert!(find(&ok, "TAG_SHORTHAND_DEPRECATED").is_none());
    }

    #[test]
    fn zen_danger_paths_bare_seq_vs_conflict() {
        let bare = project_findings("zen:\n  danger_paths:\n  - .env\n");
        assert_eq!(
            find(&bare, "ZEN_DANGER_PATHS_BARE_SEQ").unwrap().classification,
            Classification::AutoHeal
        );
        let conflict =
            project_findings("zen:\n  danger_paths:\n    extend: [.env]\n    override: [x]\n");
        assert_eq!(
            find(&conflict, "ZEN_DANGER_PATHS_CONFLICT").unwrap().classification,
            Classification::NeedsJudgment
        );
        // The current single-key form is clean.
        let ok = project_findings("zen:\n  danger_paths:\n    extend: [.env]\n");
        assert!(codes(&ok).iter().all(|c| !c.starts_with("ZEN_DANGER")));
    }

    // ---- statuses --------------------------------------------------------

    #[test]
    fn legacy_status_id_is_auto_heal_but_wire_form_and_canonical_are_clean() {
        let e = entry("project.demo.statuses", "project:demo", SurfaceFormat::Yaml);
        let mut out = Vec::new();
        sniff_statuses(
            &e,
            "statuses:\n- id: wip\n  name: WIP\n  category: active\n",
            &mut out,
        );
        assert_eq!(
            find(&out, "STATUS_ID_LEGACY").unwrap().classification,
            Classification::AutoHeal
        );
        // `in_progress` is the legitimate on-disk wire form — not a deprecation.
        let mut wire = Vec::new();
        sniff_statuses(
            &e,
            "statuses:\n- id: in_progress\n  name: X\n  category: active\n",
            &mut wire,
        );
        assert!(find(&wire, "STATUS_ID_LEGACY").is_none());
        // Canonical kebab id is clean.
        let mut canonical = Vec::new();
        sniff_statuses(
            &e,
            "statuses:\n- id: in-progress\n  name: X\n  category: active\n",
            &mut canonical,
        );
        assert!(find(&canonical, "STATUS_ID_LEGACY").is_none());
    }

    // ---- workflow --------------------------------------------------------

    #[test]
    fn workflow_bare_owner_is_auto_heal_and_named_conflict_is_judgment() {
        let e = entry(
            "project.demo.workflow.task",
            "project:demo",
            SurfaceFormat::Yaml,
        );
        let mut bare = Vec::new();
        sniff_workflow(
            &e,
            "name: task\nstatuses:\n- id: in-progress\n  owner: agent\n",
            &mut bare,
        );
        assert_eq!(
            find(&bare, "WORKFLOW_BARE_OWNER").unwrap().classification,
            Classification::AutoHeal
        );
        let mut conflict = Vec::new();
        sniff_workflow(
            &e,
            "name: task\nstatuses:\n- id: review\n  owner: alice\n  agent: bob\n",
            &mut conflict,
        );
        assert_eq!(
            find(&conflict, "WORKFLOW_OWNER_AGENT_CONFLICT")
                .unwrap()
                .classification,
            Classification::NeedsJudgment
        );
    }

    #[test]
    fn workflow_named_default_is_auto_heal() {
        let e = entry(
            "project.demo.workflow.default",
            "project:demo",
            SurfaceFormat::Yaml,
        );
        let mut out = Vec::new();
        sniff_workflow(&e, "name: default\nstatuses: []\n", &mut out);
        assert_eq!(
            find(&out, "WORKFLOW_DEFAULT_NAME").unwrap().classification,
            Classification::AutoHeal
        );
    }

    // ---- global config.yaml keymap --------------------------------------

    #[test]
    fn legacy_keymap_zen_toggle_is_auto_heal() {
        let e = entry("global.preferences", "global", SurfaceFormat::Yaml);
        let mut out = Vec::new();
        sniff_user_config(&e, "keymap:\n  zen_toggle: ctrl-g\n", &mut out);
        assert_eq!(
            find(&out, "KEYMAP_ZEN_TOGGLE_LEGACY").unwrap().classification,
            Classification::AutoHeal
        );
    }

    // ---- settings JSON ---------------------------------------------------

    #[test]
    fn settings_hook_signatures_are_detected() {
        let e = entry(
            "project.demo.agent.developer.settings",
            "project:demo",
            SurfaceFormat::Json,
        );
        let mut worker = Vec::new();
        sniff_settings_json(&e, "{\"model\": \"{{worker_model}}\"}", &mut worker);
        assert!(find(&worker, "SETTINGS_LEGACY_WORKER_PLACEHOLDER").is_some());
        // The `{{worker_}}` placeholder short-circuits the JSON-shaped checks.
        assert_eq!(worker.len(), 1);

        let mut hooks = Vec::new();
        let unanchored =
            "{\"hooks\":{\"Stop\":[{\"hooks\":[{\"command\":\".shelbi/hooks/stop.sh\"}]}]}}";
        sniff_settings_json(&e, unanchored, &mut hooks);
        assert!(find(&hooks, "SETTINGS_HOOK_PATH_UNANCHORED").is_some());

        let mut unquoted = Vec::new();
        sniff_settings_json(
            &e,
            "{\"command\":\"$CLAUDE_PROJECT_DIR/.shelbi/hooks/stop.sh\"}",
            &mut unquoted,
        );
        assert!(find(&unquoted, "SETTINGS_HOOK_CMD_UNQUOTED").is_some());

        let mut stale = Vec::new();
        sniff_settings_json(&e, "{\"command\":\".shelbi/hooks/claude.stop.sh\"}", &mut stale);
        assert!(find(&stale, "SETTINGS_STALE_CLAUDE_HOOK").is_some());
    }

    #[test]
    fn clean_settings_template_is_a_no_op() {
        let e = entry(
            "project.demo.workspace-settings-template",
            "project:demo",
            SurfaceFormat::Json,
        );
        let mut out = Vec::new();
        sniff_settings_json(
            &e,
            "{\"permissions\":{\"defaultMode\":\"{{workspace_permissions_mode}}\"},\
             \"hooks\":{\"Stop\":[{\"hooks\":[{\"command\":\"\\\"$CLAUDE_PROJECT_DIR/.shelbi/hooks/stop.sh\\\"\"}]}]}}",
            &mut out,
        );
        assert!(out.is_empty(), "clean template flagged: {:?}", codes(&out));
    }

    // ---- detect + emit (home-scoped) ------------------------------------

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-config-upgrade-test-{}-{}",
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
    fn detect_surfaces_legacy_project_and_noop_is_clean() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME"]);
        guard.set("SHELBI_HOME", &home);

        let legacy = "\
name: demo
display_name: My Demo
repo: /tmp/demo
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
        std::fs::write(home.join("projects/demo.yaml"), legacy).unwrap();
        let report = detect(&["demo".to_string()]).unwrap();
        let cs = codes(&report.findings);
        assert!(cs.contains(&"PROJECT_DISPLAY_NAME_DEPRECATED"), "{cs:?}");
        assert!(cs.contains(&"WORKSPACE_RUNNER_REMOVED"), "{cs:?}");
        assert!(cs.contains(&"WORKSPACE_ROLE_DEPRECATED"), "{cs:?}");

        // A fully-current config is a no-op.
        let clean = "\
name: demo
repo: /tmp/demo
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
  tags: [review]
";
        std::fs::write(home.join("projects/demo.yaml"), clean).unwrap();
        let report = detect(&["demo".to_string()]).unwrap();
        assert!(
            report.is_empty(),
            "clean config produced findings: {:?}",
            codes(&report.findings)
        );
    }

    #[test]
    fn emit_writes_findings_file_and_clears_it_when_clean() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME"]);
        guard.set("SHELBI_HOME", &home);

        let report = UpgradeReport {
            schema_version: FINDINGS_SCHEMA_VERSION,
            shelbi_version: "test".into(),
            findings: vec![finding(
                &reg_entry(),
                Classification::NeedsJudgment,
                "PROJECT_UNKNOWN_KEY",
                "unknown key",
                "decide",
                Location { line: 1, column: 1 },
            )],
        };
        emit(&report).unwrap();
        let path = findings_path().unwrap();
        let body = std::fs::read_to_string(&path).expect("findings file written");
        assert!(body.contains("PROJECT_UNKNOWN_KEY"));

        // A subsequent clean pass removes the stale file.
        emit(&empty_report()).unwrap();
        assert!(!path.exists(), "stale findings file not cleared");
    }

    // ---- slice 3: needs-judgment channel --------------------------------

    #[test]
    fn needs_judgment_findings_carry_stable_id_and_rationale() {
        // A needs-judgment finding gets a non-empty id + rationale; an auto-heal
        // finding gets an id but an empty rationale (it carries no such question).
        let nj = project_findings("mystery_field: 3\n");
        let f = find(&nj, "PROJECT_UNKNOWN_KEY").expect("nj finding");
        assert_eq!(f.classification, Classification::NeedsJudgment);
        assert!(f.id.starts_with("cfg-"), "id: {}", f.id);
        assert!(!f.rationale.is_empty(), "nj finding missing rationale");

        let ah = project_findings("workers:\n- name: w1\n  machine: hub\n");
        let g = find(&ah, "PROJECT_WORKERS_KEY_DEPRECATED").expect("auto-heal finding");
        assert_eq!(g.classification, Classification::AutoHeal);
        assert!(g.id.starts_with("cfg-"), "id: {}", g.id);
        assert!(g.rationale.is_empty(), "auto-heal finding should carry no rationale");

        // The same finding hashes to the same id on a second detection (stable
        // across restarts → the channel never accumulates duplicates).
        let again = project_findings("mystery_field: 3\n");
        assert_eq!(find(&again, "PROJECT_UNKNOWN_KEY").unwrap().id, f.id);
    }

    #[test]
    fn on_start_pass_channels_needs_judgment_and_never_applies_it() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        // A config with BOTH an auto-heal form (workspace `runner:`) and a
        // needs-judgment one (zen.danger_paths extend+override conflict).
        let legacy = "\
name: demo
repo: /tmp/demo
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
zen:
  danger_paths:
    extend:
    - .env
    override:
    - secrets/**
";
        let path = home.join("projects/demo.yaml");
        std::fs::write(&path, legacy).unwrap();

        let outcome = run_for_project("demo");
        // The auto-heal ran...
        assert!(
            outcome.applied.iter().any(|c| c.code == "WORKSPACE_RUNNER_REMOVED"),
            "auto-heal did not run: {:?}",
            outcome.applied
        );
        // ...but the needs-judgment finding was NOT applied.
        assert!(
            !outcome.applied.iter().any(|c| c.code == "ZEN_DANGER_PATHS_CONFLICT"),
            "needs-judgment finding was auto-applied"
        );

        // On disk: runner gone, but the danger_paths conflict is untouched.
        let healed: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let ws = &healed.as_mapping().unwrap().get(Value::String("workspaces".into())).unwrap()
            .as_sequence().unwrap()[0];
        assert!(ws.as_mapping().unwrap().get(Value::String("runner".into())).is_none());
        let dp = get(get(&healed, "zen").unwrap(), "danger_paths").unwrap();
        let dpm = dp.as_mapping().unwrap();
        assert!(dpm.contains_key(Value::String("extend".into())));
        assert!(dpm.contains_key(Value::String("override".into())), "conflict was resolved on start");

        // The channel file carries the needs-judgment finding with id + rationale.
        let channel = load_findings().unwrap().expect("channel written on start");
        let nj = needs_judgment_report(&channel);
        let f = nj
            .findings
            .iter()
            .find(|f| f.code == "ZEN_DANGER_PATHS_CONFLICT")
            .expect("conflict not in channel");
        assert_eq!(f.classification, Classification::NeedsJudgment);
        assert!(f.id.starts_with("cfg-"));
        assert!(!f.rationale.is_empty());
        assert!(f.proposed_fix.contains("extend"));
    }

    #[test]
    fn no_needs_judgment_leaves_an_empty_channel() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let guard = EnvGuard::new(&["SHELBI_HOME", "SHELBI_HUB_SOCK"]);
        guard.set("SHELBI_HOME", &home);
        guard.remove("SHELBI_HUB_SOCK");

        // Only an auto-heal form (workspace `runner:`); no needs-judgment.
        let legacy = "\
name: demo
repo: /tmp/demo
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
";
        std::fs::write(home.join("projects/demo.yaml"), legacy).unwrap();
        run_for_project("demo");

        // The auto-heal fully resolved the config, so no channel file lingers.
        assert!(
            load_findings().unwrap().is_none(),
            "clean-after-heal config left a stale channel"
        );

        // And a config that is clean from the start writes no channel at all.
        std::fs::write(
            home.join("projects/demo.yaml"),
            "name: demo\nrepo: /tmp/demo\nmachines:\n- name: hub\n  kind: local\n  work_dir: /tmp/demo\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\nworkspaces:\n- name: w1\n  machine: hub\n",
        )
        .unwrap();
        run_for_project("demo");
        let nj = load_findings()
            .unwrap()
            .map(|r| needs_judgment_report(&r))
            .unwrap_or_else(empty_report);
        assert!(nj.findings.is_empty(), "clean config surfaced needs-judgment");
    }
}
