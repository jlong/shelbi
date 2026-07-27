//! Versioned discovery and read-only validation for every Shelbi-owned
//! configuration surface.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use shelbi_core::{
    validate_project_name, ConfigMode, Project, ProjectStatuses, Workflow, LOCAL_PROJECT_FIELDS,
    SHARED_PROJECT_FIELDS,
};
use shelbi_state::keymap::{
    validate_keymaps_yaml, ErrorKind as KeymapErrorKind, KeymapDiagnostic,
    WarningKind as KeymapWarningKind,
};
use shelbi_state::{
    hub_config_path, projects_dir, shelbi_home, user_config_path, HubConfig, UserConfig,
    DEFAULT_AGENTS,
};

pub const INVENTORY_SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = ".shelbi-config-inventory.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub schema_version: u32,
    pub shelbi_version: String,
    pub staged_dir: PathBuf,
    pub entries: Vec<InventoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub logical_id: String,
    pub scope: String,
    pub canonical_path: PathBuf,
    pub candidate_path: PathBuf,
    pub format: SurfaceFormat,
    pub exists: bool,
    pub lifecycle_owned: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceFormat {
    Yaml,
    Json,
    Markdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub logical_id: String,
    pub file: PathBuf,
    pub location: Location,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct LintReport {
    pub schema_version: u32,
    pub shelbi_version: String,
    pub diagnostics: Vec<Diagnostic>,
    pub warnings: usize,
    pub errors: usize,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

pub fn discover_project_names() -> Result<Vec<String>> {
    let dir = projects_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|v| v.to_str()) == Some("yaml") {
            if let Some(stem) = path.file_stem().and_then(|v| v.to_str()) {
                names.insert(stem.to_string());
            }
        } else if path.is_dir() && path.join("local.yaml").is_file() {
            if let Some(name) = path.file_name().and_then(|v| v.to_str()) {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

pub fn inventory(projects: &[String]) -> Result<Inventory> {
    let staged_dir = unique_stage_dir();
    fs::create_dir_all(&staged_dir)
        .with_context(|| format!("creating staged inventory {}", staged_dir.display()))?;
    let mut entries = collect_entries(projects)?;
    entries.sort_by(|a, b| a.logical_id.cmp(&b.logical_id));

    for entry in &entries {
        if !entry.exists {
            continue;
        }
        let target = staged_dir.join(&entry.candidate_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&entry.canonical_path, &target).with_context(|| {
            format!(
                "materializing {} from {}",
                entry.logical_id,
                entry.canonical_path.display()
            )
        })?;
    }

    let inventory = Inventory {
        schema_version: INVENTORY_SCHEMA_VERSION,
        shelbi_version: env!("CARGO_PKG_VERSION").to_string(),
        staged_dir: staged_dir.clone(),
        entries,
    };
    fs::write(
        staged_dir.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    Ok(inventory)
}

pub fn lint_live(projects: &[String]) -> Result<LintReport> {
    let inventory = inventory(projects)?;
    let result = lint_inventory(&inventory, None);
    let _ = fs::remove_dir_all(&inventory.staged_dir);
    result
}

pub fn lint_staged(staged: &Path, selected_projects: Option<&[String]>) -> Result<LintReport> {
    let manifest_path = staged.join(MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "{} is not a Shelbi staged configuration inventory",
            staged.display()
        )
    })?;
    let mut inventory: Inventory = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    if inventory.schema_version != INVENTORY_SCHEMA_VERSION {
        bail!(
            "unsupported staged inventory schema version {} (this shelbi supports {})",
            inventory.schema_version,
            INVENTORY_SCHEMA_VERSION
        );
    }
    // The directory supplied by the caller is authoritative. This keeps a
    // copied/moved candidate snapshot portable instead of pinning it to the
    // temporary directory where inventory first created it.
    inventory.staged_dir = staged.to_path_buf();
    for entry in &mut inventory.entries {
        entry.canonical_path = checked_candidate(staged, &entry.candidate_path)?;
    }
    augment_staged_entries(staged, &mut inventory.entries)?;
    lint_inventory(&inventory, selected_projects)
}

fn augment_staged_entries(stage: &Path, entries: &mut Vec<InventoryEntry>) -> Result<()> {
    let projects: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| entry.scope.strip_prefix("project:").map(str::to_string))
        .collect();
    let mut known: HashSet<PathBuf> = entries
        .iter()
        .map(|entry| entry.candidate_path.clone())
        .collect();
    for project in projects {
        let scope = format!("project:{project}");
        let project_rel = PathBuf::from("projects").join(&project);
        let workflow_dir = stage.join(&project_rel).join("workflows");
        if let Ok(files) = fs::read_dir(&workflow_dir) {
            for file in files.flatten() {
                let path = file.path();
                if !path.is_file()
                    || path.extension().and_then(|v| v.to_str()) != Some("yaml")
                    || path.file_name().and_then(|v| v.to_str()) == Some("statuses.yaml")
                {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|v| v.to_str()) else {
                    continue;
                };
                let candidate = project_rel.join(format!("workflows/{stem}.yaml"));
                if known.insert(candidate.clone()) {
                    entries.push(entry(
                        &format!("project.{project}.workflow.{stem}"),
                        &scope,
                        path,
                        candidate,
                        SurfaceFormat::Yaml,
                        false,
                    ));
                }
            }
        }

        let agents_dir = stage.join(&project_rel).join("agents");
        if let Ok(agents) = fs::read_dir(&agents_dir) {
            for agent in agents.flatten() {
                let agent_path = agent.path();
                let Some(name) = agent.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !agent_path.is_dir() || name.starts_with(['.', '_']) {
                    continue;
                }
                for (suffix, filename, format) in [
                    ("instructions", "instructions.md", SurfaceFormat::Markdown),
                    ("settings", "settings.json", SurfaceFormat::Json),
                ] {
                    let candidate = project_rel.join(format!("agents/{name}/{filename}"));
                    let path = stage.join(&candidate);
                    if path.is_file() && known.insert(candidate.clone()) {
                        entries.push(entry(
                            &format!("project.{project}.agent.{name}.{suffix}"),
                            &scope,
                            path,
                            candidate,
                            format,
                            false,
                        ));
                    }
                }
                collect_staged_skills(
                    stage,
                    &project,
                    &scope,
                    &name,
                    &project_rel,
                    &mut known,
                    entries,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_staged_skills(
    stage: &Path,
    project: &str,
    scope: &str,
    agent: &str,
    project_rel: &Path,
    known: &mut HashSet<PathBuf>,
    entries: &mut Vec<InventoryEntry>,
) -> Result<()> {
    let skills_rel = project_rel.join(format!("agents/{agent}/skills"));
    let skills = stage.join(&skills_rel);
    if !skills.exists() {
        return Ok(());
    }
    let mut pending = vec![skills];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(&dir)? {
            let item = item?;
            let path = item.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if !path.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("md")
            {
                continue;
            }
            let candidate = path
                .strip_prefix(stage)
                .context("staged skill escaped candidate directory")?
                .to_path_buf();
            if !known.insert(candidate.clone()) {
                continue;
            }
            let rel = path.strip_prefix(stage.join(&skills_rel)).unwrap_or(&path);
            let id_suffix = rel
                .to_string_lossy()
                .replace(['/', '\\'], ".")
                .trim_end_matches(".md")
                .to_string();
            entries.push(entry(
                &format!("project.{project}.agent.{agent}.skill.{id_suffix}"),
                scope,
                path,
                candidate,
                SurfaceFormat::Markdown,
                false,
            ));
        }
    }
    Ok(())
}

fn unique_stage_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("shelbi-config-{}-{nanos}", std::process::id()))
}

fn collect_entries(projects: &[String]) -> Result<Vec<InventoryEntry>> {
    let home = absolute(shelbi_home()?)?;
    let mut out = vec![
        entry(
            "global.preferences",
            "global",
            absolute(user_config_path()?)?,
            "global/config.yaml",
            SurfaceFormat::Yaml,
            true,
        ),
        entry(
            "global.keybindings",
            "global",
            home.join("keys.yaml"),
            "global/keys.yaml",
            SurfaceFormat::Yaml,
            false,
        ),
        entry(
            "global.hub",
            "global",
            absolute(hub_config_path()?)?,
            "global/shelbi.yaml",
            SurfaceFormat::Yaml,
            true,
        ),
    ];

    for project in projects {
        validate_project_name(project)
            .with_context(|| format!("invalid project id `{project}` in inventory selection"))?;
        collect_project_entries(project, &home, &mut out)?;
    }
    Ok(out)
}

fn collect_project_entries(
    project: &str,
    home: &Path,
    out: &mut Vec<InventoryEntry>,
) -> Result<()> {
    let scope = format!("project:{project}");
    let candidate_root = PathBuf::from("projects").join(project);
    let global_path = home.join("projects").join(format!("{project}.yaml"));
    let local_path = home.join("projects").join(project).join("local.yaml");

    let (config_root, parsed, split) = if global_path.is_file() {
        out.push(entry(
            &format!("project.{project}.registration"),
            &scope,
            global_path.clone(),
            candidate_root.join("registration.yaml"),
            SurfaceFormat::Yaml,
            true,
        ));
        let parsed = fs::read_to_string(&global_path)
            .ok()
            .and_then(|s| Project::from_yaml_str(&s).ok());
        let root = parsed
            .as_ref()
            .and_then(|p| p.workspace_settings_template.as_ref())
            .and_then(|_| global_path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| home.join("projects").join(project));
        (root, parsed, false)
    } else {
        out.push(entry(
            &format!("project.{project}.registration.local"),
            &scope,
            local_path.clone(),
            candidate_root.join("local.yaml"),
            SurfaceFormat::Yaml,
            true,
        ));
        let local_text = fs::read_to_string(&local_path).ok();
        let repo = local_text
            .as_deref()
            .and_then(yaml_repo)
            .map(|repo| shelbi_state::expand_tilde_str(&repo));
        let shared_path = repo
            .as_ref()
            .map(|r| r.join(".shelbi/project.yaml"))
            .unwrap_or_else(|| home.join("projects").join(project).join("project.yaml"));
        out.push(entry(
            &format!("project.{project}.registration.shared"),
            &scope,
            absolute(shared_path.clone())?,
            candidate_root.join("project.yaml"),
            SurfaceFormat::Yaml,
            true,
        ));
        let shared_text = fs::read_to_string(&shared_path).ok();
        let parsed = match (shared_text.as_deref(), local_text.as_deref()) {
            (Some(shared), Some(local)) => Project::from_split_yaml_str(shared, local).ok(),
            _ => None,
        };
        (
            repo.map(|r| r.join(".shelbi"))
                .unwrap_or_else(|| home.join("projects").join(project)),
            parsed,
            true,
        )
    };
    let config_root = absolute(config_root)?;

    let template_path = parsed
        .as_ref()
        .and_then(|p| p.workspace_settings_template.clone())
        .map(|p| shelbi_state::expand_tilde_path(&p))
        .unwrap_or_else(|| config_root.join("workspace-settings.json.template"));
    out.push(entry(
        &format!("project.{project}.workspace-settings-template"),
        &scope,
        absolute(template_path)?,
        candidate_root.join("workspace-settings.json.template"),
        SurfaceFormat::Json,
        true,
    ));
    out.push(entry(
        &format!("project.{project}.zenmode"),
        &scope,
        config_root.join("zenmode.md"),
        candidate_root.join("zenmode.md"),
        SurfaceFormat::Markdown,
        false,
    ));

    let workflows = config_root.join("workflows");
    out.push(entry(
        &format!("project.{project}.statuses"),
        &scope,
        workflows.join("statuses.yaml"),
        candidate_root.join("workflows/statuses.yaml"),
        SurfaceFormat::Yaml,
        false,
    ));
    let mut workflow_names = BTreeSet::from(["task".to_string(), "subtask".to_string()]);
    if let Ok(files) = fs::read_dir(&workflows) {
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|v| v.to_str()) == Some("yaml")
                && path.file_name().and_then(|v| v.to_str()) != Some("statuses.yaml")
            {
                if let Some(stem) = path.file_stem().and_then(|v| v.to_str()) {
                    workflow_names.insert(stem.to_string());
                }
            }
        }
    }
    for name in workflow_names {
        out.push(entry(
            &format!("project.{project}.workflow.{name}"),
            &scope,
            workflows.join(format!("{name}.yaml")),
            candidate_root.join(format!("workflows/{name}.yaml")),
            SurfaceFormat::Yaml,
            false,
        ));
    }

    let agents = config_root.join("agents");
    out.push(entry(
        &format!("project.{project}.agents.shared-preamble"),
        &scope,
        agents.join("_shared/preamble.md"),
        candidate_root.join("agents/_shared/preamble.md"),
        SurfaceFormat::Markdown,
        false,
    ));
    let mut agent_names: BTreeSet<String> =
        DEFAULT_AGENTS.iter().map(|a| a.name.to_string()).collect();
    if let Ok(dirs) = fs::read_dir(&agents) {
        for dir in dirs.flatten() {
            if dir.path().is_dir() {
                if let Some(name) = dir.file_name().to_str() {
                    if !name.starts_with(['.', '_']) {
                        agent_names.insert(name.to_string());
                    }
                }
            }
        }
    }
    for agent in agent_names {
        let agent_root = agents.join(&agent);
        let lifecycle_owned = DEFAULT_AGENTS.iter().any(|a| a.name == agent);
        out.push(entry(
            &format!("project.{project}.agent.{agent}.instructions"),
            &scope,
            agent_root.join("instructions.md"),
            candidate_root.join(format!("agents/{agent}/instructions.md")),
            SurfaceFormat::Markdown,
            lifecycle_owned,
        ));
        out.push(entry(
            &format!("project.{project}.agent.{agent}.settings"),
            &scope,
            agent_root.join("settings.json"),
            candidate_root.join(format!("agents/{agent}/settings.json")),
            SurfaceFormat::Json,
            lifecycle_owned,
        ));
        collect_skill_entries(
            project,
            &scope,
            &agent,
            &agent_root.join("skills"),
            &candidate_root.join(format!("agents/{agent}/skills")),
            out,
        )?;
        if agent == "review" {
            let expected = agent_root.join("skills/load-run-detection/SKILL.md");
            if !out.iter().any(|e| e.canonical_path == expected) {
                out.push(entry(
                    &format!("project.{project}.agent.review.skill.load-run-detection.SKILL"),
                    &scope,
                    expected,
                    candidate_root.join("agents/review/skills/load-run-detection/SKILL.md"),
                    SurfaceFormat::Markdown,
                    true,
                ));
            }
        }
    }

    // Preserve the layout fact in the manifest even though it is derivable
    // from the registration entries. This assertion catches accidental
    // collection drift during development.
    debug_assert_eq!(
        split,
        out.iter()
            .any(|e| e.logical_id == format!("project.{project}.registration.shared"))
    );
    Ok(())
}

fn collect_skill_entries(
    project: &str,
    scope: &str,
    agent: &str,
    root: &Path,
    candidate_root: &Path,
    out: &mut Vec<InventoryEntry>,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut pending = vec![(root.to_path_buf(), candidate_root.to_path_buf())];
    while let Some((dir, candidate_dir)) = pending.pop() {
        for item in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let item = item?;
            let path = item.path();
            let candidate = candidate_dir.join(item.file_name());
            if path.is_dir() {
                pending.push((path, candidate));
            } else if path.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("md")
            {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let id_suffix = rel
                    .to_string_lossy()
                    .replace(['/', '\\'], ".")
                    .trim_end_matches(".md")
                    .to_string();
                out.push(entry(
                    &format!("project.{project}.agent.{agent}.skill.{id_suffix}"),
                    scope,
                    path,
                    candidate,
                    SurfaceFormat::Markdown,
                    false,
                ));
            }
        }
    }
    Ok(())
}

fn entry(
    logical_id: &str,
    scope: &str,
    canonical_path: impl Into<PathBuf>,
    candidate_path: impl Into<PathBuf>,
    format: SurfaceFormat,
    lifecycle_owned: bool,
) -> InventoryEntry {
    let canonical_path = canonical_path.into();
    InventoryEntry {
        logical_id: logical_id.to_string(),
        scope: scope.to_string(),
        exists: canonical_path.is_file(),
        canonical_path,
        candidate_path: candidate_path.into(),
        format,
        lifecycle_owned,
    }
}

fn yaml_repo(text: &str) -> Option<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    value
        .as_mapping()?
        .get(serde_yaml::Value::String("repo".into()))?
        .as_str()
        .map(str::to_string)
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn lint_inventory(
    inventory: &Inventory,
    selected_projects: Option<&[String]>,
) -> Result<LintReport> {
    let stage = &inventory.staged_dir;
    let mut entries: Vec<&InventoryEntry> = inventory
        .entries
        .iter()
        .filter(|entry| {
            selected_projects.map_or(true, |projects| {
                entry.scope == "global"
                    || entry
                        .scope
                        .strip_prefix("project:")
                        .is_some_and(|p| projects.iter().any(|wanted| wanted == p))
            })
        })
        .collect();
    entries.sort_by(|a, b| a.logical_id.cmp(&b.logical_id));
    let mut diagnostics = Vec::new();
    let mut texts = HashMap::new();
    for entry in &entries {
        let path = checked_candidate(stage, &entry.candidate_path)?;
        match fs::read_to_string(&path) {
            Ok(text) => {
                texts.insert(entry.logical_id.clone(), text);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => diagnostics.push(diag(
                entry,
                Severity::Error,
                "CONFIG_FILE_UNREADABLE",
                format!("could not read candidate: {e}"),
                None,
                Location { line: 1, column: 1 },
            )),
        }
    }

    lint_global(&entries, &texts, &mut diagnostics);
    let project_names: BTreeSet<String> = entries
        .iter()
        .filter_map(|e| e.scope.strip_prefix("project:").map(str::to_string))
        .collect();
    for project in project_names {
        lint_project(&project, &entries, &texts, &mut diagnostics);
    }
    diagnostics.sort_by(|a, b| {
        (&a.file, a.location.line, a.location.column, &a.code).cmp(&(
            &b.file,
            b.location.line,
            b.location.column,
            &b.code,
        ))
    });
    let warnings = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    let errors = diagnostics.len() - warnings;
    Ok(LintReport {
        schema_version: INVENTORY_SCHEMA_VERSION,
        shelbi_version: env!("CARGO_PKG_VERSION").to_string(),
        diagnostics,
        warnings,
        errors,
    })
}

fn checked_candidate(stage: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!(
            "unsafe candidate path in staged manifest: {}",
            relative.display()
        );
    }
    Ok(stage.join(relative))
}

fn lint_global(
    entries: &[&InventoryEntry],
    texts: &HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    if let Some(entry) = find_entry(entries, "global.preferences") {
        if let Some(text) = texts.get(&entry.logical_id) {
            lint_typed_yaml::<UserConfig>(entry, text, out);
        }
    }
    if let Some(entry) = find_entry(entries, "global.hub") {
        if let Some(text) = texts.get(&entry.logical_id) {
            lint_typed_yaml::<HubConfig>(entry, text, out);
        }
    }
    if let Some(entry) = find_entry(entries, "global.keybindings") {
        if let Some(text) = texts.get(&entry.logical_id) {
            let projects: Vec<&str> = entries
                .iter()
                .filter_map(|entry| entry.scope.strip_prefix("project:"))
                .collect();
            lint_keybindings(entry, text, &projects, out);
        }
    }
}

fn lint_project(
    project: &str,
    entries: &[&InventoryEntry],
    texts: &HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    let global_id = format!("project.{project}.registration");
    let local_id = format!("project.{project}.registration.local");
    let shared_id = format!("project.{project}.registration.shared");
    let registration = if let Some(entry) = find_entry(entries, &global_id) {
        match texts.get(&global_id) {
            Some(text) => {
                lint_unknown_yaml::<Project>(entry, text, out);
                match Project::from_yaml_str(text) {
                    Ok(mut p) => {
                        p.name = project.to_string();
                        if p.config_mode == Some(ConfigMode::InRepo) {
                            out.push(diag(
                                entry,
                                Severity::Error,
                                "PROJECT_LAYOUT_INCONSISTENT",
                                "a flat global registration cannot declare config_mode: in-repo"
                                    .into(),
                                Some(
                                    "Run the project config migration command to create the split layout."
                                        .into(),
                                ),
                                locate_key(text, "config_mode"),
                            ));
                        }
                        match p.validate_workspaces() {
                            Ok(()) => Some(p),
                            Err(e) => {
                                out.push(semantic(entry, "PROJECT_REFERENCE_INVALID", e, None));
                                None
                            }
                        }
                    }
                    Err(e) => {
                        push_parse(entry, "PROJECT_INVALID", e, out);
                        None
                    }
                }
            }
            None => {
                out.push(missing(entry, "PROJECT_REGISTRATION_MISSING"));
                None
            }
        }
    } else {
        let local_entry = find_entry(entries, &local_id);
        let shared_entry = find_entry(entries, &shared_id);
        match (
            local_entry,
            shared_entry,
            texts.get(&local_id),
            texts.get(&shared_id),
        ) {
            (Some(local_entry), Some(shared_entry), Some(local), Some(shared)) => {
                lint_split_unknown(local_entry, local, LOCAL_PROJECT_FIELDS, out);
                lint_split_unknown(shared_entry, shared, SHARED_PROJECT_FIELDS, out);
                lint_split_nested_unknown(shared_entry, local_entry, shared, local, out);
                match Project::from_split_yaml_str(shared, local) {
                    Ok(mut p) => {
                        p.name = project.to_string();
                        if p.config_mode != Some(ConfigMode::InRepo) {
                            out.push(diag(
                                shared_entry,
                                Severity::Error,
                                "PROJECT_LAYOUT_INCONSISTENT",
                                "a split registration must declare config_mode: in-repo in project.yaml"
                                    .into(),
                                Some("Set config_mode: in-repo in the shared project.yaml.".into()),
                                locate_key(shared, "config_mode"),
                            ));
                        }
                        match p.validate_workspaces() {
                            Ok(()) => Some(p),
                            Err(e) => {
                                out.push(semantic(
                                    shared_entry,
                                    "PROJECT_REFERENCE_INVALID",
                                    e,
                                    None,
                                ));
                                None
                            }
                        }
                    }
                    Err(e) => {
                        out.push(semantic(
                            shared_entry,
                            "PROJECT_SPLIT_INCONSISTENT",
                            e,
                            Some(
                                "Keep shared fields in project.yaml and machine-local fields in local.yaml."
                                    .into(),
                            ),
                        ));
                        None
                    }
                }
            }
            (Some(local_entry), _, None, _) => {
                out.push(missing(local_entry, "PROJECT_LOCAL_MISSING"));
                None
            }
            (_, Some(shared_entry), _, None) => {
                out.push(missing(shared_entry, "PROJECT_SHARED_MISSING"));
                None
            }
            _ => None,
        }
    };

    let statuses_id = format!("project.{project}.statuses");
    let statuses = find_entry(entries, &statuses_id).and_then(|entry| {
        texts.get(&statuses_id).and_then(|text| {
            lint_unknown_yaml::<ProjectStatuses>(entry, text, out);
            match ProjectStatuses::from_yaml_str(text) {
                Ok(statuses) => {
                    for warning in statuses.category_warnings() {
                        out.push(diag(
                            entry,
                            Severity::Warning,
                            "STATUSES_CATEGORY_WARNING",
                            warning,
                            None,
                            Location { line: 1, column: 1 },
                        ));
                    }
                    Some(statuses)
                }
                Err(e) => {
                    push_parse(entry, "STATUSES_INVALID", e, out);
                    None
                }
            }
        })
    });

    let available_agents: HashSet<String> = entries
        .iter()
        .filter_map(|e| {
            let prefix = format!("project.{project}.agent.");
            let suffix = e.logical_id.strip_prefix(&prefix)?;
            let name = suffix.strip_suffix(".instructions")?;
            texts.contains_key(&e.logical_id).then(|| name.to_string())
        })
        .collect();
    let mut workflow_names = HashMap::<String, String>::new();
    for entry in entries.iter().filter(|e| {
        e.logical_id
            .starts_with(&format!("project.{project}.workflow."))
    }) {
        let Some(text) = texts.get(&entry.logical_id) else {
            continue;
        };
        lint_unknown_yaml::<Workflow>(entry, text, out);
        if let Ok(inline) = Workflow::inline_identity_fields(text) {
            if !inline.is_empty() {
                out.push(diag(
                    entry,
                    Severity::Error,
                    "WORKFLOW_INLINE_STATUS_IDENTITY",
                    "workflow status entries must keep name/category in workflows/statuses.yaml"
                        .into(),
                    Some("Remove inline name/category fields from this workflow.".into()),
                    Location { line: 1, column: 1 },
                ));
            }
        }
        match Workflow::from_yaml_str_with_diagnostics(text) {
            Ok((workflow, warnings)) => {
                for warning in warnings {
                    out.push(diag(
                        entry,
                        Severity::Warning,
                        "WORKFLOW_DEPRECATED",
                        warning,
                        None,
                        Location { line: 1, column: 1 },
                    ));
                }
                let stem = entry
                    .canonical_path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or("");
                if workflow.name != stem {
                    out.push(diag(
                        entry,
                        Severity::Warning,
                        "WORKFLOW_NAME_MISMATCH",
                        format!(
                            "workflow declares name `{}` but its file is `{stem}.yaml`",
                            workflow.name
                        ),
                        Some(format!("Rename the file or set name: {stem}.")),
                        Location { line: 1, column: 1 },
                    ));
                }
                if let Some(previous) =
                    workflow_names.insert(workflow.name.clone(), entry.logical_id.clone())
                {
                    out.push(diag(
                        entry,
                        Severity::Error,
                        "WORKFLOW_NAME_DUPLICATE",
                        format!(
                            "workflow name `{}` is also declared by {previous}",
                            workflow.name
                        ),
                        None,
                        Location { line: 1, column: 1 },
                    ));
                }
                if let Some(statuses) = &statuses {
                    match workflow.resolve_against(statuses) {
                        Ok(resolved) => {
                            for status in &resolved.statuses {
                                if let Some(agent) = &status.agent {
                                    if !available_agents.contains(agent) {
                                        out.push(diag(
                                            entry,
                                            Severity::Error,
                                            "WORKFLOW_AGENT_UNKNOWN",
                                            format!(
                                                "workflow status `{}` references unknown agent `{agent}`",
                                                status.id
                                            ),
                                            Some(format!(
                                                "Create agents/{agent}/instructions.md or choose an existing agent."
                                            )),
                                            Location { line: 1, column: 1 },
                                        ));
                                    }
                                }
                            }
                        }
                        Err(e) => out.push(semantic(
                            entry,
                            "WORKFLOW_STATUS_REFERENCE_INVALID",
                            e,
                            None,
                        )),
                    }
                } else {
                    out.push(diag(
                        entry,
                        Severity::Error,
                        "WORKFLOW_STATUSES_MISSING",
                        "workflow cannot be resolved without workflows/statuses.yaml".into(),
                        Some("Restore or create workflows/statuses.yaml.".into()),
                        Location { line: 1, column: 1 },
                    ));
                }
            }
            Err(e) => push_parse(entry, "WORKFLOW_INVALID", e, out),
        }
    }

    if let Some(project_config) = registration {
        if let Some(default) = project_config.default_workflow {
            let expected = format!("project.{project}.workflow.{default}");
            let legacy_alias_exists = default == "default"
                && texts.contains_key(&format!("project.{project}.workflow.task"));
            if !(texts.contains_key(&expected) || legacy_alias_exists) {
                if let Some(entry) =
                    find_entry(entries, &global_id).or_else(|| find_entry(entries, &shared_id))
                {
                    out.push(diag(
                        entry,
                        Severity::Error,
                        "PROJECT_DEFAULT_WORKFLOW_MISSING",
                        format!("default_workflow `{default}` has no workflow file"),
                        Some(format!(
                            "Create workflows/{default}.yaml or change default_workflow."
                        )),
                        Location { line: 1, column: 1 },
                    ));
                }
            }
        }
    }

    for entry in entries
        .iter()
        .filter(|e| e.scope == format!("project:{project}"))
    {
        let Some(text) = texts.get(&entry.logical_id) else {
            if entry.logical_id.ends_with(".instructions") && entry.lifecycle_owned {
                out.push(diag(
                    entry,
                    Severity::Warning,
                    "AGENT_INSTRUCTIONS_MISSING",
                    "bundled agent instructions are missing".into(),
                    Some("Run `shelbi reload` to restore lifecycle-owned agent assets.".into()),
                    Location { line: 1, column: 1 },
                ));
            }
            continue;
        };
        match entry.format {
            SurfaceFormat::Json => lint_json_surface(entry, text, out),
            SurfaceFormat::Markdown => lint_markdown_surface(entry, text, out),
            SurfaceFormat::Yaml => {}
        }
    }
}

fn lint_typed_yaml<T: DeserializeOwned>(
    entry: &InventoryEntry,
    text: &str,
    out: &mut Vec<Diagnostic>,
) {
    lint_unknown_yaml::<T>(entry, text, out);
    if let Err(e) = serde_yaml::from_str::<T>(text) {
        push_yaml_error(entry, "CONFIG_YAML_SYNTAX", &e, out);
    }
}

fn lint_unknown_yaml<T: DeserializeOwned>(
    entry: &InventoryEntry,
    text: &str,
    out: &mut Vec<Diagnostic>,
) {
    let deserializer = serde_yaml::Deserializer::from_str(text);
    let mut unknown = Vec::new();
    let result: std::result::Result<T, _> =
        serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()));
    if result.is_err() {
        return;
    }
    for path in unknown {
        let leaf = path.rsplit('.').next().unwrap_or(&path);
        out.push(diag(
            entry,
            Severity::Error,
            "CONFIG_UNKNOWN_FIELD",
            format!("unknown configuration field `{path}`"),
            Some(
                "Remove the field or check the configuration reference for this Shelbi version."
                    .into(),
            ),
            locate_key(text, leaf),
        ));
    }
}

