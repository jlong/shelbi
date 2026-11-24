//! A worker's OpenCode wiring, all of it in Shelbi's own namespace
//! (`<worktree>/.shelbi/opencode/`), handed to OpenCode through the launch's
//! `OPENCODE_CONFIG_CONTENT`:
//!
//! | path | what |
//! | --- | --- |
//! | `config.json` | the config layer ([`shelbi_agent::RunnerAdapter::opencode_config`]) |
//! | `instructions.md` | the agent's composed instructions, at system authority |
//! | `skills/` | the agent's skills (not `.claude/skills/`, which OpenCode also reads) |
//! | `plugins/shelbi/` | Shelbi's plugin (`index.js`, `package.json`) |
//! | `plugin.lock`, `plugin.alive` | the plugin's single-instance lock and its health beacon |
//!
//! Everything is rewritten on each launch, and nothing goes under
//! `.opencode/`, `opencode.json`, `AGENTS.md` or `~/.config/opencode`. The
//! plugin's old home, `.opencode/plugins/shelbi.js`, is removed on launch when
//! Shelbi wrote it.

use std::path::Path;
use std::time::{Duration, Instant};

use shelbi_core::Host;

use super::{
    append_dispatch_status, gitignore_worktree_paths, mount_skill_entry, read_owned_skills,
    read_worktree_text, remove_worktree_entry, worktree_entry_exists, write_owned_skills,
    write_worktree_text, Error, Result, SHELBI_EXCLUDE_HEADER, WORKTREE_AGENT_SKILLS_REL,
};

/// Worktree-relative root of a worker's OpenCode layer.
pub const OPENCODE_DIR_REL: &str = ".shelbi/opencode";
/// The generated config layer the launch reads into `OPENCODE_CONFIG_CONTENT`.
pub const OPENCODE_CONFIG_REL: &str = ".shelbi/opencode/config.json";
/// The agent's composed instructions, named in the layer's `instructions`.
pub const OPENCODE_INSTRUCTIONS_REL: &str = ".shelbi/opencode/instructions.md";
/// The agent's skills, named in the layer's `skills`.
pub const OPENCODE_SKILLS_REL: &str = ".shelbi/opencode/skills";
/// Shelbi's plugin directory, named in the layer's `plugins`.
pub const OPENCODE_PLUGIN_DIR_REL: &str = ".shelbi/opencode/plugins/shelbi";
/// The beacon the plugin writes once it has loaded and taken the lock.
pub const OPENCODE_PLUGIN_ALIVE_REL: &str = ".shelbi/opencode/plugin.alive";
/// The plugin's single-instance lock (holds the acting instance's pid).
pub const OPENCODE_PLUGIN_LOCK_REL: &str = ".shelbi/opencode/plugin.lock";
/// Where the plugin lived before the config layer: a Shelbi file inside
/// OpenCode's project directory. Removed on launch when Shelbi wrote it.
pub const LEGACY_OPENCODE_PLUGIN_REL: &str = ".opencode/plugins/shelbi.js";

/// The plugin body (see `opencode_plugin.js`).
pub(crate) const OPENCODE_PLUGIN_JS: &str = include_str!("../opencode_plugin.js");

/// The plugin directory's manifest. OpenCode loads a local plugin from a
/// directory, through its package entry point.
const OPENCODE_PLUGIN_PACKAGE_JSON: &str = r#"{
  "name": "shelbi-opencode-plugin",
  "private": true,
  "type": "module",
  "main": "index.js"
}
"#;

/// First line of every plugin file Shelbi writes, so it can tell its own
/// file from a user's.
pub(crate) const OPENCODE_PLUGIN_SIGNATURE: &str = "// Shelbi's OpenCode plugin.";

/// How long a launch waits for the plugin's beacon before reporting the
/// pane's state observation as degraded.
pub const OPENCODE_PLUGIN_BEACON_WAIT: Duration = Duration::from_secs(10);

/// Why a workspace's state is read off the screen (the body of its
/// `observation-degraded` record and the event's `reason`).
pub const OPENCODE_PLUGIN_SILENT_REASON: &str = "opencode-plugin-silent";

/// What [`compose_and_stage_opencode_context`] staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedOpenCodeContext {
    /// The composed instructions (preamble + the agent's `instructions.md`).
    pub instructions: String,
    /// Whether the agent has any skills (so the layer names the directory).
    pub has_skills: bool,
}

