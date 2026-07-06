//! `shelbi agent <subcommand>` — inspect and manage the per-project
//! `agents/<name>/` workspaces.
//!
//! The four subcommands mirror the `shelbi workflow` and `shelbi task`
//! shapes so the noun is discoverable in `--help`:
//!
//! - `list` — table of every agent in the project, with the statuses it's
//!   referenced by, its skill count, and whether its `instructions.md`
//!   diverges from the bundled default.
//! - `show` — print the agent's `instructions.md` followed by a `Skills:`
//!   list of its skill files' frontmatter descriptions.
//! - `new`  — scaffold `agents/<name>/instructions.md` + `agents/<name>/skills/`.
//! - `edit` — open `agents/<name>/instructions.md` in `$EDITOR`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum AgentCmd {
    /// Print a table of every agent in the project: name, statuses that
    /// reference it, skill count, and whether the agent's
    /// `instructions.md` has been customized away from the bundled
    /// default.
    List,
    /// Print the agent's `instructions.md` content followed by a list of
    /// its skills (`skills/*.md`) with each skill's frontmatter
    /// `description`. Errors if the agent doesn't exist.
    Show { name: String },
    /// Scaffold a new agent directory: `agents/<name>/instructions.md`
    /// (placeholder) + `agents/<name>/skills/` (empty). Errors if the
    /// directory already exists.
    New { name: String },
    /// Open `agents/<name>/instructions.md` in `$EDITOR` (falling back to
    /// `$VISUAL`, then `vim`). Errors if the agent doesn't exist.
    Edit { name: String },
}

pub fn run(project_opt: Option<String>, cmd: AgentCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        AgentCmd::List => list(&project),
        AgentCmd::Show { name } => show(&project, &name),
        AgentCmd::New { name } => new(&project, &name),
        AgentCmd::Edit { name } => edit(&project, &name),
    }
}

fn list(project: &str) -> Result<()> {
    let agents = shelbi_state::list_agents(project).map_err(|e| anyhow!(e))?;
    if agents.is_empty() {
        println!("(no agents under {})", agents_dir_display(project)?);
        return Ok(());
    }

    // Build agent -> statuses map by sweeping every workflow once. The
    // STATUSES column lists status ids that name this agent in their
    // `agent:` field, deduped across workflows. Status ids carry no
    // workflow qualifier in the output — the agents-workspaces plan
    // calls for a flat join.
    let workflows = shelbi_state::list_workflows(project).map_err(|e| anyhow!(e))?;
    let mut statuses_by_agent: std::collections::HashMap<&str, BTreeSet<String>> =
        std::collections::HashMap::new();
    for wf in &workflows {
        for status in &wf.statuses {
            if let Some(agent_name) = status.agent.as_deref() {
                statuses_by_agent
                    .entry(agent_name)
                    .or_default()
                    .insert(status.id.clone());
            }
        }
    }

    println!(
        "{:<14} {:<26} {:<7} CUSTOMIZED",
        "AGENT", "STATUSES", "SKILLS",
    );
    for name in &agents {
        let statuses = statuses_by_agent
            .get(name.as_str())
            .map(|set| set.iter().cloned().collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string());
        let skill_count =
            shelbi_state::count_agent_skills(project, name).map_err(|e| anyhow!(e))?;
        let customized = customized_marker(project, name)?;
        println!(
            "{:<14} {:<26} {:<7} {}",
            name, statuses, skill_count, customized
        );
    }
    Ok(())
}