fn lint_split_unknown(
    entry: &InventoryEntry,
    text: &str,
    allowed: &[&str],
    out: &mut Vec<Diagnostic>,
) {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return;
    };
    let Some(mapping) = value.as_mapping() else {
        return;
    };
    for key in mapping.keys().filter_map(|v| v.as_str()) {
        if !allowed.contains(&key) {
            out.push(diag(
                entry,
                Severity::Error,
                "CONFIG_UNKNOWN_FIELD",
                format!("unknown or misplaced project field `{key}`"),
                Some("Move the field to the correct split half or remove it.".into()),
                locate_key(text, key),
            ));
        }
    }
}

fn lint_split_nested_unknown(
    shared_entry: &InventoryEntry,
    local_entry: &InventoryEntry,
    shared: &str,
    local: &str,
    out: &mut Vec<Diagnostic>,
) {
    let (Ok(shared_value), Ok(local_value)) = (
        serde_yaml::from_str::<serde_yaml::Value>(shared),
        serde_yaml::from_str::<serde_yaml::Value>(local),
    ) else {
        return;
    };
    let (Some(shared_map), Some(local_map)) = (shared_value.as_mapping(), local_value.as_mapping())
    else {
        return;
    };
    let mut merged = shared_map.clone();
    for (key, value) in local_map {
        if !merged.contains_key(key) {
            merged.insert(key.clone(), value.clone());
        }
    }
    let Ok(text) = serde_yaml::to_string(&serde_yaml::Value::Mapping(merged)) else {
        return;
    };
    let deserializer = serde_yaml::Deserializer::from_str(&text);
    let mut unknown = Vec::new();
    let result: std::result::Result<Project, _> =
        serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()));
    if result.is_err() {
        return;
    }
    for path in unknown {
        // Top-level unknowns were already diagnosed by `lint_split_unknown`,
        // which can provide the exact source-half location.
        if !path.contains('.') && !path.contains('[') {
            continue;
        }
        let root = path.split(['.', '[']).next().unwrap_or(path.as_str());
        let leaf = path
            .rsplit(['.', '['])
            .next()
            .unwrap_or(path.as_str())
            .trim_end_matches(']');
        let (entry, source) = if LOCAL_PROJECT_FIELDS.contains(&root) {
            (local_entry, local)
        } else {
            (shared_entry, shared)
        };
        out.push(diag(
            entry,
            Severity::Error,
            "CONFIG_UNKNOWN_FIELD",
            format!("unknown configuration field `{path}`"),
            Some(
                "Remove the field or check the configuration reference for this Shelbi version."
                    .into(),
            ),
            locate_key(source, leaf),
        ));
    }
}