/// Compose the agent's instructions and stage its skills for OpenCode: the
/// skills go to [`OPENCODE_SKILLS_REL`], not `.claude/skills/`. OpenCode
/// reads `.claude/skills/` too, so a skill Shelbi left there for an earlier
/// Claude dispatch on this workspace is removed (only Shelbi's own, by its
/// manifest), or the agent would see every skill twice.
pub(crate) fn compose_and_stage_opencode_context(
    host: &Host,
    worktree: &Path,
    project_name: &str,
    agent: &str,
) -> Result<StagedOpenCodeContext> {
    let instructions = shelbi_state::compose_agent_prompt(project_name, agent)
        .map_err(|e| Error::Other(format!("{e}")))?;
    let skills_src = shelbi_state::agent_skills_dir(project_name, agent)?;
    clear_owned_claude_skills(host, worktree)?;
    let has_skills = stage_opencode_skills(host, worktree, &skills_src)?;
    let _ = shelbi_state::maybe_emit_claude_md_migration_hint(project_name);
    Ok(StagedOpenCodeContext { instructions, has_skills })
}

/// Remove the `.claude/skills/` entries Shelbi mounted on an earlier dispatch
/// and empty its manifest. A user's own skills there are left alone.
fn clear_owned_claude_skills(host: &Host, worktree: &Path) -> Result<()> {
    let owned = read_owned_skills(host, worktree)?;
    if owned.is_empty() {
        return Ok(());
    }
    let skills_dir = worktree.join(WORKTREE_AGENT_SKILLS_REL);
    for name in &owned {
        remove_worktree_entry(host, &skills_dir.join(name))?;
    }
    write_owned_skills(host, worktree, &[])
}

/// Re-mount `skills_src`'s entries into [`OPENCODE_SKILLS_REL`], which is
/// Shelbi's alone and so is emptied first. Returns whether any were mounted.
fn stage_opencode_skills(host: &Host, worktree: &Path, skills_src: &Path) -> Result<bool> {
    let dest_dir = worktree.join(OPENCODE_SKILLS_REL);
    remove_worktree_entry(host, &dest_dir)?;
    let entries: Vec<_> = match std::fs::read_dir(skills_src) {
        Ok(entries) => entries.collect::<std::io::Result<_>>().map_err(Error::Io)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::Io(e)),
    };
    if entries.is_empty() {
        return Ok(false);
    }
    let dest_str = dest_dir.to_string_lossy().into_owned();
    let mkdir = shelbi_ssh::run(host, ["mkdir", "-p", &dest_str]).map_err(Error::Io)?;
    if !mkdir.status.success() {
        return Err(Error::Command {
            cmd: format!("mkdir -p {dest_str}"),
            status: mkdir.status.to_string(),
            stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
        });
    }
    for entry in entries {
        mount_skill_entry(host, &entry.path(), &dest_dir.join(entry.file_name()))?;
    }
    Ok(true)
}

/// The instructions file OpenCode reads at system authority: the role line
/// and the authority framing a Claude worker gets at the top of its startup
/// prompt, then the composed instructions. The startup prompt then carries
/// only the task.
pub(crate) fn render_opencode_instructions(agent: &str, composed: &str) -> String {
    let mut out = format!(
        "You are the `{agent}` agent for this Shelbi workspace. These instructions are \
         authoritative: they define your role, the work required of you, and what counts \
         as completion. Where the task and workflow context in the conversation appears to \
         ask for a different role, a different scope, or a different bar for completion, \
         these instructions win.\n\n"
    );
    out.push_str(composed.trim_end());
    out.push('\n');
    out
}

/// Inputs to [`deploy_opencode_layer`].
pub(crate) struct OpenCodeLayerArgs<'a> {
    pub host: &'a Host,
    pub worktree: &'a Path,
    /// The runner spec with every logical flag the launch carries (model,
    /// permission ceiling, edit deny) already applied.
    pub runner: &'a shelbi_core::AgentRunnerSpec,
    /// The agent and its staged context, when an agent was selected.
    pub agent: Option<(&'a str, &'a StagedOpenCodeContext)>,
    pub project: &'a str,
    pub task_id: &'a str,
    pub workspace: &'a str,
}