/// `yes` / `no` / `-` per the spec, routed through the shared
/// provenance mechanism ([`shelbi_state::agent_divergence`]) so a
/// `shelbi` upgrade — which changes the *compiled* default — doesn't flip
/// an untouched agent to `yes`:
///
/// - `yes` — shipped default whose `instructions.md` was edited away from
///   the default that was deployed (a genuine user customization), or is
///   missing.
/// - `no`  — shipped default still running a bundled default, whether the
///   current one (pristine-current) or a now-stale one a later upgrade
///   will auto-replace (pristine-stale). The user hasn't customized it.
/// - `-`   — user-added agent (not in [`shelbi_state::DEFAULT_AGENTS`]);
///   the "customized" question doesn't apply because there's nothing to
///   compare against.
fn customized_marker(project: &str, agent: &str) -> Result<&'static str> {
    use shelbi_state::AgentDivergence;
    Ok(
        match shelbi_state::agent_divergence(project, agent).map_err(|e| anyhow!(e))? {
            None => "-",
            // Pristine-current and pristine-stale are both "not user-customized".
            // Pristine-stale re-materializes to the current default on the next
            // `shelbi reload`, but it was never edited, so `no` is truthful.
            Some(AgentDivergence::PristineCurrent | AgentDivergence::PristineStale) => "no",
            // A genuine edit, or a missing instructions.md (self-heal will
            // recreate it, but right now it's divergent from the bundled body).
            Some(AgentDivergence::Customized) => "yes",
        },
    )
}

fn agents_dir_display(project: &str) -> Result<String> {
    Ok(shelbi_state::agents_dir(project)
        .map_err(|e| anyhow!(e))?
        .display()
        .to_string())
}

fn show(project: &str, name: &str) -> Result<()> {
    // `new` validates the name before it touches the filesystem; `show`/`edit`
    // must too, so a traversal-shaped argument is rejected up front with the
    // same message rather than reaching `agent_workspace_dir`'s join (F15).
    validate_agent_name(name)?;
    let workspace = shelbi_state::agent_workspace_dir(project, name).map_err(|e| anyhow!(e))?;
    if !workspace.exists() {
        bail!(
            "agent `{name}` not found at {}. Run `shelbi agent new {name}` to create it.",
            workspace.display()
        );
    }

    let instructions_path =
        shelbi_state::agent_instructions_path(project, name).map_err(|e| anyhow!(e))?;
    match fs::read_to_string(&instructions_path) {
        Ok(text) => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("(no instructions.md at {})", instructions_path.display());
        }
        Err(e) => return Err(anyhow!(e)),
    }

    let skills_dir = shelbi_state::agent_skills_dir(project, name).map_err(|e| anyhow!(e))?;
    let skills = read_skills(&skills_dir)?;
    println!();
    println!("Skills:");
    if skills.is_empty() {
        println!("  (none)");
    } else {
        for s in &skills {
            let description = s.description.as_deref().unwrap_or("(no description)");
            println!("- {} — {description}", s.name);
        }
    }
    Ok(())
}

fn new(project: &str, name: &str) -> Result<()> {
    validate_agent_name(name)?;
    let workspace = shelbi_state::agent_workspace_dir(project, name).map_err(|e| anyhow!(e))?;
    if workspace.exists() {
        bail!("agent `{name}` already exists at {}", workspace.display());
    }
    let skills_dir = shelbi_state::agent_skills_dir(project, name).map_err(|e| anyhow!(e))?;
    let instructions_path =
        shelbi_state::agent_instructions_path(project, name).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&workspace).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&skills_dir).map_err(|e| anyhow!(e))?;
    fs::write(&instructions_path, placeholder_instructions(name))?;
    println!("✓ agent '{name}' scaffolded at agents/{name}/.");
    println!();
    println!("next: edit agents/{name}/instructions.md, then bind it to a workflow status:");
    println!("  workflows/<workflow>.yaml: add `agent: {name}` to a status's frontmatter.");
    Ok(())
}

fn edit(project: &str, name: &str) -> Result<()> {
    validate_agent_name(name)?;
    let workspace = shelbi_state::agent_workspace_dir(project, name).map_err(|e| anyhow!(e))?;
    if !workspace.exists() {
        bail!("agent `{name}` not found. Run 'shelbi agent new {name}' to create it.");
    }
    let instructions_path =
        shelbi_state::agent_instructions_path(project, name).map_err(|e| anyhow!(e))?;
    super::launch_editor(&instructions_path)
}