fn lint_keybindings(
    entry: &InventoryEntry,
    text: &str,
    projects: &[&str],
    out: &mut Vec<Diagnostic>,
) {
    let value: serde_yaml::Value = match serde_yaml::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            push_yaml_error(entry, "KEYBINDINGS_YAML_SYNTAX", &e, out);
            return;
        }
    };
    let Some(root) = value.as_mapping() else {
        out.push(diag(
            entry,
            Severity::Error,
            "KEYBINDINGS_INVALID",
            "keys.yaml must be a mapping".into(),
            None,
            Location { line: 1, column: 1 },
        ));
        return;
    };
    for key in root.keys().filter_map(|v| v.as_str()) {
        if !["defaults", "projects"].contains(&key) {
            out.push(diag(
                entry,
                Severity::Error,
                "CONFIG_UNKNOWN_FIELD",
                format!("unknown keybindings field `{key}`"),
                None,
                locate_key(text, key),
            ));
        }
    }
    let mut seen = HashSet::new();
    for project in std::iter::once(None).chain(projects.iter().map(|name| Some(*name))) {
        let (_, diagnostics) = validate_keymaps_yaml(text, project);
        for diagnostic in diagnostics {
            let (severity, code, message, location) = match diagnostic {
                KeymapDiagnostic::Error {
                    kind,
                    message,
                    location,
                } => (
                    Severity::Error,
                    match kind {
                        KeymapErrorKind::ParseError => "KEYBINDINGS_PARSE_ERROR",
                        KeymapErrorKind::UnknownAction => "KEYBINDINGS_UNKNOWN_ACTION",
                        KeymapErrorKind::UnknownChord => "KEYBINDINGS_UNKNOWN_CHORD",
                        KeymapErrorKind::Collision => "KEYBINDINGS_COLLISION",
                    },
                    message,
                    location,
                ),
                KeymapDiagnostic::Warning {
                    kind,
                    message,
                    location,
                } => (
                    Severity::Warning,
                    match kind {
                        KeymapWarningKind::ReservedChordRebind => {
                            "KEYBINDINGS_RESERVED_CHORD_REBOUND"
                        }
                        KeymapWarningKind::LegacyZenToggleField => "KEYBINDINGS_LEGACY_ZEN_TOGGLE",
                        KeymapWarningKind::LegacyZenToggleMigrated => {
                            "KEYBINDINGS_LEGACY_ZEN_MIGRATED"
                        }
                        KeymapWarningKind::ShadowedByGlobal => "KEYBINDINGS_SHADOWED_BY_GLOBAL",
                    },
                    message,
                    location,
                ),
            };
            let key = (code, message.clone(), location.clone());
            if !seen.insert(key) {
                continue;
            }
            let location = location
                .as_deref()
                .and_then(|path| path.rsplit('.').next())
                .map(|key| locate_key(text, key))
                .unwrap_or(Location { line: 1, column: 1 });
            out.push(diag(entry, severity, code, message, None, location));
        }
    }
}