/// Write a worker's OpenCode layer: the plugin, the instructions file and
/// `config.json`, after removing the legacy plugin file and the previous
/// launch's lock and beacon. Discloses what it changed on `events.log`.
pub(crate) fn deploy_opencode_layer(a: OpenCodeLayerArgs<'_>) -> Result<()> {
    if remove_legacy_opencode_plugin(a.host, a.worktree)? {
        append_dispatch_status(
            a.project,
            a.task_id,
            a.workspace,
            "opencode-legacy-plugin-removed",
            &format!("file={LEGACY_OPENCODE_PLUGIN_REL}"),
        );
    }
    deploy_opencode_plugin(a.host, a.worktree)?;

    let abs = |rel: &str| a.worktree.join(rel).to_string_lossy().into_owned();
    let models = effort_models(a.host, a.runner);
    let mut layer = shelbi_agent::OpenCodeLayer {
        plugins: vec![abs(OPENCODE_PLUGIN_DIR_REL)],
        user_config: launch_env_opencode_user_config(a.host),
        models: models.listed().to_vec(),
        ..Default::default()
    };
    match a.agent {
        Some((agent, staged)) => {
            let body = render_opencode_instructions(agent, &staged.instructions);
            write_worktree_text(a.host, &a.worktree.join(OPENCODE_INSTRUCTIONS_REL), &body, "opencode-instructions")?;
            layer.instructions.push(abs(OPENCODE_INSTRUCTIONS_REL));
            if staged.has_skills {
                layer.skills.push(abs(OPENCODE_SKILLS_REL));
            }
        }
        None => remove_worktree_entry(a.host, &a.worktree.join(OPENCODE_INSTRUCTIONS_REL))?,
    }

    let config = shelbi_agent::RunnerAdapter::for_spec(a.runner).opencode_config(a.runner, &layer);
    if config.user_config_unreadable {
        append_dispatch_status(
            a.project,
            a.task_id,
            a.workspace,
            "opencode-user-config-ignored",
            "reason=not-a-json-object var=OPENCODE_CONFIG_CONTENT",
        );
    }
    if let Some(effort) = config.effort_unapplied {
        // The launch goes ahead on the model's own default effort.
        append_dispatch_status(
            a.project,
            a.task_id,
            a.workspace,
            "opencode-effort-unapplied",
            &format!("effort={} reason={}", effort.as_str(), models.unapplied_reason(a.runner)),
        );
    }
    // The layer always names the plugin, so there's always a document.
    let json = config.json.unwrap_or_else(|| "{}".to_string());
    write_worktree_text(a.host, &a.worktree.join(OPENCODE_CONFIG_REL), &json, "opencode-config")?;
    let _ = gitignore_worktree_paths(a.host, a.worktree, &[".shelbi/".to_string()]);
    Ok(())
}

/// The model list an OpenCode launch maps its effort against: the hub's
/// `opencode models` probe, the same catalog the effort picker reads.
#[derive(Debug)]
pub(crate) enum EffortModels {
    /// No effort was resolved, so nothing was asked.
    NotNeeded,
    /// The launch runs on another machine, whose OpenCode the hub's list
    /// doesn't describe: no variant rather than a guessed one.
    Remote,
    /// The probe couldn't answer (OpenCode missing, too old, or slow).
    Unavailable,
    Listed(Vec<shelbi_core::OpenCodeModel>),
}

impl EffortModels {
    pub(crate) fn listed(&self) -> &[shelbi_core::OpenCodeModel] {
        match self {
            EffortModels::Listed(models) => models,
            _ => &[],
        }
    }

    /// Why an effort set no variant, for the dispatch status line.
    pub(crate) fn unapplied_reason(&self, runner: &shelbi_core::AgentRunnerSpec) -> &'static str {
        match self {
            EffortModels::Remote => "remote-host",
            EffortModels::Unavailable => "models-unavailable",
            _ if shelbi_agent::opencode_model(runner).is_none() => "no-model",
            _ => "variant-not-reported",
        }
    }
}

/// The models to map `runner`'s effort against. Asked only when an effort
/// was resolved and the launch is on this machine; waits for the probe (it
/// runs once per process, within [`shelbi_app::opencode_models::DEADLINE`])
/// and never fails the dispatch.
pub(crate) fn effort_models(host: &Host, runner: &shelbi_core::AgentRunnerSpec) -> EffortModels {
    use shelbi_app::opencode_models::{wait, Catalog, DEADLINE};
    if shelbi_agent::opencode_effort(runner).is_none() {
        return EffortModels::NotNeeded;
    }
    if !host.is_local() {
        return EffortModels::Remote;
    }
    match wait(DEADLINE) {
        Catalog::Listed(models) => EffortModels::Listed(models.to_vec()),
        Catalog::Probing | Catalog::Unavailable => EffortModels::Unavailable,
    }
}

/// Write the plugin directory and clear the previous launch's lock and
/// beacon, so the beacon a launch waits for is its own plugin's.
pub(crate) fn deploy_opencode_plugin(host: &Host, worktree: &Path) -> Result<()> {
    let dir = worktree.join(OPENCODE_PLUGIN_DIR_REL);
    write_worktree_text(host, &dir.join("index.js"), OPENCODE_PLUGIN_JS, "opencode-plugin")?;
    write_worktree_text(host, &dir.join("package.json"), OPENCODE_PLUGIN_PACKAGE_JSON, "opencode-plugin")?;
    remove_worktree_entry(host, &worktree.join(OPENCODE_PLUGIN_ALIVE_REL))?;
    remove_worktree_entry(host, &worktree.join(OPENCODE_PLUGIN_LOCK_REL))?;
    Ok(())
}