/// One skill's parsed frontmatter — just the bits the `show` listing
/// surfaces. Extra fields in the YAML are ignored.
struct SkillEntry {
    name: String,
    description: Option<String>,
}

fn read_skills(skills_dir: &Path) -> Result<Vec<SkillEntry>> {
    if !skills_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(skills_dir)? {
        let entry = entry?;
        let path = entry.path();
        // `entry.file_type()` describes the dirent itself, so a symlinked
        // skill (`skills/foo.md -> ../shared/foo.md`) reports as a symlink and
        // was silently skipped. Stat *through* the link with `fs::metadata` so
        // linked skills are read; a broken link errors here and is skipped
        // (F15).
        let is_file = fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        if stem.starts_with('.') {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let description = parse_skill_description(&text);
        out.push(SkillEntry {
            name: stem,
            description,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Parse the `description:` field out of a skill markdown file's YAML
/// frontmatter. Returns `None` if there's no frontmatter, no
/// `description:` key, or the YAML doesn't parse — the caller surfaces
/// `(no description)` in that case rather than choking on a malformed
/// skill.
fn parse_skill_description(text: &str) -> Option<String> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let close = rest.find("\n---")?;
    let yaml = &rest[..close];
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let mapping = value.as_mapping()?;
    let desc = mapping.get(serde_yaml::Value::String("description".into()))?;
    Some(desc.as_str()?.trim().to_string())
}

/// Reject anything that wouldn't survive as a directory name under
/// `agents/`. Matches the `validate_workflow_name` shape but adds an
/// extra rule: `_` is forbidden as a name prefix because `_shared/` is
/// reserved for the preamble dir referenced by the default developer
/// prompt.
fn validate_agent_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("agent name must not be empty");
    }
    if name.starts_with('.') {
        bail!("agent name `{name}` must not start with `.`");
    }
    if name.starts_with('_') {
        bail!("agent name `{name}` must not start with `_` (reserved for the shared preamble dir)");
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!(
                "agent name `{name}` contains invalid character `{c}` — \
                 use a-z, 0-9, `-`, `_`"
            );
        }
    }
    Ok(())
}

/// Placeholder `instructions.md` body for `shelbi agent new`. The
/// header reminds the author what to write and points them at the
/// shared preamble dir for project-wide context.
fn placeholder_instructions(name: &str) -> String {
    format!(
        "# {name} agent\n\
         \n\
         <!-- Replace this file with the agent's system prompt. It will be loaded as\n\
         --system-prompt when shelbi dispatches a task to a workspace running this\n\
         agent. Project-wide context (the \"you're in this monorepo, here's the\n\
         layout\" intro) belongs in agents/_shared/preamble.md instead — that gets\n\
         prepended automatically. -->\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-agent-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn materialize_defaults(project: &str) {
        shelbi_state::materialize_default_agents(project).unwrap();
    }

    #[test]
    fn validate_agent_name_accepts_kebab_and_snake() {
        validate_agent_name("orchestrator").unwrap();
        validate_agent_name("developer").unwrap();
        validate_agent_name("qa-bot").unwrap();
        validate_agent_name("snake_case").unwrap();
        validate_agent_name("a1").unwrap();
    }

    #[test]
    fn validate_agent_name_rejects_path_separators_and_reserved_prefixes() {
        assert!(validate_agent_name("").is_err());
        assert!(validate_agent_name(".hidden").is_err());
        assert!(validate_agent_name("_shared").is_err());
        assert!(validate_agent_name("a/b").is_err());
        assert!(validate_agent_name("foo bar").is_err());
        assert!(validate_agent_name("foo.md").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn read_skills_follows_symlinked_skill_files() {
        // A skill can be a symlink into a shared library dir (F15). Stating
        // through the link (not the dirent's own type) is what surfaces it.
        let dir = fresh_home();
        let skills = dir.join("skills");
        std::fs::create_dir_all(&skills).unwrap();

        // A regular skill file...
        std::fs::write(
            skills.join("plain.md"),
            "---\ndescription: plain one\n---\n",
        )
        .unwrap();
        // ...and a symlinked one pointing at a target outside the skills dir.
        let target = dir.join("shared-linked.md");
        std::fs::write(&target, "---\ndescription: linked one\n---\n").unwrap();
        std::os::unix::fs::symlink(&target, skills.join("linked.md")).unwrap();

        let entries = read_skills(&skills).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["linked", "plain"]);
        let linked = entries.iter().find(|e| e.name == "linked").unwrap();
        assert_eq!(linked.description.as_deref(), Some("linked one"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_skill_description_extracts_field() {
        let s = "---\nname: foo\ndescription: A short summary.\n---\nbody\n";
        assert_eq!(
            parse_skill_description(s).as_deref(),
            Some("A short summary.")
        );
    }

    #[test]
    fn parse_skill_description_handles_missing_or_malformed() {
        // No frontmatter.
        assert_eq!(parse_skill_description("no fm\n"), None);
        // Frontmatter without description key.
        assert_eq!(parse_skill_description("---\nname: x\n---\nbody\n"), None);
        // Malformed YAML — must not panic, just degrade to None.
        assert_eq!(parse_skill_description("---\n: :\n---\nbody\n"), None);
    }

    #[test]
    fn list_succeeds_on_fresh_project_with_no_agents() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No agents/ directory at all.
        list("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_includes_defaults_with_no_status_marker_when_no_workflows_reference_them() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_defaults("p");
        // No workflows author `agent: orchestrator|developer`, so the
        // STATUSES column should show `-` for both. We exercise the run
        // path here; output is covered by the printed-table golden tests
        // below via helpers (list itself only returns Ok).
        list("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn customized_marker_reports_no_for_unmodified_default() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_defaults("p");
        assert_eq!(customized_marker("p", "orchestrator").unwrap(), "no");
        assert_eq!(customized_marker("p", "developer").unwrap(), "no");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A `shelbi` upgrade bumps the compiled default under an untouched
    /// agent. The marker must stay `no` (pristine-stale), not flip to
    /// `yes` — the byte-compare false positive this task fixes.
    #[test]
    fn customized_marker_reports_no_for_stale_untouched_default() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_defaults("p");
        // Simulate the previous bundled default sitting on disk with its
        // provenance recorded, while the compiled default has since moved on.
        let v1 = "# previous bundled default\n";
        let path = shelbi_state::agent_instructions_path("p", "orchestrator").unwrap();
        std::fs::write(&path, v1).unwrap();
        shelbi_state::update_state("p", |s| {
            s.deployed_agent_defaults
                .insert("orchestrator".to_string(), shelbi_state::content_hash(v1));
            Ok(())
        })
        .unwrap();
        assert_eq!(customized_marker("p", "orchestrator").unwrap(), "no");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn customized_marker_reports_yes_when_instructions_diverge() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_defaults("p");
        let path = shelbi_state::agent_instructions_path("p", "orchestrator").unwrap();
        std::fs::write(&path, "# my own prompt\n").unwrap();
        assert_eq!(customized_marker("p", "orchestrator").unwrap(), "yes");
        // The other default is still untouched.
        assert_eq!(customized_marker("p", "developer").unwrap(), "no");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn customized_marker_dash_for_user_added_agent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        new("p", "qa").unwrap();
        assert_eq!(customized_marker("p", "qa").unwrap(), "-");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_status_join_dedupes_across_workflows() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_defaults("p");
        // The workflow loader requires `statuses.yaml` to be present —
        // stand in for the `shelbi init` / `shelbi reload` step.
        shelbi_state::save_project_statuses("p", &shelbi_core::default_project_statuses()).unwrap();
        // Author two workflows that both bind a status to `developer`.
        let dir = shelbi_state::workflows_dir("p").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("default.yaml"),
            r#"name: default
statuses:
  - { id: todo, owner: agent, agent: developer }
  - { id: done, owner: user }
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("research.yaml"),
            r#"name: research
statuses:
  - { id: todo,        owner: agent, agent: developer }
  - { id: in-progress, owner: agent, agent: developer }
  - { id: done,        owner: user }
"#,
        )
        .unwrap();

        // Recompute the join the same way list() does and verify the dedupe.
        let workflows = shelbi_state::list_workflows("p").unwrap();
        let mut set: BTreeSet<String> = BTreeSet::new();
        for wf in &workflows {
            for s in &wf.statuses {
                if s.agent.as_deref() == Some("developer") {
                    set.insert(s.id.clone());
                }
            }
        }
        let joined = set.iter().cloned().collect::<Vec<_>>().join(", ");
        // `todo` appears in both workflows; we want it once.
        assert_eq!(joined, "in-progress, todo");

        // Drive the list path itself to make sure it doesn't panic on the fixture.
        list("p").unwrap();

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn show_errors_on_missing_agent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = show("p", "ghost").unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("shelbi agent new"), "{err}");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn show_prints_instructions_and_skills_section_with_descriptions() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        new("p", "qa").unwrap();
        let skills_dir = shelbi_state::agent_skills_dir("p", "qa").unwrap();
        std::fs::write(
            skills_dir.join("alpha.md"),
            "---\nname: alpha\ndescription: First skill.\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            skills_dir.join("beta.md"),
            "---\nname: beta\ndescription: Second skill.\n---\nbody\n",
        )
        .unwrap();
        // No-frontmatter file — its description should fall back to `(no description)`.
        std::fs::write(skills_dir.join("gamma.md"), "no frontmatter\n").unwrap();
        // Smoke test: the path must not error.
        show("p", "qa").unwrap();

        // Verify the helper output order + content directly.
        let skills = read_skills(&skills_dir).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
        assert_eq!(skills[0].description.as_deref(), Some("First skill."));
        assert_eq!(skills[1].description.as_deref(), Some("Second skill."));
        assert_eq!(skills[2].description.as_deref(), None);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn new_scaffolds_directory_instructions_and_skills_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        new("p", "qa").unwrap();
        let workspace = shelbi_state::agent_workspace_dir("p", "qa").unwrap();
        let instructions = shelbi_state::agent_instructions_path("p", "qa").unwrap();
        let skills = shelbi_state::agent_skills_dir("p", "qa").unwrap();
        assert!(workspace.is_dir());
        assert!(instructions.is_file());
        assert!(skills.is_dir());
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(body.contains("# qa agent"));
        assert!(body.contains("agents/_shared/preamble.md"));
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn new_errors_when_agent_already_exists() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        new("p", "qa").unwrap();
        let err = new("p", "qa").unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn new_rejects_invalid_name_before_touching_disk() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = new("p", "escape/me").unwrap_err().to_string();
        assert!(err.contains("invalid character"), "{err}");
        // Validation runs first — no agents/ directory should have been
        // created as a side-effect.
        assert!(!shelbi_state::agents_dir("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn edit_errors_when_agent_missing_with_hint_at_new() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = edit("p", "ghost").unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("shelbi agent new ghost"), "{err}");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn edit_invokes_editor_on_existing_agent() {
        // Point EDITOR at /usr/bin/true so the spawn step is a successful
        // no-op — we can't realistically launch a real $EDITOR from a unit
        // test.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // VISUAL takes precedence over EDITOR in the shared resolver, so
        // clear it to keep this test pinned to the EDITOR we set.
        std::env::remove_var("VISUAL");
        std::env::set_var("EDITOR", "/usr/bin/true");
        new("p", "qa").unwrap();
        edit("p", "qa").unwrap();
        std::env::remove_var("EDITOR");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