fn lint_json_surface(entry: &InventoryEntry, text: &str, out: &mut Vec<Diagnostic>) {
    let rendered = if entry.logical_id.ends_with("workspace-settings-template") {
        if text.contains("{{worker_") {
            out.push(diag(
                entry,
                Severity::Error,
                "TEMPLATE_LEGACY_PLACEHOLDER",
                "workspace settings template contains an unsupported `{{worker_*}}` placeholder"
                    .into(),
                Some("Run `shelbi reload` to refresh the lifecycle-owned template.".into()),
                Location { line: 1, column: 1 },
            ));
        }
        text.replace("{{workspace_permissions_mode}}", "default")
    } else {
        text.to_string()
    };
    match serde_json::from_str::<serde_json::Value>(&rendered) {
        Ok(value) if value.is_object() => {}
        Ok(_) => out.push(diag(
            entry,
            Severity::Error,
            "CONFIG_JSON_CONTRACT",
            "settings JSON must be an object at the top level".into(),
            None,
            Location { line: 1, column: 1 },
        )),
        Err(e) => out.push(diag(
            entry,
            Severity::Error,
            "CONFIG_JSON_SYNTAX",
            e.to_string(),
            None,
            Location {
                line: e.line(),
                column: e.column(),
            },
        )),
    }
}