/// Remove the plugin an older Shelbi wrote to
/// [`LEGACY_OPENCODE_PLUGIN_REL`], plus its git-exclude line, when the file
/// starts with Shelbi's signature. A user's file there is never touched. The
/// `.opencode/plugins/` and `.opencode/` directories go too, but only when
/// that leaves them empty. Returns whether a file was removed.
pub(crate) fn remove_legacy_opencode_plugin(host: &Host, worktree: &Path) -> Result<bool> {
    let file = worktree.join(LEGACY_OPENCODE_PLUGIN_REL);
    match read_worktree_text(host, &file)? {
        Some(body) if body.starts_with(OPENCODE_PLUGIN_SIGNATURE) => {}
        _ => return Ok(false),
    }
    remove_worktree_entry(host, &file)?;
    for dir in [".opencode/plugins", ".opencode"] {
        let dir = worktree.join(dir).to_string_lossy().into_owned();
        // `rmdir` refuses a directory with anything in it.
        let _ = shelbi_ssh::run(host, ["rmdir", &dir]);
    }
    let _ = unexclude_worktree_path(host, worktree, LEGACY_OPENCODE_PLUGIN_REL);
    Ok(true)
}

/// Drop `rel`'s line from the worktree's git exclude file, and Shelbi's
/// header with it when no Shelbi line is left under it. Best-effort, like
/// [`gitignore_worktree_paths`].
fn unexclude_worktree_path(host: &Host, worktree: &Path, rel: &str) -> Result<()> {
    let wt = worktree.to_string_lossy().into_owned();
    let out = match shelbi_ssh::run(host, ["git", "-C", &wt, "rev-parse", "--git-path", "info/exclude"]) {
        Ok(out) if out.status.success() => out,
        _ => return Ok(()),
    };
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return Ok(());
    }
    let exclude = if Path::new(&raw).is_absolute() { raw.into() } else { worktree.join(&raw) };
    let Some(existing) = read_worktree_text(host, &exclude)? else {
        return Ok(());
    };
    if let Some(body) = without_exclude_line(&existing, rel) {
        write_worktree_text(host, &exclude, &body, "git-exclude")?;
    }
    Ok(())
}

/// `exclude` without `rel`'s line (and without Shelbi's header if nothing
/// follows it any more), or `None` when `rel` isn't there.
fn without_exclude_line(exclude: &str, rel: &str) -> Option<String> {
    if !exclude.lines().any(|l| l.trim() == rel) {
        return None;
    }
    let mut lines: Vec<&str> = exclude.lines().filter(|l| l.trim() != rel).collect();
    if let Some(i) = lines.iter().position(|l| *l == SHELBI_EXCLUDE_HEADER) {
        let rest = &lines[i + 1..];
        if rest.iter().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')) {
            lines.remove(i);
        }
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Some(out)
}

/// The user's own `OPENCODE_CONFIG_CONTENT` in the environment the pane's
/// launch runs in ([`shelbi_agent::opencode_user_config`]). Locally that's
/// the session child environment every pane is given. On a remote host it's
/// the user's login shell there, asked over SSH; an unreachable host reads as
/// none.
pub(crate) fn launch_env_opencode_user_config(host: &Host) -> Option<String> {
    match host {
        Host::Local => {
            let env = shelbi_core::session_child_env();
            shelbi_agent::opencode_user_config(|key| env.get(key).cloned())
        }
        Host::Ssh { .. } => {
            let out = shelbi_ssh::run_with_deadline(
                host,
                ["sh", "-c", &remote_env_probe_script()],
                Duration::from_secs(15),
            )
            .ok()?;
            parse_remote_env_probe(&String::from_utf8_lossy(&out.stdout))
        }
    }
}

/// Record separator around each field of the remote probe's output, which
/// rc-file chatter and JSON can't contain.
const PROBE_SEP: char = '\u{1e}';

/// Prints the two variables from an interactive login shell (the
/// environment a remote session hands its pane), each field fenced by
/// [`PROBE_SEP`]: `K<1 if the kept value is set>`, `U<kept value>`,
/// `C<OPENCODE_CONFIG_CONTENT>`, `E`.
fn remote_env_probe_script() -> String {
    let user = shelbi_agent::OPENCODE_USER_CONFIG_ENV;
    let config = shelbi_agent::OPENCODE_CONFIG_ENV;
    let inner = format!(
        r#"printf '\036K%s\036U%s\036C%s\036E' "${{{user}+1}}" "${{{user}-}}" "${{{config}-}}""#
    );
    format!(
        "\"${{SHELL:-/bin/sh}}\" -l -i -c {} </dev/null 2>/dev/null",
        shelbi_core::shell_escape(&inner)
    )
}