fn lint_markdown_surface(entry: &InventoryEntry, text: &str, out: &mut Vec<Diagnostic>) {
    if text.trim().is_empty() {
        out.push(diag(
            entry,
            Severity::Error,
            "MARKDOWN_EMPTY",
            "Markdown configuration must not be empty".into(),
            None,
            Location { line: 1, column: 1 },
        ));
        return;
    }
    if entry.logical_id.ends_with(".zenmode") && text.lines().next().unwrap_or("").trim().is_empty()
    {
        out.push(diag(
            entry,
            Severity::Error,
            "ZENMODE_SUMMARY_MISSING",
            "zenmode.md must begin with a non-empty one-line summary".into(),
            Some(
                "Put the heartbeat summary on the first line; the remaining prose is free-form."
                    .into(),
            ),
            Location { line: 1, column: 1 },
        ));
    }
}

fn find_entry<'a>(entries: &'a [&InventoryEntry], logical_id: &str) -> Option<&'a InventoryEntry> {
    entries
        .iter()
        .copied()
        .find(|entry| entry.logical_id == logical_id)
}

fn locate_key(text: &str, key: &str) -> Location {
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(key)
            && trimmed
                .get(key.len()..)
                .is_some_and(|tail| tail.trim_start().starts_with(':'))
        {
            return Location {
                line: line_index + 1,
                column: line.len() - trimmed.len() + 1,
            };
        }
    }
    Location { line: 1, column: 1 }
}

fn push_yaml_error(
    entry: &InventoryEntry,
    code: &str,
    error: &serde_yaml::Error,
    out: &mut Vec<Diagnostic>,
) {
    let location = error
        .location()
        .map(|location| Location {
            line: location.line(),
            column: location.column(),
        })
        .unwrap_or(Location { line: 1, column: 1 });
    out.push(diag(
        entry,
        Severity::Error,
        code,
        error.to_string(),
        None,
        location,
    ));
}

fn push_parse(
    entry: &InventoryEntry,
    code: &str,
    error: impl std::fmt::Display,
    out: &mut Vec<Diagnostic>,
) {
    out.push(semantic(entry, code, error, None));
}

fn semantic(
    entry: &InventoryEntry,
    code: &str,
    error: impl std::fmt::Display,
    remediation: Option<String>,
) -> Diagnostic {
    diag(
        entry,
        Severity::Error,
        code,
        error.to_string(),
        remediation,
        Location { line: 1, column: 1 },
    )
}

fn missing(entry: &InventoryEntry, code: &str) -> Diagnostic {
    diag(
        entry,
        Severity::Error,
        code,
        "required configuration file is missing from the candidate".into(),
        None,
        Location { line: 1, column: 1 },
    )
}

fn diag(
    entry: &InventoryEntry,
    severity: Severity,
    code: &str,
    message: String,
    remediation: Option<String>,
    location: Location,
) -> Diagnostic {
    Diagnostic {
        logical_id: entry.logical_id.clone(),
        file: entry.canonical_path.clone(),
        location,
        severity,
        code: code.to_string(),
        message,
        remediation,
    }
}