/// The user's value from [`remote_env_probe_script`]'s output, or `None`
/// when the fields aren't all there.
fn parse_remote_env_probe(out: &str) -> Option<String> {
    let start = out.find(&format!("{PROBE_SEP}K"))?;
    let mut fields = out[start + PROBE_SEP.len_utf8()..].split(PROBE_SEP);
    let kept = fields.next()?.strip_prefix('K')?;
    let user = fields.next()?.strip_prefix('U')?;
    let config = fields.next()?.strip_prefix('C')?;
    fields.next()?.strip_prefix('E')?;
    shelbi_agent::opencode_user_config(|key| {
        if key == shelbi_agent::OPENCODE_USER_CONFIG_ENV {
            (kept == "1").then(|| user.to_string())
        } else if key == shelbi_agent::OPENCODE_CONFIG_ENV {
            (!config.is_empty()).then(|| config.to_string())
        } else {
            None
        }
    })
}

/// After an OpenCode launch, wait up to `wait` for the plugin's beacon. When
/// it never comes, the pane's state can only be read off its screen: record
/// that hub-side (the poller then matches OpenCode's own dialogs) and report
/// `state_observation=degraded` once. Returns whether the plugin reported in.
pub(crate) fn observe_opencode_plugin(
    host: &Host,
    worktree: &Path,
    project: &str,
    task_id: &str,
    workspace: &str,
    wait: Duration,
) -> bool {
    let beacon = worktree.join(OPENCODE_PLUGIN_ALIVE_REL);
    let deadline = Instant::now() + wait;
    // Each look at a remote worktree is an SSH round trip, so look less often.
    let every = match host {
        Host::Local => Duration::from_millis(250),
        Host::Ssh { .. } => Duration::from_secs(1),
    };
    loop {
        if worktree_entry_exists(host, &beacon) {
            return true;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(every);
    }
    if let Err(e) = shelbi_state::mark_observation_degraded(project, workspace, OPENCODE_PLUGIN_SILENT_REASON) {
        eprintln!("shelbi: couldn't record degraded state observation for `{workspace}`: {e}");
    }
    append_dispatch_status(
        project,
        task_id,
        workspace,
        "integration",
        &format!(
            "state_observation=degraded reason={OPENCODE_PLUGIN_SILENT_REASON} file={OPENCODE_PLUGIN_ALIVE_REL} \
             fallback=dialog-signatures"
        ),
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-opencode-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plugin_lands_in_shelbis_namespace_and_clears_the_last_launchs_beacon() {
        let wt = tmp("plugin");
        let state = wt.join(OPENCODE_DIR_REL);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(wt.join(OPENCODE_PLUGIN_ALIVE_REL), "{}").unwrap();
        std::fs::write(wt.join(OPENCODE_PLUGIN_LOCK_REL), "1\n").unwrap();

        deploy_opencode_plugin(&Host::Local, &wt).unwrap();
        let dir = wt.join(OPENCODE_PLUGIN_DIR_REL);
        let index = std::fs::read_to_string(dir.join("index.js")).unwrap();
        assert!(index.starts_with(OPENCODE_PLUGIN_SIGNATURE));
        assert_eq!(index, OPENCODE_PLUGIN_JS);
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
        assert_eq!(manifest["main"], "index.js");
        assert_eq!(manifest["type"], "module");
        assert!(!wt.join(OPENCODE_PLUGIN_ALIVE_REL).exists());
        assert!(!wt.join(OPENCODE_PLUGIN_LOCK_REL).exists());
        // Nothing under OpenCode's own project directory.
        assert!(!wt.join(".opencode").exists());
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn legacy_plugin_is_removed_only_when_shelbi_wrote_it() {
        let wt = tmp("legacy");
        let legacy = wt.join(LEGACY_OPENCODE_PLUGIN_REL);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();

        // Shelbi's: removed, with the directories it leaves empty.
        std::fs::write(&legacy, format!("{OPENCODE_PLUGIN_SIGNATURE} old\n")).unwrap();
        assert!(remove_legacy_opencode_plugin(&Host::Local, &wt).unwrap());
        assert!(!wt.join(".opencode").exists());

        // A user's file at the same path: untouched.
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "export default {}\n").unwrap();
        assert!(!remove_legacy_opencode_plugin(&Host::Local, &wt).unwrap());
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "export default {}\n");

        // Shelbi's beside a user's other plugin: only Shelbi's file goes.
        std::fs::write(&legacy, format!("{OPENCODE_PLUGIN_SIGNATURE}\n")).unwrap();
        std::fs::write(wt.join(".opencode/plugins/mine.js"), "x").unwrap();
        assert!(remove_legacy_opencode_plugin(&Host::Local, &wt).unwrap());
        assert!(!legacy.exists());
        assert!(wt.join(".opencode/plugins/mine.js").exists());

        // Nothing there at all.
        let empty = tmp("legacy-none");
        assert!(!remove_legacy_opencode_plugin(&Host::Local, &empty).unwrap());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn exclude_line_removal_keeps_everything_else() {
        let header = SHELBI_EXCLUDE_HEADER;
        let only = format!("*.log\n{header}\n{LEGACY_OPENCODE_PLUGIN_REL}\n");
        assert_eq!(without_exclude_line(&only, LEGACY_OPENCODE_PLUGIN_REL).unwrap(), "*.log\n");
        let shared = format!("{header}\n.shelbi/\n{LEGACY_OPENCODE_PLUGIN_REL}\n");
        assert_eq!(
            without_exclude_line(&shared, LEGACY_OPENCODE_PLUGIN_REL).unwrap(),
            format!("{header}\n.shelbi/\n")
        );
        assert_eq!(without_exclude_line("*.log\n", LEGACY_OPENCODE_PLUGIN_REL), None);
    }

    #[test]
    fn instructions_file_carries_the_role_and_authority() {
        let body = render_opencode_instructions("developer", "# Developer\n\nDo the work.\n\n");
        assert!(body.starts_with("You are the `developer` agent for this Shelbi workspace."));
        assert!(body.contains("these instructions win"));
        assert!(body.ends_with("# Developer\n\nDo the work.\n"));
    }

    #[test]
    fn remote_probe_output_yields_the_users_value() {
        let sep = PROBE_SEP;
        // rc chatter before the fields is skipped.
        let out = format!("Welcome!\n{sep}K{sep}U{sep}C{{\"a\":1}}{sep}E");
        assert_eq!(parse_remote_env_probe(&out), Some("{\"a\":1}".into()));
        // A value a Shelbi launch kept wins over Shelbi's own layer.
        let out = format!("{sep}K1{sep}U{{\"u\":2}}{sep}C{{\"shelbi\":1}}{sep}E");
        assert_eq!(parse_remote_env_probe(&out), Some("{\"u\":2}".into()));
        let out = format!("{sep}K1{sep}U{sep}C{{\"shelbi\":1}}{sep}E");
        assert_eq!(parse_remote_env_probe(&out), None);
        // Nothing set, or a truncated reply.
        assert_eq!(parse_remote_env_probe(&format!("{sep}K{sep}U{sep}C{sep}E")), None);
        assert_eq!(parse_remote_env_probe(&format!("{sep}K{sep}U")), None);
        assert_eq!(parse_remote_env_probe(""), None);
    }

    /// The probe script, run by a local `sh` standing in for the remote
    /// host, prints fields [`parse_remote_env_probe`] reads back.
    #[test]
    fn remote_probe_script_round_trips_through_a_shell() {
        let out = std::process::Command::new("sh")
            .args(["-c", &remote_env_probe_script()])
            .env("SHELL", "/bin/sh")
            .env("OPENCODE_CONFIG_CONTENT", r#"{"theme":"a b"}"#)
            .env_remove("SHELBI_OPENCODE_USER_CONFIG")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(parse_remote_env_probe(&text), Some(r#"{"theme":"a b"}"#.into()), "{text:?}");
    }

    /// Deploy a worker layer for `runner` and read back the model both
    /// agents get, plus the events the dispatch disclosed.
    fn deployed_model(runner: &shelbi_core::AgentRunnerSpec, tag: &str) -> (serde_json::Value, String) {
        let wt = tmp(tag);
        deploy_opencode_layer(OpenCodeLayerArgs {
            host: &Host::Local,
            worktree: &wt,
            runner,
            agent: None,
            project: "demo",
            task_id: tag,
            workspace: "alpha",
        })
        .expect("the dispatch never fails over effort");
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(wt.join(OPENCODE_CONFIG_REL)).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&wt);
        assert_eq!(config["agents"]["build"]["model"], config["agents"]["plan"]["model"], "{config}");
        let events = std::fs::read_to_string(shelbi_state::events_log_path().unwrap())
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(&format!("task={tag} ")))
            .collect::<Vec<_>>()
            .join("\n");
        (config["agents"]["build"]["model"].clone(), events)
    }

    fn opencode_runner(model: Option<&str>, effort: Option<shelbi_core::ReasoningEffort>) -> shelbi_core::AgentRunnerSpec {
        let base = shelbi_core::AgentRunnerSpec {
            command: "opencode".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        shelbi_agent::with_reasoning_effort(&shelbi_agent::with_model(&base, model), effort)
    }

    /// Decision Q4 at dispatch: the effort becomes the variant only when the
    /// hub's model list reports it; otherwise the launch goes ahead with no
    /// variant and says why.
    #[test]
    fn dispatch_maps_effort_onto_a_reported_variant_only() {
        use shelbi_app::opencode_models::{listed, set_for_this_thread, Catalog};
        use shelbi_core::ReasoningEffort::{High, Max};
        let _g = crate::test_lock::isolated_home();

        set_for_this_thread(Some(listed(&[("anthropic/sonnet", &["low", "high"])])));
        let (model, events) = deployed_model(&opencode_runner(Some("anthropic/sonnet"), Some(High)), "eff-high");
        assert_eq!(model, "anthropic/sonnet#high");
        assert!(!events.contains("opencode-effort-unapplied"), "{events}");

        // A variant the model doesn't report.
        let (model, events) = deployed_model(&opencode_runner(Some("anthropic/sonnet"), Some(Max)), "eff-max");
        assert_eq!(model, "anthropic/sonnet");
        assert!(events.contains("status=opencode-effort-unapplied"), "{events}");
        assert!(events.contains("effort=max") && events.contains("reason=variant-not-reported"), "{events}");

        // No effort: no variant, nothing to say.
        let (model, events) = deployed_model(&opencode_runner(Some("anthropic/sonnet"), None), "eff-none");
        assert_eq!(model, "anthropic/sonnet");
        assert!(!events.contains("opencode-effort-unapplied"), "{events}");

        // The probe failed: no variant, and the dispatch still succeeds.
        set_for_this_thread(Some(Catalog::Unavailable));
        let (model, events) = deployed_model(&opencode_runner(Some("anthropic/sonnet"), Some(High)), "eff-noprobe");
        assert_eq!(model, "anthropic/sonnet");
        assert!(events.contains("effort=high") && events.contains("reason=models-unavailable"), "{events}");
        set_for_this_thread(None);
    }

    /// A remote launch never borrows the hub's list, and nothing is asked
    /// when there's no effort.
    #[test]
    fn effort_models_asks_only_for_a_local_launch_with_an_effort() {
        let remote = Host::Ssh { host: "box".into() };
        let high = opencode_runner(Some("p/m"), Some(shelbi_core::ReasoningEffort::High));
        let none = opencode_runner(Some("p/m"), None);
        assert!(matches!(effort_models(&remote, &high), EffortModels::Remote));
        assert_eq!(effort_models(&remote, &high).unapplied_reason(&high), "remote-host");
        assert!(matches!(effort_models(&Host::Local, &none), EffortModels::NotNeeded));
        let no_model = opencode_runner(None, Some(shelbi_core::ReasoningEffort::High));
        assert_eq!(EffortModels::Listed(vec![]).unapplied_reason(&no_model), "no-model");
    }

    #[test]
    fn a_silent_plugin_marks_the_workspace_degraded() {
        let _g = crate::test_lock::isolated_home();
        let wt = tmp("beacon");
        let _ = shelbi_state::clear_observation_degraded("demo", "alpha");

        // The beacon is there: nothing recorded.
        std::fs::create_dir_all(wt.join(OPENCODE_DIR_REL)).unwrap();
        std::fs::write(wt.join(OPENCODE_PLUGIN_ALIVE_REL), "{}").unwrap();
        assert!(observe_opencode_plugin(&Host::Local, &wt, "demo", "t1", "alpha", Duration::ZERO));
        assert!(!shelbi_state::observation_degraded("demo", "alpha"));

        // No beacon by the deadline: degraded, disclosed on events.log.
        std::fs::remove_file(wt.join(OPENCODE_PLUGIN_ALIVE_REL)).unwrap();
        assert!(!observe_opencode_plugin(&Host::Local, &wt, "demo", "t1", "alpha", Duration::ZERO));
        assert!(shelbi_state::observation_degraded("demo", "alpha"));
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains("state_observation=degraded"), "{log}");
        assert!(log.contains(OPENCODE_PLUGIN_SILENT_REASON), "{log}");
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn opencode_skills_replace_shelbis_claude_skill_mounts() {
        let tmp_root = tmp("skills");
        let wt = tmp_root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let src = tmp_root.join("agent-skills");
        std::fs::create_dir_all(src.join("greet")).unwrap();
        std::fs::write(src.join("greet/SKILL.md"), "hi\n").unwrap();

        // An earlier Claude dispatch left Shelbi's mount (and a user's skill).
        super::super::refresh_agent_skills(&Host::Local, &wt, &src).unwrap();
        std::fs::create_dir_all(wt.join(".claude/skills/mine")).unwrap();
        assert!(wt.join(".claude/skills/greet").exists());

        clear_owned_claude_skills(&Host::Local, &wt).unwrap();
        assert!(stage_opencode_skills(&Host::Local, &wt, &src).unwrap());
        assert!(!wt.join(".claude/skills/greet").exists(), "Shelbi's Claude mount must go");
        assert!(wt.join(".claude/skills/mine").exists(), "the user's skill stays");
        assert_eq!(
            std::fs::read_to_string(wt.join(OPENCODE_SKILLS_REL).join("greet/SKILL.md")).unwrap(),
            "hi\n"
        );
        assert!(read_owned_skills(&Host::Local, &wt).unwrap().is_empty());

        // No skills: the directory is gone and the layer won't name it.
        let none = tmp_root.join("no-skills");
        assert!(!stage_opencode_skills(&Host::Local, &wt, &none).unwrap());
        assert!(!wt.join(OPENCODE_SKILLS_REL).exists());
        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    /// Runs the plugin under Node against a fake OpenCode `api`: a
    /// `setup` without Shelbi's named session stays passive, the first with
    /// it takes the lock and writes the beacon, a second in the same
    /// process stays passive, a lock held by another live process keeps it
    /// passive, and a dead holder's lock is taken over. Skipped without
    /// `node`.
    #[test]
    fn plugin_takes_a_single_instance_lock_and_writes_its_beacon() {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("node not found; skipping the plugin lock test");
            return;
        }
        let root = tmp("plugin-node");
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join(".shelbi/hooks")).unwrap();
        let plugin = root.join("plugin.mjs");
        std::fs::write(&plugin, OPENCODE_PLUGIN_JS).unwrap();
        let script = root.join("run.mjs");
        std::fs::write(
            &script,
            r#"
import { existsSync, readFileSync, writeFileSync } from "node:fs";
const [pluginPath, wt] = process.argv.slice(2);
const plugin = (await import(pluginPath)).default;
const api = () => ({
  location: { directory: wt },
  session: { get: async () => ({}), synthetic: async () => {} },
  event: { subscribe: (opts) => ({ async *[Symbol.asyncIterator]() {
    await new Promise((r) => opts.signal.addEventListener("abort", r));
  } }) },
});
const lock = `${wt}/.shelbi/opencode/plugin.lock`;
const alive = `${wt}/.shelbi/opencode/plugin.alive`;
const out = {};
// A pane without Shelbi's named session isn't Shelbi's: no lock, no beacon.
const session = process.env.SHELBI_OPENCODE_SESSION;
delete process.env.SHELBI_OPENCODE_SESSION;
out.unnamedPassive = (await plugin.setup(api())) === undefined && !existsSync(lock);
process.env.SHELBI_OPENCODE_SESSION = session;
const first = await plugin.setup(api());
out.firstActive = typeof first === "function";
out.lockPid = parseInt(readFileSync(lock, "utf8"), 10) === process.pid;
out.beacon = JSON.parse(readFileSync(alive, "utf8"));
const second = await plugin.setup(api());
out.secondPassive = second === undefined;
first();
out.lockReleased = !existsSync(lock);
// Held by another live process (our parent).
writeFileSync(lock, `${process.ppid}\n`);
out.heldPassive = (await plugin.setup(api())) === undefined;
// Held by a pid that's gone.
writeFileSync(lock, "999999\n");
const taken = await plugin.setup(api());
out.staleTaken = typeof taken === "function";
taken?.();
console.log(JSON.stringify(out));
"#,
        )
        .unwrap();
        let out = std::process::Command::new("node")
            .arg(&script)
            .arg(&plugin)
            .arg(&wt)
            .env("SHELBI_PANE_TTY", "/dev/null")
            .env("SHELBI_OPENCODE_SESSION", "ses_shelbi0123456789abcdef0123")
            .env_remove("SHELBI_OPENCODE_TITLE")
            .env_remove("TASK_ID")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let result: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        assert_eq!(result["unnamedPassive"], true, "{result}");
        assert_eq!(result["firstActive"], true, "{result}");
        assert_eq!(result["lockPid"], true, "{result}");
        assert_eq!(result["beacon"]["plugin"], "shelbi", "{result}");
        assert_eq!(result["secondPassive"], true, "{result}");
        assert_eq!(result["lockReleased"], true, "{result}");
        assert_eq!(result["heldPassive"], true, "{result}");
        assert_eq!(result["staleTaken"], true, "{result}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