pub fn render_human(report: &LintReport) -> String {
    let mut out = String::new();
    for diagnostic in &report.diagnostics {
        let severity = match diagnostic.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        out.push_str(&format!(
            "{}:{}:{}: {severity}[{}] {} ({})\n",
            diagnostic.file.display(),
            diagnostic.location.line,
            diagnostic.location.column,
            diagnostic.code,
            diagnostic.message,
            diagnostic.logical_id,
        ));
        if let Some(remediation) = &diagnostic.remediation {
            out.push_str(&format!("  help: {remediation}\n"));
        }
    }
    if report.is_clean() {
        out.push_str("configuration valid: 0 warnings, 0 errors\n");
    } else {
        out.push_str(&format!(
            "{} warnings, {} errors\n",
            report.warnings, report.errors
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_entry(id: &str, format: SurfaceFormat) -> InventoryEntry {
        InventoryEntry {
            logical_id: id.into(),
            scope: "global".into(),
            canonical_path: PathBuf::from("/tmp/config"),
            candidate_path: PathBuf::from("config"),
            format,
            exists: true,
            lifecycle_owned: false,
        }
    }

    #[test]
    fn unknown_user_config_field_is_stable_diagnostic() {
        let entry = fake_entry("global.preferences", SurfaceFormat::Yaml);
        let mut diagnostics = Vec::new();
        lint_typed_yaml::<UserConfig>(&entry, "editor: vim\nmystery: true\n", &mut diagnostics);
        assert!(diagnostics.iter().any(|d| d.code == "CONFIG_UNKNOWN_FIELD"));
    }

    #[test]
    fn keybinding_collision_is_an_error() {
        let entry = fake_entry("global.keybindings", SurfaceFormat::Yaml);
        let mut diagnostics = Vec::new();
        lint_keybindings(
            &entry,
            "defaults:\n  sidebar:\n    nav_up: k\n    nav_down: k\n",
            &[],
            &mut diagnostics,
        );
        assert!(diagnostics
            .iter()
            .any(|d| d.code == "KEYBINDINGS_COLLISION"));
    }

    #[test]
    fn candidate_paths_cannot_escape_stage() {
        assert!(checked_candidate(Path::new("/tmp/stage"), Path::new("../live")).is_err());
    }
}
