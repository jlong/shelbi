//! Pluggable agent CLI runners.
//!
//! v1 model: every runner is a CLI command + flags. shelbi launches it in
//! interactive mode inside a session process and drives it over the session
//! socket.
//!
//! In the future this trait can grow methods for richer integration
//! (`session_id`, `--resume`, streaming JSON), but the v1 surface stays
//! intentionally minimal.

use shelbi_core::{AgentRunnerSpec, CapabilityLadder, ReasoningEffort, RunnerKind};

/// POSIX shell-quoting, re-exported from `shelbi-core` so the historical
/// `shelbi_agent::shell_escape` path keeps working for the command-string
/// builders across the orchestrator and CLI. The canonical definition (and
/// its tests) live in [`shelbi_core::shell`]; the SSH-boundary escaper in
/// `shelbi-ssh` reaches the same function without depending on this crate.
pub use shelbi_core::shell_escape;

/// Construct the shell command to launch the agent CLI. Returns a single
/// string suitable for a session process's child command.
///
/// OpenCode's line is assembled by [`opencode_launch_command`].
pub fn launch_command(spec: &AgentRunnerSpec) -> String {
    if RunnerAdapter::for_spec(spec).is_opencode() {
        return opencode_launch_command(spec);
    }
    let mut parts = vec![shell_escape(&spec.command)];
    for f in &spec.flags {
        parts.push(shell_escape(f));
    }
    parts.join(" ")
}

/// Where an OpenCode launch takes its `OPENCODE_CONFIG_CONTENT` from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeConfigSource<'a> {
    /// Built from the spec's logical flags plus this layer, and written into
    /// the launch line. Left out when there's nothing to set.
    Inline(&'a OpenCodeLayer),
    /// Read at launch from this worktree-relative file, which the caller
    /// generated with [`RunnerAdapter::opencode_config`] (a worker's
    /// `.shelbi/opencode/config.json`). The spec's logical flags are already
    /// in it, so the launch only strips them.
    File(&'a str),
}

/// OpenCode's launch line with no layer beyond the spec's own flags. See
/// [`opencode_launch_command_with`].
pub fn opencode_launch_command(spec: &AgentRunnerSpec) -> String {
    opencode_launch_command_with(spec, OpenCodeConfigSource::Inline(&OpenCodeLayer::default()))
}

/// OpenCode's launch line:
///
/// ```text
/// env SHELBI_PANE_TTY="$(tty)" \
///     [SHELBI_OPENCODE_SESSION=<id> SHELBI_OPENCODE_TITLE='Shelbi · <ws> · <task>'] \
///     [SHELBI_OPENCODE_ROLE=orchestrator] \
///     [SHELBI_OPENCODE_USER_CONFIG="${SHELBI_OPENCODE_USER_CONFIG-${OPENCODE_CONFIG_CONTENT-}}" \
///      OPENCODE_CONFIG_CONTENT=<config>] \
///     OPENCODE_CLI_CONFIG_CONTENT='{"terminal":{"title":false},"highlights":false}' \
///     OPENCODE_DISABLE_AUTOUPDATE=1 \
///   opencode --standalone [--auto] [--session <id>] <flags>
/// ```
///
/// - `--session <id>` (from [`with_opencode_session`]) reopens Shelbi's own
///   session for the task, or creates it. Its id and title are exported for
///   the plugin, which acts only on that session and its subagents and
///   titles it. A `--continue` / `-c` beside it is dropped: it would pick the
///   directory's newest session instead.
/// - `SHELBI_OPENCODE_ROLE` (from [`with_opencode_role`]) tells the plugin
///   which part to play: `orchestrator` delivers board events to the session
///   instead of running a worker's hook scripts.
/// - `--standalone` gives the pane a private server instead of OpenCode's
///   shared background service, so the server (and Shelbi's plugin inside it)
///   inherits this pane's environment (`TASK_ID`, the hub address). That
///   server has no controlling terminal, so the pane's tty path is exported
///   as [`OPENCODE_PANE_TTY_ENV`] for the plugin's `shelbi:<state>` markers.
/// - `OPENCODE_CONFIG_CONTENT` is the config layer OpenCode merges over the
///   user's own files (see [`RunnerAdapter::opencode_config`]): inline, or
///   `"$(cat <file>)"` for a worker's generated layer. It is left out when
///   the inline layer has nothing to set, so the user's settings stand.
/// - Whenever Shelbi sets `OPENCODE_CONFIG_CONTENT` it also keeps the user's
///   own value, as it was before any Shelbi launch, in
///   [`OPENCODE_USER_CONFIG_ENV`]. Everything the OpenCode process starts
///   inherits Shelbi's layer, so a `shelbi` command run from inside the pane
///   reads the user's value back from there ([`opencode_user_config`])
///   rather than mistaking Shelbi's layer for it.
/// - `OPENCODE_CLI_CONFIG_CONTENT` turns off OpenCode's terminal title,
///   which otherwise overwrites the plugin's `shelbi:<state>` markers with
///   `OC | <session title>`, and its highlights.
/// - `OPENCODE_DISABLE_AUTOUPDATE` keeps a worker pane from updating the
///   binary under the hub mid-task.
/// - `--auto` (answer every remaining ask) is passed only for a
///   `bypassPermissions` ceiling; any other ceiling drops a hand-written one,
///   since it would approve the asks the ceiling exists to raise.
pub fn opencode_launch_command_with(spec: &AgentRunnerSpec, config: OpenCodeConfigSource<'_>) -> String {
    let mode = flag_value(&spec.flags, &[OPENCODE_PERMISSION_FLAG]);
    let bypass = mode.as_deref().is_some_and(|m| shelbi_core::permission_mode_rank(m) >= 3);
    let session = flag_value(&spec.flags, OPENCODE_SESSION_FLAGS);
    let title = flag_value(&spec.flags, &[OPENCODE_TITLE_FLAG]);
    let role = flag_value(&spec.flags, &[OPENCODE_ROLE_FLAG]);
    let mut flags: Vec<String> =
        strip_flag_with_value(
            &spec.flags,
            &[
                "--model",
                "-m",
                OPENCODE_PERMISSION_FLAG,
                OPENCODE_TITLE_FLAG,
                OPENCODE_EFFORT_FLAG,
                OPENCODE_ROLE_FLAG,
            ],
        )
            .into_iter()
            .filter(|f| f != OPENCODE_DENY_EDITS_FLAG && f != OPENCODE_STANDALONE_FLAG)
            .collect();
    if session.is_some() {
        flags.retain(|f| !OPENCODE_CONTINUE_FLAGS.contains(&f.as_str()));
    }
    if mode.is_some() {
        flags.retain(|f| f != OPENCODE_AUTO_FLAG);
        if bypass {
            flags.insert(0, OPENCODE_AUTO_FLAG.into());
        }
    }

    let content = match config {
        OpenCodeConfigSource::Inline(layer) => RunnerAdapter::for_spec(spec)
            .opencode_config(spec, layer)
            .json
            .map(|json| shell_escape(&json)),
        OpenCodeConfigSource::File(rel) => Some(format!("\"$(cat {})\"", shell_escape(rel))),
    };

    // `env`, not a bare `VAR=value` prefix: callers run the line as
    // `exec {launch}`, and after `exec` an assignment word is taken for the
    // command name.
    let mut parts = vec!["env".to_string(), format!("{OPENCODE_PANE_TTY_ENV}=\"$(tty)\"")];
    if let Some(session) = &session {
        parts.push(format!("{OPENCODE_SESSION_ENV}={}", shell_escape(session)));
        if let Some(title) = &title {
            parts.push(format!("{OPENCODE_TITLE_ENV}={}", shell_escape(title)));
        }
    }
    if let Some(role) = &role {
        parts.push(format!("{OPENCODE_ROLE_ENV}={}", shell_escape(role)));
    }
    if let Some(content) = content {
        parts.push(format!(
            "{OPENCODE_USER_CONFIG_ENV}=\"${{{OPENCODE_USER_CONFIG_ENV}-${{{OPENCODE_CONFIG_ENV}-}}}}\""
        ));
        parts.push(format!("{OPENCODE_CONFIG_ENV}={content}"));
    }
    parts.push(format!("OPENCODE_CLI_CONFIG_CONTENT={}", shell_escape(OPENCODE_CLI_CONFIG_CONTENT)));
    parts.push("OPENCODE_DISABLE_AUTOUPDATE=1".into());
    parts.push(shell_escape(&spec.command));
    parts.push(OPENCODE_STANDALONE_FLAG.into());
    for f in &flags {
        parts.push(shell_escape(f));
    }
    parts.join(" ")
}

/// OpenCode's inline-config variable: the highest-ranking config document a
/// user can set, above global, project and `.opencode/` config.
pub const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// Where an OpenCode launch keeps the user's own `OPENCODE_CONFIG_CONTENT`
/// (empty when they had none) while Shelbi's layer takes the variable. See
/// [`opencode_launch_command_with`].
pub const OPENCODE_USER_CONFIG_ENV: &str = "SHELBI_OPENCODE_USER_CONFIG";

/// The user's own `OPENCODE_CONFIG_CONTENT` in a launch environment, read
/// through `get`: the value kept in [`OPENCODE_USER_CONFIG_ENV`] when an
/// OpenCode pane Shelbi launched is an ancestor (its `OPENCODE_CONFIG_CONTENT`
/// is Shelbi's layer), else `OPENCODE_CONFIG_CONTENT` itself. `None` for
/// unset or blank.
pub fn opencode_user_config(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let value = match get(OPENCODE_USER_CONFIG_ENV) {
        Some(kept) => kept,
        None => get(OPENCODE_CONFIG_ENV)?,
    };
    (!value.trim().is_empty()).then_some(value)
}

/// What an OpenCode config layer carries beyond the runner spec's logical
/// flags (model, permission ceiling, edit deny). Every path is absolute:
/// OpenCode resolves a relative entry against the document that names it,
/// and an inline document has no directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenCodeLayer {
    /// Instruction files, added to the system prompt (system authority, and
    /// re-read when the file changes).
    pub instructions: Vec<String>,
    /// Skill directories, each holding `<name>/SKILL.md` entries.
    pub skills: Vec<String>,
    /// Plugin directories.
    pub plugins: Vec<String>,
    /// The models OpenCode lists, each with the variants it reports (the
    /// hub's `opencode models` probe, the one the effort picker reads).
    /// Empty when the probe couldn't answer. A resolved effort becomes the
    /// model's `#variant` only when the spec's model is listed here with a
    /// same-named variant ([`opencode_effort_variant`]); otherwise no variant
    /// is set, never a guessed one.
    pub models: Vec<shelbi_core::OpenCodeModel>,
    /// The user's own `OPENCODE_CONFIG_CONTENT` from the launch environment
    /// ([`opencode_user_config`]), merged under Shelbi's keys.
    pub user_config: Option<String>,
}

/// The `OPENCODE_CONFIG_CONTENT` an OpenCode launch sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeConfig {
    /// The JSON, or `None` when there's nothing to set (and the user's own
    /// value, if any, can stand as it is).
    pub json: Option<String>,
    /// The user's own value wasn't a JSON object, so it was left out rather
    /// than merged. The caller says so: the user's settings didn't apply.
    pub user_config_unreadable: bool,
    /// An effort was resolved for the launch but set no variant: there's no
    /// model, the model isn't listed ([`OpenCodeLayer::models`] is empty when
    /// the probe failed), or it reports no variant of that name. The model's
    /// own default stands; the caller says so.
    pub effort_unapplied: Option<ReasoningEffort>,
}

impl RunnerAdapter {
    /// The config layer for an OpenCode launch, as the JSON for
    /// `OPENCODE_CONFIG_CONTENT`. Pure: everything comes from `spec`'s
    /// logical flags and `layer`.
    ///
    /// ```jsonc
    /// {
    ///   "instructions": ["<abs>/.shelbi/opencode/instructions.md"],
    ///   "skills": ["<abs>/.shelbi/opencode/skills"],
    ///   "plugins": ["<abs>/.shelbi/opencode/plugins/shelbi"],
    ///   "agents": {
    ///     "build": { "model": "provider/model#variant", "permissions": [ … ] },
    ///     "plan":  { "model": "provider/model#variant", "permissions": [ … ] }
    ///   }
    /// }
    /// ```
    ///
    /// - A key is emitted only when Shelbi has a value for it, so with no
    ///   model and no ceiling `agents` is left out and the user's OpenCode
    ///   settings govern.
    /// - The model goes on the `build` and `plan` agents rather than the
    ///   root, because OpenCode drops a root model's `#variant`.
    /// - A resolved effort ([`opencode_effort`]) becomes that `#variant` only
    ///   when [`OpenCodeLayer::models`] lists the model with a same-named
    ///   variant. Otherwise the bare model is written and the effort is
    ///   reported back in [`OpenCodeConfig::effort_unapplied`]. A model that
    ///   already names a variant keeps it.
    /// - The permission rules ([`opencode_permission_rules`]) go on both
    ///   agents. Agent rules load after the user's and the project's, and
    ///   the last matching rule wins, so they hold the ceiling.
    /// - The user's own value ([`OpenCodeLayer::user_config`]) is merged
    ///   under Shelbi's keys, since setting the variable would otherwise
    ///   shadow it: objects merge key by key, lists keep the user's entries
    ///   and add Shelbi's after them (so Shelbi's permission rules still come
    ///   last), and any other value Shelbi sets replaces the user's.
    ///
    /// A runner other than OpenCode gets no layer.
    pub fn opencode_config(self, spec: &AgentRunnerSpec, layer: &OpenCodeLayer) -> OpenCodeConfig {
        use serde_json::{Map, Value};

        if !self.is_opencode() {
            return OpenCodeConfig { json: None, user_config_unreadable: false, effort_unapplied: None };
        }
        let strings = |list: &[String]| Value::Array(list.iter().cloned().map(Value::String).collect());

        let mut shelbi = Map::new();
        if !layer.instructions.is_empty() {
            shelbi.insert("instructions".into(), strings(&layer.instructions));
        }
        if !layer.skills.is_empty() {
            shelbi.insert("skills".into(), strings(&layer.skills));
        }
        if !layer.plugins.is_empty() {
            shelbi.insert("plugins".into(), strings(&layer.plugins));
        }

        let model = opencode_model(spec);
        let effort = opencode_effort(spec);
        // A model the user already pinned a variant on keeps it.
        let variant = match (&model, effort) {
            (Some(model), Some(effort)) if !model.contains('#') => layer
                .models
                .iter()
                .find(|m| m.id == *model)
                .and_then(|m| opencode_effort_variant(effort, &m.variants)),
            _ => None,
        };
        let pinned = model.as_deref().is_some_and(|m| m.contains('#'));
        let effort_unapplied = effort.filter(|_| variant.is_none() && !pinned);
        let model = model.map(|model| match &variant {
            Some(variant) => format!("{model}#{variant}"),
            None => model,
        });
        let mode = flag_value(&spec.flags, &[OPENCODE_PERMISSION_FLAG]);
        let deny_edits = spec.flags.iter().any(|f| f == OPENCODE_DENY_EDITS_FLAG);
        let rules = opencode_permission_rules(mode.as_deref(), deny_edits);
        let mut agent = Map::new();
        if let Some(model) = model {
            agent.insert("model".into(), Value::String(model));
        }
        if !rules.is_empty() {
            agent.insert(
                "permissions".into(),
                Value::Array(rules.into_iter().map(OpenCodeRule::to_json).collect()),
            );
        }
        if !agent.is_empty() {
            let agent = Value::Object(agent);
            let mut agents = Map::new();
            agents.insert("build".into(), agent.clone());
            agents.insert("plan".into(), agent);
            shelbi.insert("agents".into(), Value::Object(agents));
        }

        let (user, user_config_unreadable) = match layer.user_config.as_deref().map(str::trim) {
            None | Some("") => (None, false),
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(user)) => (Some(user), false),
                _ => (None, true),
            },
        };
        if shelbi.is_empty() {
            // Nothing of Shelbi's: the user's value (if any) stands unchanged
            // in the environment, so there's nothing to set.
            return OpenCodeConfig { json: None, user_config_unreadable, effort_unapplied };
        }
        let mut merged = Value::Object(user.unwrap_or_default());
        merge_under(&mut merged, Value::Object(shelbi));
        OpenCodeConfig {
            json: Some(merged.to_string()),
            user_config_unreadable,
            effort_unapplied,
        }
    }
}

/// Merge `ours` over `base`: objects key by key, lists by appending the
/// entries `base` doesn't already hold, and anything else replaced.
fn merge_under(base: &mut serde_json::Value, ours: serde_json::Value) {
    use serde_json::Value;
    match (base, ours) {
        (Value::Object(base), Value::Object(ours)) => {
            for (key, value) in ours {
                match base.get_mut(&key) {
                    Some(existing) => merge_under(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(base), Value::Array(ours)) => {
            for value in ours {
                if !base.contains(&value) {
                    base.push(value);
                }
            }
        }
        (base, ours) => *base = ours,
    }
}

/// The OpenCode model variant for a Shelbi effort level: the same-named
/// variant (`low`, `medium`, `high`, `max`), only when OpenCode reports it
/// for the chosen model (`available`, from `opencode models`). Variants are
/// provider-specific, so anything else gets no variant and the model's own
/// default stands.
pub fn opencode_effort_variant(effort: ReasoningEffort, available: &[String]) -> Option<String> {
    let name = effort.as_str();
    available
        .iter()
        .find(|v| v.trim().eq_ignore_ascii_case(name))
        .map(|_| name.to_string())
}

/// The model an OpenCode launch's config layer sets (its logical `--model`),
/// if any.
pub fn opencode_model(spec: &AgentRunnerSpec) -> Option<String> {
    flag_value(&spec.flags, &["--model", "-m"])
}

/// The effort [`RunnerAdapter::with_reasoning_effort`] resolved for an
/// OpenCode launch (its logical `--shelbi-effort` flag), if any.
pub fn opencode_effort(spec: &AgentRunnerSpec) -> Option<ReasoningEffort> {
    let value = flag_value(&spec.flags, &[OPENCODE_EFFORT_FLAG])?;
    [ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High, ReasoningEffort::Max]
        .into_iter()
        .find(|e| value.trim().eq_ignore_ascii_case(e.as_str()))
}

/// One OpenCode permission rule: `effect` for `action` on `resource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCodeRule {
    pub action: &'static str,
    pub resource: &'static str,
    pub effect: &'static str,
}

impl OpenCodeRule {
    const fn new(action: &'static str, effect: &'static str) -> Self {
        OpenCodeRule { action, resource: "*", effect }
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({ "action": self.action, "resource": self.resource, "effect": self.effect })
    }
}

/// The OpenCode permission rules for a Shelbi permission ceiling, in order
/// (the last match wins). OpenCode's own default `build` agent allows
/// everything, so every ceiling has to be spelled out:
///
/// | ceiling | rules |
/// | --- | --- |
/// | none | none: the user's OpenCode settings stand |
/// | `plan` | deny everything, then allow `read`, `glob`, `grep`, `webfetch`, `websearch`, `skill`, `question` |
/// | `default` | ask for `edit`, `shell`, `webfetch`, `external_directory` |
/// | `read-only` | `default`'s, then deny `edit` |
/// | `auto` / `acceptEdits` | allow `edit`, ask for `shell` and `external_directory` |
/// | `bypassPermissions` | allow everything (and the launch adds `--auto`) |
///
/// `deny_edits` (an agent whose settings deny Claude's edit tools, such as
/// the review agent) appends an `edit` deny at any ceiling, and with none.
/// An unrecognized mode ranks as `default`, so a typo never loosens.
pub fn opencode_permission_rules(mode: Option<&str>, deny_edits: bool) -> Vec<OpenCodeRule> {
    const ASK_DEFAULT: [OpenCodeRule; 4] = [
        OpenCodeRule::new("edit", "ask"),
        OpenCodeRule::new("shell", "ask"),
        OpenCodeRule::new("webfetch", "ask"),
        OpenCodeRule::new("external_directory", "ask"),
    ];
    let mut rules = match mode.map(str::trim) {
        None => Vec::new(),
        Some("read-only" | "readonly") => {
            let mut r = ASK_DEFAULT.to_vec();
            r.push(OpenCodeRule::new("edit", "deny"));
            r
        }
        Some(m) => match shelbi_core::permission_mode_rank(m) {
            0 => {
                let mut r = vec![OpenCodeRule::new("*", "deny")];
                r.extend(
                    ["read", "glob", "grep", "webfetch", "websearch", "skill", "question"]
                        .into_iter()
                        .map(|a| OpenCodeRule::new(a, "allow")),
                );
                r
            }
            1 => ASK_DEFAULT.to_vec(),
            2 => vec![
                OpenCodeRule::new("edit", "allow"),
                OpenCodeRule::new("shell", "ask"),
                OpenCodeRule::new("external_directory", "ask"),
            ],
            _ => vec![OpenCodeRule::new("*", "allow")],
        },
    };
    if deny_edits {
        rules.push(OpenCodeRule::new("edit", "deny"));
    }
    rules
}

/// The logical flag carrying the permission ceiling to
/// [`opencode_launch_command`], which turns it into permission rules. Spelled
/// like Claude's real flag; it never reaches the OpenCode argv.
const OPENCODE_PERMISSION_FLAG: &str = "--permission-mode";

/// The logical flag [`with_edits_denied`] adds for [`opencode_launch_command`]
/// to append an `edit` deny. It never reaches the OpenCode argv.
const OPENCODE_DENY_EDITS_FLAG: &str = "--shelbi-deny-edits";

/// The logical flag [`RunnerAdapter::with_reasoning_effort`] carries an
/// OpenCode launch's effort on, for [`RunnerAdapter::opencode_config`] to
/// turn into the model's `#variant`. It never reaches the OpenCode argv.
const OPENCODE_EFFORT_FLAG: &str = "--shelbi-effort";

/// OpenCode's switch that approves every ask the config doesn't deny.
const OPENCODE_AUTO_FLAG: &str = "--auto";

/// OpenCode's flag naming the session to reopen, or to create when it
/// doesn't exist.
const OPENCODE_SESSION_FLAGS: &[&str] = &["--session", "-s"];

/// OpenCode's "newest session in this directory" flags, which a named
/// session replaces.
const OPENCODE_CONTINUE_FLAGS: &[&str] = &["--continue", "-c"];

/// The logical flag [`with_opencode_session`] carries the session title on.
/// It becomes [`OPENCODE_TITLE_ENV`] and never reaches the OpenCode argv.
const OPENCODE_TITLE_FLAG: &str = "--shelbi-session-title";

/// Environment variable naming the OpenCode session Shelbi launched the pane
/// on. The plugin acts only on that session and its subagents.
pub const OPENCODE_SESSION_ENV: &str = "SHELBI_OPENCODE_SESSION";

/// Environment variable carrying the title the plugin gives that session.
pub const OPENCODE_TITLE_ENV: &str = "SHELBI_OPENCODE_TITLE";

/// The logical flag [`with_opencode_role`] carries the plugin's role on. It
/// becomes [`OPENCODE_ROLE_ENV`] and never reaches the OpenCode argv.
const OPENCODE_ROLE_FLAG: &str = "--shelbi-opencode-role";

/// Environment variable telling Shelbi's plugin which role the pane plays.
/// Unset is a worker.
pub const OPENCODE_ROLE_ENV: &str = "SHELBI_OPENCODE_ROLE";

/// The [`OPENCODE_ROLE_ENV`] value of the orchestrator's pane: its plugin
/// runs `shelbi orchestrator events stream` and the `shelbi orchestrator
/// hook` calls, the awareness contract Claude's `shelbi-awareness` plugin
/// runs.
pub const OPENCODE_ORCHESTRATOR_ROLE: &str = "orchestrator";

/// Return a copy of `spec` whose launch tells Shelbi's plugin to play
/// `role` ([`OPENCODE_ROLE_ENV`]), replacing any role already set. Other
/// runners get `spec` back unchanged.
pub fn with_opencode_role(spec: &AgentRunnerSpec, role: &str) -> AgentRunnerSpec {
    if !RunnerAdapter::for_spec(spec).is_opencode() {
        return spec.clone();
    }
    let mut out = spec.clone();
    out.flags = strip_flag_with_value(&spec.flags, &[OPENCODE_ROLE_FLAG]);
    out.flags.extend([OPENCODE_ROLE_FLAG.into(), role.into()]);
    out
}

/// Return a copy of `spec` that launches OpenCode on the named session:
/// `--session <id>` (which reopens it, or creates it on a first launch) plus
/// the title the plugin gives it. Any session or `--continue` flag already in
/// `spec` is replaced, since the named session is the one Shelbi tracks.
/// Other runners get `spec` back unchanged.
///
/// The id comes from [`shelbi_core::opencode_session_id`]; the caller keeps
/// the generation that picks it, so a resume passes the same id and a fresh
/// start a new one.
pub fn with_opencode_session(spec: &AgentRunnerSpec, id: &str, title: &str) -> AgentRunnerSpec {
    if !RunnerAdapter::for_spec(spec).is_opencode() {
        return spec.clone();
    }
    let mut names: Vec<&str> = OPENCODE_SESSION_FLAGS.to_vec();
    names.push(OPENCODE_TITLE_FLAG);
    let mut out = spec.clone();
    out.flags = strip_flag_with_value(&spec.flags, &names)
        .into_iter()
        .filter(|f| !OPENCODE_CONTINUE_FLAGS.contains(&f.as_str()))
        .collect();
    out.flags.extend(["--session".into(), id.into(), OPENCODE_TITLE_FLAG.into(), title.into()]);
    out
}

/// OpenCode's TUI config for a Shelbi pane: no terminal title (it would
/// overwrite the `shelbi:<state>` markers) and no highlights.
pub const OPENCODE_CLI_CONFIG_CONTENT: &str = r#"{"terminal":{"title":false},"highlights":false}"#;

/// Return a copy of `spec` that denies file edits, for an agent whose
/// settings deny Claude's edit tools (the review agent's no-edit posture).
/// Claude enforces that deny from the agent's `settings.json` itself, so this
/// only changes an OpenCode runner, whose launch then appends an `edit` deny
/// rule (see [`opencode_permission_rules`]). Idempotent.
pub fn with_edits_denied(spec: &AgentRunnerSpec, deny: bool) -> AgentRunnerSpec {
    if !deny
        || !RunnerAdapter::for_spec(spec).is_opencode()
        || spec.flags.iter().any(|f| f == OPENCODE_DENY_EDITS_FLAG)
    {
        return spec.clone();
    }
    let mut out = spec.clone();
    out.flags.push(OPENCODE_DENY_EDITS_FLAG.into());
    out
}

/// The value of the last `--flag value` / `--flag=value` among `names`.
fn flag_value(flags: &[String], names: &[&str]) -> Option<String> {
    let mut found = None;
    let mut iter = flags.iter();
    while let Some(f) = iter.next() {
        if names.iter().any(|n| f == n) {
            found = iter.next().cloned();
        } else if let Some(v) = names.iter().find_map(|n| f.strip_prefix(&format!("{n}="))) {
            found = Some(v.to_string());
        }
    }
    found
}

/// Environment variable carrying the pane's tty path into an OpenCode
/// session, where Shelbi's plugin writes its title markers.
pub const OPENCODE_PANE_TTY_ENV: &str = "SHELBI_PANE_TTY";

/// OpenCode flag that runs a private server for this TUI rather than
/// attaching to the shared background service.
const OPENCODE_STANDALONE_FLAG: &str = "--standalone";

/// How Shelbi restores a runner's prior context when a workspace is resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeStrategy {
    /// Reload the most recent transcript in the pane's cwd (`claude
    /// --continue`) — the strongest resume Shelbi can drive.
    Transcript,
    /// Reopen the session Shelbi named at launch (`opencode --session <id>`,
    /// see [`with_opencode_session`]): exactly that conversation, never
    /// whichever session in the worktree is newest.
    NamedSession,
    /// Re-open the runner's own native conversation thread (the Codex bridge's
    /// persisted owned thread).
    NativeThread,
    /// No transcript/thread affordance: relaunch cold and let the agent
    /// re-read its own prior work from the worktree.
    ColdBanner,
}

/// Session-scoped path used to expose Shelbi's reserved orchestrator plugin.
///
/// The adapter owns the runner distinction while the orchestrator crate owns
/// filesystem materialization and Codex app-server RPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorPluginInjection {
    /// Claude Code loads the isolated bundle with `--plugin-dir`.
    ClaudePluginDir,
    /// Codex app-server discovers the bundle's skill directory through its
    /// non-persistent `skills/extraRoots/set` request.
    CodexNativeSkillRoot,
    /// Older Codex app-servers receive the same SKILL.md bytes as developer
    /// instructions for the new/resumed owned thread.
    CodexDeveloperInstructions,
    /// Generic/custom runners are intentionally outside the system plugin.
    None,
}

/// The single per-runner integration adapter.
///
/// Constructed once by detection — an explicit `integration:` field on the
/// runner spec, else the executable basename — and then consulted for every
/// runner-specific decision: launch-flag assembly ([`with_permission_mode`],
/// [`with_continue`]), message channel, resume strategy, capability ladder,
/// and the readiness/submit gating that higher crates dispatch off
/// [`kind`](RunnerAdapter::kind). Confining the `claude` / `codex`
/// classification behind this constructor is the whole point: callers reason
/// about the adapter, never re-derive the basename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerAdapter {
    kind: RunnerKind,
}

impl RunnerAdapter {
    /// Build the adapter for a runner spec — honoring an explicit
    /// `integration:` field, else auto-detecting from the command basename.
    pub fn for_spec(spec: &AgentRunnerSpec) -> Self {
        Self {
            kind: RunnerKind::detect(spec),
        }
    }

    /// Build the adapter from a bare command string, with no explicit-field
    /// override available. Prefer [`for_spec`](RunnerAdapter::for_spec)
    /// wherever the full spec is in hand so the `integration:` override is
    /// honored.
    pub fn for_command(command: &str) -> Self {
        Self {
            kind: RunnerKind::from_command(command),
        }
    }

    /// The detected runner kind.
    pub fn kind(self) -> RunnerKind {
        self.kind
    }

    /// Whether this runner is Claude Code.
    pub fn is_claude(self) -> bool {
        self.kind == RunnerKind::Claude
    }

    /// Whether this runner is Codex.
    pub fn is_codex(self) -> bool {
        self.kind == RunnerKind::Codex
    }

    /// Whether this runner is OpenCode.
    pub fn is_opencode(self) -> bool {
        self.kind == RunnerKind::OpenCode
    }

    /// The per-contract transport tiers Shelbi has with this runner.
    pub fn capabilities(self) -> CapabilityLadder {
        self.kind.capabilities()
    }

    /// How a resumed workspace on this runner recovers its prior context.
    pub fn resume_strategy(self) -> ResumeStrategy {
        match self.kind {
            RunnerKind::Claude => ResumeStrategy::Transcript,
            RunnerKind::OpenCode => ResumeStrategy::NamedSession,
            RunnerKind::Codex => ResumeStrategy::NativeThread,
            RunnerKind::Generic => ResumeStrategy::ColdBanner,
        }
    }

    /// Which delivery channel this runner uses for hub→workspace messages.
    /// See [`message_channel`] for the health basis behind each runner's mode.
    pub fn message_channel(self) -> MessageChannel {
        match self.kind {
            RunnerKind::Claude | RunnerKind::OpenCode => MessageChannel::Hooks,
            RunnerKind::Codex | RunnerKind::Generic => MessageChannel::Polling,
        }
    }

    /// Does this runner pull hub→workspace messages by polling the log itself?
    pub fn polls_for_messages(self) -> bool {
        matches!(self.message_channel(), MessageChannel::Polling)
    }

    /// Whether the startup path should run Claude's bordered-composer
    /// readiness probe before typing the prompt. Only Claude has one; every
    /// other runner is delivered to without the probe.
    pub fn needs_claude_readiness_probe(self) -> bool {
        self.is_claude()
    }

    /// Select the system-plugin transport for a built-in orchestrator.
    ///
    /// `codex_native_supported` is the result of the app-server capability
    /// probe. It is ignored for non-Codex runners.
    pub fn orchestrator_plugin_injection(
        self,
        codex_native_supported: bool,
    ) -> OrchestratorPluginInjection {
        match self.kind {
            RunnerKind::Claude => OrchestratorPluginInjection::ClaudePluginDir,
            RunnerKind::Codex if codex_native_supported => {
                OrchestratorPluginInjection::CodexNativeSkillRoot
            }
            RunnerKind::Codex => OrchestratorPluginInjection::CodexDeveloperInstructions,
            RunnerKind::OpenCode | RunnerKind::Generic => OrchestratorPluginInjection::None,
        }
    }

    /// Add Claude's session-scoped plugin directory without changing the
    /// runner-owned global registry. Other runners are returned unchanged.
    pub fn with_orchestrator_plugin_dir(
        self,
        spec: &AgentRunnerSpec,
        plugin_dir: &str,
    ) -> AgentRunnerSpec {
        if !matches!(
            self.orchestrator_plugin_injection(false),
            OrchestratorPluginInjection::ClaudePluginDir
        ) {
            return spec.clone();
        }
        let mut out = spec.clone();
        out.flags.push("--plugin-dir".into());
        out.flags.push(plugin_dir.into());
        out
    }

    /// Return a copy of `spec` with `--permission-mode <mode>` appended when
    /// this runner is `claude` and the mode is non-default, or carrying the
    /// mode to OpenCode's permission rules. See the free
    /// [`with_permission_mode`] wrapper for the full rationale; this is the
    /// adapter-scoped form that skips re-detecting the runner kind.
    pub fn with_permission_mode(self, spec: &AgentRunnerSpec, mode: Option<&str>) -> AgentRunnerSpec {
        // OpenCode's own default allows everything, so every configured mode,
        // `default` included, becomes permission rules; only `None` (nothing
        // configured) leaves the user's OpenCode settings in charge. The mode
        // rides a logical `--permission-mode` that `opencode_launch_command`
        // turns into rules, replacing one a user wrote: the resolved ceiling
        // is the authority.
        if self.is_opencode() {
            let Some(mode) = mode else {
                return spec.clone();
            };
            let mut out = spec.clone();
            out.flags = strip_flag_with_value(&spec.flags, &[OPENCODE_PERMISSION_FLAG]);
            out.flags.push(OPENCODE_PERMISSION_FLAG.into());
            out.flags.push(mode.into());
            return out;
        }
        // `None` (nothing configured) and `Some("default")` (claude's own
        // baseline) both mean "pass no flag" — the agent runs under the user's
        // `permissions.defaultMode`. A flag is appended only for claude and only
        // for a concrete, non-default mode.
        let mode = match mode {
            Some(m) if m != "default" => m,
            _ => return spec.clone(),
        };
        if !self.is_claude() {
            return spec.clone();
        }
        if spec
            .flags
            .iter()
            .any(|f| f == "--permission-mode" || f.starts_with("--permission-mode="))
        {
            return spec.clone();
        }
        let mut out = spec.clone();
        out.flags.push("--permission-mode".into());
        out.flags.push(mode.into());
        out
    }

    /// Return a copy of `spec` with the resolved `model` injected as the
    /// runner's model-selection flag. This is the adapter's job — a `model`
    /// like `claude-opus-4-8` is a *logical* value; the flag that carries it is
    /// runner-specific (`--model` for Claude and Codex; nothing for a generic
    /// runner Shelbi can't drive). When the resolved model takes precedence over
    /// a model already baked into `flags` (the legacy `--model` in `agent_runners`
    /// form), the stale flag pair is stripped first so the launched line carries
    /// exactly one model. `None` (nothing resolved) returns the spec unchanged so
    /// a project still relying on model-in-flags keeps working.
    pub fn with_model(self, spec: &AgentRunnerSpec, model: Option<&str>) -> AgentRunnerSpec {
        let Some(model) = model else {
            return spec.clone();
        };
        let flag = match self.kind {
            // Claude and Codex both select the model with `--model <id>`.
            // OpenCode's TUI has no model flag, so its `--model` is logical:
            // [`launch_command`] moves it into `OPENCODE_CONFIG_CONTENT`.
            RunnerKind::Claude | RunnerKind::Codex | RunnerKind::OpenCode => "--model",
            // A generic runner has no model contract Shelbi knows how to drive.
            RunnerKind::Generic => return spec.clone(),
        };
        let mut out = spec.clone();
        out.flags = strip_flag_with_value(&out.flags, &["--model", "-m"]);
        out.flags.push(flag.into());
        out.flags.push(model.into());
        out
    }

    /// Return a copy of `spec` with the resolved `reasoning_effort` injected as
    /// the runner's effort knob. Per-adapter mapping: Claude takes `--effort
    /// <level>` (its `low`/`medium`/`high`/`max` scale lines up 1:1); Codex takes
    /// a config override `-c model_reasoning_effort="<value>"`, and since Codex's
    /// top tier is `xhigh` the `Max` level lands there. A generic runner has no
    /// effort contract, so it's a no-op. `None` returns the spec unchanged.
    /// Idempotent-ish: if the same effort flag is already present it isn't added
    /// twice.
    pub fn with_reasoning_effort(
        self,
        spec: &AgentRunnerSpec,
        effort: Option<ReasoningEffort>,
    ) -> AgentRunnerSpec {
        let Some(effort) = effort else {
            return spec.clone();
        };
        match self.kind {
            RunnerKind::Claude => {
                if spec
                    .flags
                    .iter()
                    .any(|f| f == "--effort" || f.starts_with("--effort="))
                {
                    return spec.clone();
                }
                let mut out = spec.clone();
                out.flags.push("--effort".into());
                out.flags.push(effort.as_str().into());
                out
            }
            RunnerKind::Codex => {
                let value = codex_effort_value(effort);
                let override_arg = format!("model_reasoning_effort=\"{value}\"");
                if spec.flags.iter().any(|f| f == &override_arg) {
                    return spec.clone();
                }
                let mut out = spec.clone();
                out.flags.push("-c".into());
                out.flags.push(override_arg);
                out
            }
            // OpenCode's effort is a per-model variant (`provider/model#high`)
            // in the config layer, and only one OpenCode reports for that
            // model counts (see `opencode_effort_variant`). The effort rides
            // a logical flag until `opencode_config` has the model list.
            RunnerKind::OpenCode => {
                let mut out = spec.clone();
                out.flags = strip_flag_with_value(&spec.flags, &[OPENCODE_EFFORT_FLAG]);
                out.flags.push(OPENCODE_EFFORT_FLAG.into());
                out.flags.push(effort.as_str().into());
                out
            }
            RunnerKind::Generic => spec.clone(),
        }
    }

    /// Return a copy of `spec` with `--continue` appended when this runner is
    /// `claude` and `resume` is set. See the free [`with_continue`] wrapper
    /// for the full rationale.
    pub fn with_continue(self, spec: &AgentRunnerSpec, resume: bool) -> AgentRunnerSpec {
        // OpenCode spells it `--continue` too, but there it means the newest
        // session in the directory, which can be the user's own. Its resume
        // names the session instead (`with_opencode_session`).
        if !resume || !self.is_claude() {
            return spec.clone();
        }
        if spec
            .flags
            .iter()
            .any(|f| f == "--continue" || f == "-c" || f == "--resume")
        {
            return spec.clone();
        }
        let mut out = spec.clone();
        out.flags.push("--continue".into());
        out
    }
}

/// Return a copy of `spec` with `--permission-mode <mode>` appended when the
/// runner is `claude` and a concrete, non-default mode is given. Passing the
/// mode on the command line is the authoritative signal; relying on
/// `settings.json`'s `defaultMode` is fragile (silent fallback to interactive
/// on any I/O race or version regression).
///
/// `mode` is `Option`: **`None` means nothing was configured**, so shelbi
/// passes no flag and the agent runs under the user's own
/// `permissions.defaultMode`. `Some("default")` (claude's baseline) is treated
/// the same way.
///
/// OpenCode is the exception on both counts: its default agent allows
/// everything, so any configured mode (`default` included) is carried, as a
/// logical flag, into permission rules in its launch config (see
/// [`opencode_permission_rules`]). Every other runner gets the spec back
/// unchanged.
///
/// Idempotent: if the user-authored YAML already includes `--permission-mode`
/// in `flags` (common for projects that adopted the flag before this helper
/// existed), the spec is returned unchanged so the launched command line
/// doesn't end up with two copies. Two copies don't break claude — the
/// right-most wins — but they clutter pane captures and obscure which mode
/// the workspace is actually running in.
pub fn with_permission_mode(spec: &AgentRunnerSpec, mode: Option<&str>) -> AgentRunnerSpec {
    RunnerAdapter::for_spec(spec).with_permission_mode(spec, mode)
}

/// Return a copy of `spec` with `--continue` appended when the runner is
/// `claude` and `resume` is set (OpenCode resumes through
/// [`with_opencode_session`] instead). `claude --continue` reloads the most recent
/// conversation in the pane's working directory, so a resumed workspace picks
/// up mid-thought with its full prior context — the session transcript lives
/// under the user's `~/.claude/` and survives a killed agent session or a
/// recreated worktree (the cwd path is stable). This is the strongest resume
/// semantics shelbi can offer, so `shelbi task resume` prefers it for claude
/// and falls back to plain prompt re-injection for every other runner (which
/// has no equivalent transcript-resume flag shelbi knows how to drive).
///
/// Non-claude runners and `resume == false` return the spec unchanged.
/// Idempotent: a YAML that already carries `--continue` / `-c` / `--resume`
/// in `flags` isn't given a second copy.
pub fn with_continue(spec: &AgentRunnerSpec, resume: bool) -> AgentRunnerSpec {
    RunnerAdapter::for_spec(spec).with_continue(spec, resume)
}

/// Free-function form of [`RunnerAdapter::with_model`] — detects the runner
/// kind from `spec`, then injects the resolved model as the runner's
/// model-selection flag. `None` returns the spec unchanged.
pub fn with_model(spec: &AgentRunnerSpec, model: Option<&str>) -> AgentRunnerSpec {
    RunnerAdapter::for_spec(spec).with_model(spec, model)
}

/// Free-function form of [`RunnerAdapter::with_reasoning_effort`].
pub fn with_reasoning_effort(
    spec: &AgentRunnerSpec,
    effort: Option<ReasoningEffort>,
) -> AgentRunnerSpec {
    RunnerAdapter::for_spec(spec).with_reasoning_effort(spec, effort)
}

/// Codex's `model_reasoning_effort` accepts `minimal|low|medium|high|xhigh`;
/// Shelbi's scale tops out at `Max`, which maps onto Codex's highest tier
/// (`xhigh`). The rest map 1:1.
fn codex_effort_value(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => "xhigh",
    }
}

/// Remove any `--flag value` (or `--flag=value`) pairs whose flag name matches
/// one of `names`, preserving the order of everything else. Used to drop a
/// stale model flag before injecting the resolved one so a launched command
/// never carries two `--model`s.
fn strip_flag_with_value(flags: &[String], names: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(flags.len());
    let mut skip_next = false;
    for f in flags {
        if skip_next {
            skip_next = false;
            continue;
        }
        // `--flag=value` — a single token; drop it outright.
        if names.iter().any(|n| f == n || f.starts_with(&format!("{n}="))) {
            // Bare `--flag` consumes the following value token too.
            if names.iter().any(|n| f == n) {
                skip_next = true;
            }
            continue;
        }
        out.push(f.clone());
    }
    out
}

/// How a runner receives hub→workspace messages (a `shelbi message`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageChannel {
    /// Shelbi wires runner hooks it has verified are healthy on this runner,
    /// so the pane pushes new messages into the agent itself and the
    /// workspace prompt carries no polling contract.
    Hooks,
    /// No verified-healthy hook channel exists for this runner, so the
    /// workspace prompt must instruct the agent to tail
    /// `.shelbi/messages/<task-id>.log` itself on a concrete cadence and ack
    /// each line.
    Polling,
}

impl MessageChannel {
    /// Stable token for a dispatch event / status detail line.
    pub fn as_str(self) -> &'static str {
        match self {
            MessageChannel::Hooks => "hooks",
            MessageChannel::Polling => "polling",
        }
    }
}

/// Resolve which delivery channel a runner uses, keyed off a *verified hook
/// health* signal rather than the executable basename alone.
///
/// - **Claude** receives messages through hooks Shelbi wires in
///   `.claude/settings.json` — a Shelbi-owned config layer — and that push
///   path is the proven, shipped default, so claude resolves to
///   [`MessageChannel::Hooks`].
/// - **OpenCode** loads Shelbi's plugin from the worktree's
///   `.shelbi/opencode/plugins/shelbi/` (named in the launch's config layer,
///   see [`RunnerAdapter::opencode_config`]), which runs the same hook scripts on OpenCode's
///   session events and pushes the drained batch back in as a synthetic
///   message, so it resolves to [`MessageChannel::Hooks`] too.
/// - **Codex and every other runner** resolve to [`MessageChannel::Polling`].
///   The installed Codex CLI rejects the `-c core.hooksPath=<path>` override
///   Shelbi previously passed (`unknown configuration field 'core'` under
///   strict validation), and Codex only discovers hooks from `~/.codex/` or
///   `<repo>/.codex/` (both user-owned, which Shelbi must not overwrite) or
///   plugin manifests. With no supported, non-destructive hook layer whose
///   handshake Shelbi has verified — and Codex `Stop` hooks additionally
///   requiring JSON output the shared drain script does not emit — there is no
///   verified-healthy hook channel to wire, so Codex polls until one is proven.
///
/// Keep this the single decision point: `polls_for_messages` and the
/// per-launch mode log both derive from it, so hooks and polling can never
/// disagree about a runner.
pub fn message_channel(spec: &AgentRunnerSpec) -> MessageChannel {
    RunnerAdapter::for_spec(spec).message_channel()
}

/// Does this runner pull hub→workspace messages by polling the log itself?
///
/// The complement of a verified-healthy hook channel: any runner Shelbi has
/// not proven can push messages through hooks must be told to poll. See
/// [`message_channel`] for the health basis behind each runner's mode.
pub fn polls_for_messages(spec: &AgentRunnerSpec) -> bool {
    matches!(message_channel(spec), MessageChannel::Polling)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recursively collect every `.rs` file under `dir`.
    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// True when any component of `path` is a `tests` directory — i.e. the file
    /// is a Cargo integration-test target, which is entirely test code.
    fn is_integration_test_file(path: &std::path::Path) -> bool {
        path.components().any(|c| c.as_os_str() == "tests")
    }

    /// Return `text` with the bodies of `#[cfg(test)]` modules removed, so the
    /// runner-basename guard only inspects production code.
    ///
    /// Test modules routinely carry config-*data* assertions such as
    /// `assert_eq!(value.as_str(), Some("claude"))` that are string-equality
    /// checks, not runner-basename *dispatch* branches. The bare-substring grep
    /// can't tell the two apart, so scanning test code produced false positives
    /// (the codify migration and the config-upgrade slice-2 tests each bounced a
    /// review over a `Some("claude")` assertion that violated nothing). Stripping
    /// the test modules first keeps the guard pointed at the code it actually
    /// governs. Brace-depth counting is a heuristic, which is all this guard
    /// needs — a `#[cfg(test)]` attribute by definition marks test-only code.
    fn strip_cfg_test_modules(text: &str) -> String {
        let mut out = String::new();
        let mut in_test = false;
        let mut depth: i32 = 0;
        let mut saw_open = false;
        for line in text.lines() {
            if !in_test {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    in_test = true;
                    depth = 0;
                    saw_open = false;
                    // Fall through to brace counting; the attribute line itself
                    // rarely opens the module body.
                } else {
                    out.push_str(line);
                    out.push('\n');
                    continue;
                }
            }
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        saw_open = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if saw_open {
                // Module body closed once braces balance back out.
                if depth <= 0 {
                    in_test = false;
                }
            } else if line.contains(';') {
                // A single-item `#[cfg(test)]` attribute (e.g. `#[cfg(test)] use
                // ...;` or `mod tests;`) with no inline body — nothing to strip
                // past this line.
                in_test = false;
            }
            // Lines inside the test module are dropped from `out`.
        }
        out
    }

    /// Grep-guard: the runner *basename* classification is allowed to live in
    /// exactly one place — [`shelbi_core::RunnerKind::from_command`] in
    /// `shelbi-core/src/model.rs`. Every other module must reason about a
    /// [`RunnerAdapter`] / [`RunnerKind`] instead of re-deriving `claude` /
    /// `codex` from the command string. This keeps the consolidation from
    /// silently eroding back into scattered branches.
    ///
    /// The basename check applies to *production* code only: test modules and
    /// integration-test files hold config-data assertions of the same textual
    /// shape (`Some("claude")`) that are not dispatch checks, so they are
    /// stripped/skipped before the grep (see [`strip_cfg_test_modules`]).
    #[test]
    fn runner_basename_checks_are_confined_to_adapter_detection() {
        // The crates workspace root: `.../crates/shelbi-agent` → `.../crates`.
        let crates_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("shelbi-agent lives under the crates dir")
            .to_path_buf();
        let mut files = Vec::new();
        rust_sources(&crates_root, &mut files);
        assert!(
            files.len() > 20,
            "expected to walk the whole crates tree, only found {} files",
            files.len()
        );

        // Built at runtime so this guard's own source doesn't trip it.
        let removed_helpers = [
            format!("is_claude{}", "_runner"),
            format!("is_codex{}", "_runner"),
        ];
        let basename_needles = [
            format!("Some({:?})", "claude"),
            format!("Some({:?})", "codex"),
        ];
        let allowed = std::path::Path::new("shelbi-core")
            .join("src")
            .join("model.rs");

        for file in &files {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            for needle in &removed_helpers {
                assert!(
                    !text.contains(needle.as_str()),
                    "`{needle}` was removed in favor of RunnerAdapter but still \
                     appears in {}",
                    file.display()
                );
            }
            if file.ends_with(&allowed) || is_integration_test_file(file) {
                continue;
            }
            let production = strip_cfg_test_modules(&text);
            for needle in &basename_needles {
                assert!(
                    !production.contains(needle.as_str()),
                    "runner basename check `{needle}` must be confined to \
                     RunnerKind::from_command, but appears in production code in {}",
                    file.display()
                );
            }
        }
    }

    /// The confinement guard must catch a real basename dispatch branch in
    /// production code while ignoring the identical textual shape when it is a
    /// config-data assertion inside a `#[cfg(test)]` module — the false-positive
    /// class that bounced two reviews.
    #[test]
    fn basename_guard_is_confined_to_production_code() {
        let needle = format!("Some({:?})", "claude");

        // Direction 1 — a genuine runner-basename dispatch branch in production
        // code is still visible to the grep after stripping.
        let production_dispatch = "\
fn detect(command: &str) -> RunnerKind {
    match std::path::Path::new(command).file_name().and_then(|s| s.to_str()) {
        Some(\"claude\") => RunnerKind::Claude,
        _ => RunnerKind::Generic,
    }
}
";
        assert!(
            strip_cfg_test_modules(production_dispatch).contains(&needle),
            "a real basename dispatch branch in production code must still be caught"
        );

        // Direction 2 — the same textual shape as a data assertion inside a
        // `#[cfg(test)]` module is stripped and does not trip the guard.
        let test_only = "\
fn detect(_command: &str) -> RunnerKind {
    RunnerKind::Generic
}

#[cfg(test)]
mod tests {
    #[test]
    fn integration_field_round_trips() {
        assert_eq!(value.as_str(), Some(\"claude\"));
    }
}
";
        assert!(
            !strip_cfg_test_modules(test_only).contains(&needle),
            "a Some(\"claude\") data assertion inside a #[cfg(test)] module must \
             not trip the guard"
        );
    }

    #[test]
    fn launch_command_minimal() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        assert_eq!(launch_command(&spec), "claude");
    }

    #[test]
    fn launch_command_with_flags() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into(), "thinking".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        assert_eq!(launch_command(&spec), "codex --print thinking");
    }

    #[test]
    fn orchestrator_plugin_transport_is_runner_scoped() {
        let claude = RunnerAdapter::for_command("claude");
        let codex = RunnerAdapter::for_command("codex");
        let custom = RunnerAdapter::for_command("aider");

        assert_eq!(
            claude.orchestrator_plugin_injection(false),
            OrchestratorPluginInjection::ClaudePluginDir
        );
        assert_eq!(
            codex.orchestrator_plugin_injection(true),
            OrchestratorPluginInjection::CodexNativeSkillRoot
        );
        assert_eq!(
            codex.orchestrator_plugin_injection(false),
            OrchestratorPluginInjection::CodexDeveloperInstructions
        );
        assert_eq!(
            custom.orchestrator_plugin_injection(true),
            OrchestratorPluginInjection::None
        );
    }

    #[test]
    fn claude_orchestrator_plugin_dir_is_session_scoped_and_preserves_registry() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = RunnerAdapter::for_spec(&spec)
            .with_orchestrator_plugin_dir(&spec, ".claude/shelbi-system-plugin");
        assert_eq!(
            out.flags,
            vec![
                "--permission-mode",
                "auto",
                "--plugin-dir",
                ".claude/shelbi-system-plugin"
            ]
        );
        assert_eq!(spec.flags, vec!["--permission-mode", "auto"]);
    }

    #[test]
    fn with_permission_mode_appends_for_claude() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        assert_eq!(out.command, "claude");
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
        assert_eq!(launch_command(&out), "claude --permission-mode auto");
    }

    #[test]
    fn with_permission_mode_preserves_existing_flags() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--dangerously-skip-permissions".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("acceptEdits"));
        assert_eq!(
            out.flags,
            vec![
                "--dangerously-skip-permissions",
                "--permission-mode",
                "acceptEdits"
            ]
        );
    }

    #[test]
    fn with_permission_mode_resolves_absolute_claude_paths() {
        // A project might pin claude to /opt/homebrew/bin/claude; the helper
        // should still recognize it by basename.
        let spec = AgentRunnerSpec {
            command: "/opt/homebrew/bin/claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
    }

    #[test]
    fn with_permission_mode_skips_non_claude_runners() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        // Codex doesn't understand --permission-mode; leave it alone.
        assert_eq!(out.flags, vec!["--print"]);
    }

    #[test]
    fn with_permission_mode_skips_default_mode() {
        // `default` is claude's own baseline; passing the flag is redundant
        // and could surprise a user who reads the launched command line.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("default"));
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_permission_mode_none_defers_and_passes_no_flag() {
        // `None` means nothing was configured anywhere: shelbi must pass no
        // --permission-mode so claude reads the user's own defaultMode.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, None);
        assert!(out.flags.is_empty());
        assert_eq!(launch_command(&out), "claude");
    }

    #[test]
    fn with_permission_mode_idempotent_when_yaml_already_has_flag() {
        // The shelbi project YAML kept `flags: [--permission-mode, auto]` as a
        // pre-bd7a23f quick fix; after bd7a23f added with_permission_mode the
        // spawn path was producing `claude --permission-mode auto
        // --permission-mode auto`. Detect the existing flag and skip the
        // append so the launched command line stays clean.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
        assert_eq!(launch_command(&out), "claude --permission-mode auto");
    }

    #[test]
    fn with_permission_mode_idempotent_for_equals_form() {
        // YAML may pin the single-token spelling (`--permission-mode=plan`).
        // The helper must recognize it too; otherwise it appends the
        // two-token form, the rightmost copy wins, and the workspace runs
        // in a mode the user explicitly configured away from.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode=plan".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        assert_eq!(out.flags, vec!["--permission-mode=plan"]);
    }

    #[test]
    fn with_permission_mode_idempotent_even_when_yaml_mode_differs() {
        // If the YAML pins a specific mode, respect it rather than silently
        // overriding from project.workspace_permissions_mode. An explicit flag
        // in the YAML is intentional configuration; quiet override would be
        // surprising.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "plan".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_permission_mode(&spec, Some("auto"));
        assert_eq!(out.flags, vec!["--permission-mode", "plan"]);
    }

    fn runner(command: &str) -> AgentRunnerSpec {
        AgentRunnerSpec {
            command: command.into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        }
    }

    #[test]
    fn claude_does_not_poll_for_messages() {
        assert!(!polls_for_messages(&runner("claude")));
        assert!(!polls_for_messages(&runner("/usr/local/bin/claude")));
    }

    #[test]
    fn adapter_honors_explicit_integration_field() {
        // A wrapper executable whose basename is unrecognized, pinned to the
        // Claude adapter, drives every Claude-only decision.
        let mut wrapped = runner("my-claude-wrapper");
        wrapped.integration = Some(RunnerKind::Claude);
        let adapter = RunnerAdapter::for_spec(&wrapped);
        assert!(adapter.is_claude());
        assert_eq!(adapter.message_channel(), MessageChannel::Hooks);
        assert_eq!(adapter.resume_strategy(), ResumeStrategy::Transcript);
        assert!(adapter.needs_claude_readiness_probe());
        // Launch-flag assembly follows the adapter, not the basename.
        assert_eq!(
            with_permission_mode(&wrapped, Some("auto")).flags,
            vec!["--permission-mode", "auto"]
        );

        // Detection strategy for each kind.
        assert_eq!(
            RunnerAdapter::for_command("codex").resume_strategy(),
            ResumeStrategy::NativeThread
        );
        assert_eq!(
            RunnerAdapter::for_command("aider").resume_strategy(),
            ResumeStrategy::ColdBanner
        );
    }

    #[test]
    fn codex_polls_for_messages_without_a_verified_hook_channel() {
        // The installed Codex CLI rejects the `-c core.hooksPath` override and
        // exposes no non-destructive hook layer Shelbi can wire, so Codex must
        // fall back to prompt-level polling rather than be silently classified
        // hook-capable by basename.
        assert!(polls_for_messages(&runner("codex")));
        assert!(polls_for_messages(&runner("/opt/bin/codex")));
        assert_eq!(message_channel(&runner("codex")), MessageChannel::Polling);
    }

    #[test]
    fn claude_uses_the_hook_channel() {
        assert_eq!(message_channel(&runner("claude")), MessageChannel::Hooks);
        assert_eq!(
            message_channel(&runner("/usr/local/bin/claude")),
            MessageChannel::Hooks
        );
    }

    #[test]
    fn unknown_runners_poll_for_messages() {
        assert!(polls_for_messages(&runner("aider")));
        // No `.exe` special-casing: a Windows-style basename isn't "claude",
        // so it classifies as a polling runner — consistent with how
        // `with_permission_mode` keys off the exact basename.
        assert!(polls_for_messages(&runner("claude.exe")));
    }

    #[test]
    fn with_continue_appends_for_claude_when_resuming() {
        let out = with_continue(&runner("claude"), true);
        assert_eq!(out.flags, vec!["--continue"]);
        // Absolute claude path classifies the same.
        let out = with_continue(&runner("/usr/local/bin/claude"), true);
        assert_eq!(out.flags, vec!["--continue"]);
    }

    #[test]
    fn with_continue_is_noop_when_not_resuming() {
        let out = with_continue(&runner("claude"), false);
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_continue_skips_non_claude_runners() {
        // codex has no `--continue` shelbi knows how to drive; leave it alone.
        let out = with_continue(&runner("codex"), true);
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_continue_is_idempotent_when_flag_already_present() {
        for existing in ["--continue", "-c", "--resume"] {
            let spec = AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![existing.into()],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            };
            let out = with_continue(&spec, true);
            assert_eq!(
                out.flags,
                vec![existing],
                "should not double-add for {existing}"
            );
        }
    }

    #[test]
    fn with_continue_preserves_existing_flags() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let out = with_continue(&spec, true);
        assert_eq!(out.flags, vec!["--permission-mode", "auto", "--continue"]);
    }

    fn bare(command: &str) -> AgentRunnerSpec {
        AgentRunnerSpec {
            command: command.into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        }
    }

    #[test]
    fn with_model_injects_claude_model_flag() {
        let out = with_model(&bare("claude"), Some("claude-opus-4-8"));
        assert_eq!(out.flags, vec!["--model", "claude-opus-4-8"]);
    }

    #[test]
    fn with_model_injects_codex_model_flag() {
        let out = with_model(&bare("codex"), Some("gpt-5"));
        assert_eq!(out.flags, vec!["--model", "gpt-5"]);
    }

    #[test]
    fn with_model_none_leaves_spec_untouched() {
        // No resolved model → model-in-flags (legacy `agent_runners`) survives.
        let mut spec = bare("claude");
        spec.flags = vec!["--model".into(), "claude-opus-4-8".into()];
        let out = with_model(&spec, None);
        assert_eq!(out.flags, vec!["--model", "claude-opus-4-8"]);
    }

    #[test]
    fn with_model_replaces_legacy_model_in_flags() {
        // A resolved model takes precedence over a `--model` baked into flags;
        // the launched line must carry exactly one model.
        let mut spec = bare("claude");
        spec.flags = vec![
            "--model".into(),
            "claude-sonnet-5".into(),
            "--verbose".into(),
        ];
        let out = with_model(&spec, Some("claude-opus-4-8"));
        assert_eq!(out.flags, vec!["--verbose", "--model", "claude-opus-4-8"]);
    }

    #[test]
    fn with_model_is_noop_for_generic_runner() {
        let out = with_model(&bare("aider"), Some("gpt-5"));
        assert!(out.flags.is_empty(), "generic runner has no model contract");
    }

    #[test]
    fn with_reasoning_effort_injects_claude_effort_flag() {
        let out = with_reasoning_effort(&bare("claude"), Some(ReasoningEffort::High));
        assert_eq!(out.flags, vec!["--effort", "high"]);
    }

    #[test]
    fn with_reasoning_effort_maps_max_to_codex_xhigh() {
        let out = with_reasoning_effort(&bare("codex"), Some(ReasoningEffort::Max));
        assert_eq!(out.flags, vec!["-c", "model_reasoning_effort=\"xhigh\""]);
    }

    #[test]
    fn with_reasoning_effort_codex_low_medium_high_pass_through() {
        let out = with_reasoning_effort(&bare("codex"), Some(ReasoningEffort::Medium));
        assert_eq!(out.flags, vec!["-c", "model_reasoning_effort=\"medium\""]);
    }

    #[test]
    fn with_reasoning_effort_none_is_noop() {
        let out = with_reasoning_effort(&bare("claude"), None);
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_reasoning_effort_is_noop_for_generic_runner() {
        let out = with_reasoning_effort(&bare("aider"), Some(ReasoningEffort::High));
        assert!(out.flags.is_empty());
    }

    #[test]
    fn model_and_effort_compose_for_claude() {
        // The dispatch path applies model then effort; both reach the launched
        // Claude command line via the adapter.
        let out = with_reasoning_effort(
            &with_model(&bare("claude"), Some("claude-opus-4-8")),
            Some(ReasoningEffort::High),
        );
        assert_eq!(
            out.flags,
            vec!["--model", "claude-opus-4-8", "--effort", "high"]
        );
    }

    fn opencode(flags: &[&str]) -> AgentRunnerSpec {
        AgentRunnerSpec {
            command: "opencode".into(),
            flags: flags.iter().map(|f| f.to_string()).collect(),
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        }
    }

    /// The env every OpenCode launch carries ahead of the binary, after the
    /// optional `OPENCODE_CONFIG_CONTENT`.
    const OC_ENV: &str = r#"OPENCODE_CLI_CONFIG_CONTENT='{"terminal":{"title":false},"highlights":false}' OPENCODE_DISABLE_AUTOUPDATE=1"#;

    #[test]
    fn opencode_launch_exports_the_pane_tty_and_runs_standalone() {
        assert_eq!(
            launch_command(&opencode(&[])),
            format!(r#"env SHELBI_PANE_TTY="$(tty)" {OC_ENV} opencode --standalone"#)
        );
        // A flag the user already wrote isn't doubled, and with no ceiling a
        // hand-written `--auto` is the user's own choice.
        assert_eq!(
            launch_command(&opencode(&["--standalone", "--auto"])),
            format!(r#"env SHELBI_PANE_TTY="$(tty)" {OC_ENV} opencode --standalone --auto"#)
        );
    }

    /// The `OPENCODE_CONFIG_CONTENT` value a launch line sets inline, parsed.
    fn inline_config(line: &str) -> Option<serde_json::Value> {
        let rest = line.split("OPENCODE_CONFIG_CONTENT='").nth(1)?;
        let json = &rest[..rest.find("' ")?];
        Some(serde_json::from_str(json).unwrap())
    }

    /// What every Shelbi-set `OPENCODE_CONFIG_CONTENT` is preceded by.
    const KEEP_USER: &str =
        r#"SHELBI_OPENCODE_USER_CONFIG="${SHELBI_OPENCODE_USER_CONFIG-${OPENCODE_CONFIG_CONTENT-}}""#;

    #[test]
    fn opencode_model_leaves_the_argv_for_the_agents_in_inline_config() {
        let spec = with_model(&opencode(&[]), Some("anthropic/claude-sonnet-4-5"));
        assert_eq!(
            launch_command(&spec),
            format!(
                r#"env SHELBI_PANE_TTY="$(tty)" {KEEP_USER} OPENCODE_CONFIG_CONTENT='{{"agents":{{"build":{{"model":"anthropic/claude-sonnet-4-5"}},"plan":{{"model":"anthropic/claude-sonnet-4-5"}}}}}}' {OC_ENV} opencode --standalone"#
            )
        );
        // A hand-written `--model=` form is honored the same way, and a quote
        // in the id stays valid JSON.
        let spec = opencode(&["--model=we\"ird"]);
        let config = inline_config(&launch_command(&spec)).unwrap();
        assert_eq!(config["agents"]["build"]["model"], "we\"ird");
    }

    /// The pane-less layer pieces the worker launch fills in.
    fn worker_layer() -> OpenCodeLayer {
        OpenCodeLayer {
            instructions: vec!["/wt/.shelbi/opencode/instructions.md".into()],
            skills: vec!["/wt/.shelbi/opencode/skills".into()],
            plugins: vec!["/wt/.shelbi/opencode/plugins/shelbi".into()],
            ..OpenCodeLayer::default()
        }
    }

    fn config_json(spec: &AgentRunnerSpec, layer: &OpenCodeLayer) -> Option<serde_json::Value> {
        RunnerAdapter::for_spec(spec)
            .opencode_config(spec, layer)
            .json
            .map(|json| serde_json::from_str(&json).unwrap())
    }

    #[test]
    fn opencode_config_layer_emits_only_the_keys_shelbi_has() {
        // Nothing at all: no layer, so the user's settings stand.
        assert_eq!(config_json(&opencode(&[]), &OpenCodeLayer::default()), None);

        // A worker's files with no model and no ceiling: no `agents`.
        assert_eq!(
            config_json(&opencode(&[]), &worker_layer()),
            Some(serde_json::json!({
                "instructions": ["/wt/.shelbi/opencode/instructions.md"],
                "skills": ["/wt/.shelbi/opencode/skills"],
                "plugins": ["/wt/.shelbi/opencode/plugins/shelbi"],
            }))
        );

        // An agent without skills leaves `skills` out.
        let no_skills = OpenCodeLayer { skills: vec![], ..worker_layer() };
        let config = config_json(&opencode(&[]), &no_skills).unwrap();
        assert!(config.get("skills").is_none(), "{config}");

        // Model, variant and ceiling land on both agents.
        let spec = with_permission_mode(
            &with_reasoning_effort(
                &with_model(&opencode(&[]), Some("anthropic/claude-sonnet-5-5")),
                Some(ReasoningEffort::High),
            ),
            Some("acceptEdits"),
        );
        let layer = OpenCodeLayer { models: models(&[("anthropic/claude-sonnet-5-5", &["high"])]), ..worker_layer() };
        let config = config_json(&spec, &layer).unwrap();
        let agent = serde_json::json!({
            "model": "anthropic/claude-sonnet-5-5#high",
            "permissions": [
                {"action": "edit", "resource": "*", "effect": "allow"},
                {"action": "shell", "resource": "*", "effect": "ask"},
                {"action": "external_directory", "resource": "*", "effect": "ask"},
            ],
        });
        assert_eq!(config["agents"], serde_json::json!({ "build": agent, "plan": agent }));

        // A model the user already pinned a variant on keeps it.
        let spec = with_reasoning_effort(&with_model(&opencode(&[]), Some("p/m#max")), Some(ReasoningEffort::High));
        let layer = OpenCodeLayer { models: models(&[("p/m", &["high", "max"])]), ..worker_layer() };
        let config = RunnerAdapter::for_spec(&spec).opencode_config(&spec, &layer);
        assert_eq!(config.effort_unapplied, None);
        let config: serde_json::Value = serde_json::from_str(&config.json.unwrap()).unwrap();
        assert_eq!(config["agents"]["build"]["model"], "p/m#max");

        // Only a ceiling: permissions without a model.
        let spec = with_permission_mode(&opencode(&[]), Some("plan"));
        let config = config_json(&spec, &OpenCodeLayer::default()).unwrap();
        assert!(config["agents"]["build"].get("model").is_none(), "{config}");
        assert_eq!(config.as_object().unwrap().len(), 1, "{config}");

        // Another runner gets no layer.
        let claude = AgentRunnerSpec { command: "claude".into(), ..opencode(&[]) };
        assert_eq!(config_json(&claude, &worker_layer()), None);
    }

    #[test]
    fn opencode_config_layer_merges_the_users_own_content_under_shelbis() {
        let spec = with_permission_mode(&with_model(&opencode(&[]), Some("p/shelbi")), Some("default"));
        let user = serde_json::json!({
            "model": "p/root",
            "theme": "dark",
            "instructions": ["/home/me/rules.md"],
            "plugins": ["/home/me/plugin"],
            "agents": {
                "build": {
                    "model": "p/user",
                    "steps": 40,
                    "permissions": [{"action": "shell", "resource": "rm *", "effect": "deny"}],
                },
                "review": {"model": "p/review"},
            },
            "mcp": {"servers": {"docs": {"url": "https://example.test"}}},
        });
        let layer = OpenCodeLayer { user_config: Some(user.to_string()), ..worker_layer() };
        let config = config_json(&spec, &layer).unwrap();

        // Keys Shelbi doesn't set survive untouched.
        assert_eq!(config["model"], "p/root");
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcp"], user["mcp"]);
        assert_eq!(config["agents"]["review"], user["agents"]["review"]);
        assert_eq!(config["agents"]["build"]["steps"], 40);
        // Lists keep the user's entries, Shelbi's after them.
        assert_eq!(
            config["instructions"],
            serde_json::json!(["/home/me/rules.md", "/wt/.shelbi/opencode/instructions.md"])
        );
        assert_eq!(
            config["plugins"],
            serde_json::json!(["/home/me/plugin", "/wt/.shelbi/opencode/plugins/shelbi"])
        );
        // Shelbi wins on the values it sets, and its permission rules come
        // last, so the ceiling still holds over the user's rules.
        assert_eq!(config["agents"]["build"]["model"], "p/shelbi");
        let rules = config["agents"]["build"]["permissions"].as_array().unwrap();
        assert_eq!(rules[0], user["agents"]["build"]["permissions"][0]);
        assert_eq!(rules.len(), 1 + rules_for(Some("default")).len());
        assert_eq!(config["agents"]["plan"]["model"], "p/shelbi");

        let result = RunnerAdapter::for_spec(&spec).opencode_config(&spec, &layer);
        assert!(!result.user_config_unreadable);

        // With nothing of Shelbi's to add, the user's value is left alone.
        let bare = OpenCodeLayer { user_config: Some(user.to_string()), ..OpenCodeLayer::default() };
        assert_eq!(config_json(&opencode(&[]), &bare), None);

        // A value that isn't a JSON object can't be merged: Shelbi's layer
        // goes in alone and the caller is told.
        for bad in ["{not json", "[1,2]", "\"text\""] {
            let layer = OpenCodeLayer { user_config: Some(bad.into()), ..worker_layer() };
            let result = RunnerAdapter::for_spec(&spec).opencode_config(&spec, &layer);
            assert!(result.user_config_unreadable, "{bad}");
            let config: serde_json::Value = serde_json::from_str(&result.json.unwrap()).unwrap();
            assert_eq!(config["instructions"], serde_json::json!(["/wt/.shelbi/opencode/instructions.md"]));
        }
        // Blank is no value at all.
        let layer = OpenCodeLayer { user_config: Some("  ".into()), ..worker_layer() };
        assert!(!RunnerAdapter::for_spec(&spec).opencode_config(&spec, &layer).user_config_unreadable);
    }

    fn rules_for(mode: Option<&str>) -> Vec<OpenCodeRule> {
        opencode_permission_rules(mode, false)
    }

    #[test]
    fn opencode_user_config_prefers_the_value_a_shelbi_launch_kept() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
        };
        assert_eq!(opencode_user_config(env(&[])), None);
        assert_eq!(
            opencode_user_config(env(&[("OPENCODE_CONFIG_CONTENT", "{\"a\":1}")])),
            Some("{\"a\":1}".into())
        );
        // Inside a Shelbi OpenCode pane the variable holds Shelbi's layer; the
        // user's own value is the one the launch kept, even an empty one.
        assert_eq!(
            opencode_user_config(env(&[
                ("OPENCODE_CONFIG_CONTENT", "{\"shelbi\":1}"),
                ("SHELBI_OPENCODE_USER_CONFIG", "{\"a\":1}"),
            ])),
            Some("{\"a\":1}".into())
        );
        assert_eq!(
            opencode_user_config(env(&[
                ("OPENCODE_CONFIG_CONTENT", "{\"shelbi\":1}"),
                ("SHELBI_OPENCODE_USER_CONFIG", ""),
            ])),
            None
        );
    }

    fn models(list: &[(&str, &[&str])]) -> Vec<shelbi_core::OpenCodeModel> {
        list.iter()
            .map(|(id, variants)| shelbi_core::OpenCodeModel {
                id: id.to_string(),
                variants: variants.iter().map(|v| v.to_string()).collect(),
            })
            .collect()
    }

    /// The model each agent gets for `spec` over a layer listing `listed`,
    /// and the effort the generator reports it couldn't apply.
    fn agent_models(spec: &AgentRunnerSpec, listed: &[(&str, &[&str])]) -> (serde_json::Value, Option<ReasoningEffort>) {
        let layer = OpenCodeLayer { models: models(listed), ..worker_layer() };
        let config = RunnerAdapter::for_spec(spec).opencode_config(spec, &layer);
        let json: serde_json::Value = serde_json::from_str(&config.json.unwrap()).unwrap();
        assert!(json.get("model").is_none(), "the root never carries the model: {json}");
        let agents = json.get("agents").cloned().unwrap_or_default();
        assert_eq!(agents.get("build"), agents.get("plan"), "{json}");
        (agents.get("build").and_then(|a| a.get("model")).cloned().unwrap_or_default(), config.effort_unapplied)
    }

    /// Decision Q4: an effort becomes the same-named variant on both agents'
    /// model, only when OpenCode reports that variant for the model.
    #[test]
    fn opencode_effort_sets_the_variant_the_model_reports() {
        let listed: &[(&str, &[&str])] = &[
            ("anthropic/claude-sonnet-5-5", &["low", "medium", "high", "max"]),
            ("openai/gpt-6", &["minimal", "high"]),
            ("local/plain", &[]),
        ];
        let spec = |model: &str, effort| with_reasoning_effort(&with_model(&opencode(&[]), Some(model)), effort);

        // Each level lands on its same-named variant.
        for (effort, variant) in [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Max, "max"),
        ] {
            assert_eq!(
                agent_models(&spec("anthropic/claude-sonnet-5-5", Some(effort)), listed),
                (serde_json::json!(format!("anthropic/claude-sonnet-5-5#{variant}")), None),
            );
        }
        assert_eq!(
            agent_models(&spec("openai/gpt-6", Some(ReasoningEffort::High)), listed),
            (serde_json::json!("openai/gpt-6#high"), None),
        );

        // A variant the model doesn't report: the bare model, never a guess.
        assert_eq!(
            agent_models(&spec("openai/gpt-6", Some(ReasoningEffort::Max)), listed),
            (serde_json::json!("openai/gpt-6"), Some(ReasoningEffort::Max)),
        );
        assert_eq!(
            agent_models(&spec("local/plain", Some(ReasoningEffort::Low)), listed),
            (serde_json::json!("local/plain"), Some(ReasoningEffort::Low)),
        );
        // A model the list doesn't have.
        assert_eq!(
            agent_models(&spec("other/model", Some(ReasoningEffort::High)), listed),
            (serde_json::json!("other/model"), Some(ReasoningEffort::High)),
        );
        // The probe failed: no list, no variant.
        assert_eq!(
            agent_models(&spec("anthropic/claude-sonnet-5-5", Some(ReasoningEffort::High)), &[]),
            (serde_json::json!("anthropic/claude-sonnet-5-5"), Some(ReasoningEffort::High)),
        );
        // An effort with no model has nothing to put a variant on.
        let no_model = with_reasoning_effort(&opencode(&[]), Some(ReasoningEffort::High));
        assert_eq!(agent_models(&no_model, listed), (serde_json::Value::Null, Some(ReasoningEffort::High)));

        // No effort: no variant, and nothing to report.
        assert_eq!(
            agent_models(&spec("anthropic/claude-sonnet-5-5", None), listed),
            (serde_json::json!("anthropic/claude-sonnet-5-5"), None),
        );
    }

    #[test]
    fn opencode_effort_maps_only_onto_a_variant_the_model_reports() {
        let variants: Vec<String> = vec!["minimal".into(), "high".into(), "Max".into()];
        assert_eq!(opencode_effort_variant(ReasoningEffort::High, &variants), Some("high".into()));
        assert_eq!(opencode_effort_variant(ReasoningEffort::Max, &variants), Some("max".into()));
        assert_eq!(opencode_effort_variant(ReasoningEffort::Low, &variants), None);
        assert_eq!(opencode_effort_variant(ReasoningEffort::Medium, &[]), None);
    }

    /// A worker's launch reads its generated layer from the file, keeps the
    /// user's own value, and strips the logical flags.
    #[test]
    fn opencode_worker_launch_reads_the_layer_file() {
        let spec = with_edits_denied(
            &with_permission_mode(
                &with_reasoning_effort(&with_model(&opencode(&[]), Some("p/m")), Some(ReasoningEffort::High)),
                Some("bypassPermissions"),
            ),
            true,
        );
        assert_eq!(
            opencode_launch_command_with(&spec, OpenCodeConfigSource::File(".shelbi/opencode/config.json")),
            format!(
                r#"env SHELBI_PANE_TTY="$(tty)" {KEEP_USER} OPENCODE_CONFIG_CONTENT="$(cat .shelbi/opencode/config.json)" {OC_ENV} opencode --standalone --auto"#
            )
        );
    }

    /// `(action, effect)` pairs of the rules a ceiling produces, every rule
    /// on resource `*`.
    fn rules(mode: Option<&str>, deny_edits: bool) -> Vec<(&'static str, &'static str)> {
        opencode_permission_rules(mode, deny_edits)
            .into_iter()
            .map(|r| {
                assert_eq!(r.resource, "*", "{r:?}");
                (r.action, r.effect)
            })
            .collect()
    }

    /// Every row of the ceiling mapping (Plans/opencode-integration.md,
    /// "Permission ceiling mapping").
    #[test]
    fn opencode_ceilings_map_onto_permission_rules() {
        let ask_default = vec![
            ("edit", "ask"),
            ("shell", "ask"),
            ("webfetch", "ask"),
            ("external_directory", "ask"),
        ];
        // None configured: nothing imposed, the user's settings stand.
        assert_eq!(rules(None, false), vec![]);
        // plan: deny everything, then allow the read-only tools.
        assert_eq!(
            rules(Some("plan"), false),
            vec![
                ("*", "deny"),
                ("read", "allow"),
                ("glob", "allow"),
                ("grep", "allow"),
                ("webfetch", "allow"),
                ("websearch", "allow"),
                ("skill", "allow"),
                ("question", "allow"),
            ]
        );
        // default: ask before edits, shell, fetches and leaving the tree.
        assert_eq!(rules(Some("default"), false), ask_default);
        // read-only: default's asks, then no edits at all.
        let mut read_only = ask_default.clone();
        read_only.push(("edit", "deny"));
        assert_eq!(rules(Some("read-only"), false), read_only);
        // auto and acceptEdits: edits allowed, shell still asks.
        for mode in ["auto", "acceptEdits"] {
            assert_eq!(
                rules(Some(mode), false),
                vec![("edit", "allow"), ("shell", "ask"), ("external_directory", "ask")],
                "{mode}"
            );
        }
        // bypassPermissions: everything allowed.
        assert_eq!(rules(Some("bypassPermissions"), false), vec![("*", "allow")]);
        // A typo ranks as default: never looser.
        assert_eq!(rules(Some("bogus"), false), ask_default);
        // The review agent's no-edit posture appends an edit deny at any
        // ceiling, and with none.
        for mode in [None, Some("plan"), Some("default"), Some("auto"), Some("bypassPermissions")] {
            let r = rules(mode, true);
            assert_eq!(r.last(), Some(&("edit", "deny")), "{mode:?}");
            assert_eq!(&r[..r.len() - 1], &rules(mode, false)[..], "{mode:?}");
        }
    }

    /// The ceiling reaches the launch as config on both agents, and only
    /// `bypassPermissions` passes `--auto`.
    #[test]
    fn opencode_ceilings_reach_the_launch_and_only_bypass_passes_auto() {
        let modes = [
            None,
            Some("plan"),
            Some("default"),
            Some("read-only"),
            Some("auto"),
            Some("acceptEdits"),
            Some("bypassPermissions"),
        ];
        for mode in modes {
            for deny_edits in [false, true] {
                let spec = with_edits_denied(&with_permission_mode(&opencode(&[]), mode), deny_edits);
                let line = launch_command(&spec);
                let argv = line.split(" opencode ").nth(1).unwrap();
                assert_eq!(
                    argv.split(' ').any(|t| t == "--auto"),
                    mode == Some("bypassPermissions"),
                    "{mode:?}: {line}"
                );
                // The logical flags never reach OpenCode.
                assert!(!line.contains("--permission-mode"), "{line}");
                assert!(!line.contains("--shelbi-deny-edits"), "{line}");
                let expected = rules(mode, deny_edits);
                if expected.is_empty() {
                    assert!(!line.contains("OPENCODE_CONFIG_CONTENT"), "{line}");
                    continue;
                }
                let list: Vec<serde_json::Value> = expected
                    .iter()
                    .map(|(a, e)| serde_json::json!({"action": a, "resource": "*", "effect": e}))
                    .collect();
                let agent = serde_json::json!({ "permissions": list });
                assert_eq!(
                    inline_config(&line),
                    Some(serde_json::json!({ "agents": { "build": agent, "plan": agent } })),
                    "{mode:?}/{deny_edits}: {line}"
                );
            }
        }
        // A ceiling below bypass drops a hand-written `--auto`: it would
        // approve the asks the ceiling raises.
        let spec = with_permission_mode(&opencode(&["--auto"]), Some("auto"));
        assert!(!launch_command(&spec).contains("--auto"));
        // The resolved ceiling replaces a hand-written logical one.
        let spec = with_permission_mode(&opencode(&["--permission-mode", "bypassPermissions"]), Some("plan"));
        assert_eq!(spec.flags, vec!["--permission-mode", "plan"]);
        // Edits are denied once, and only on OpenCode (Claude reads its
        // settings.json deny itself).
        let twice = with_edits_denied(&with_edits_denied(&opencode(&[]), true), true);
        assert_eq!(twice.flags, vec!["--shelbi-deny-edits"]);
        let claude = AgentRunnerSpec { command: "claude".into(), ..opencode(&[]) };
        assert!(with_edits_denied(&claude, true).flags.is_empty());
    }

    /// The whole line a resumed review worker (edits denied) on an
    /// `acceptEdits` ceiling with a model launches with, and a bypass one.
    #[test]
    fn opencode_full_launch_line_golden() {
        let spec = with_permission_mode(&with_model(&opencode(&[]), Some("anthropic/claude-sonnet-5-5")), Some("acceptEdits"));
        let spec = with_edits_denied(&with_continue(&spec, true), true);
        let spec = with_opencode_session(&spec, "ses_shelbi0123456789abcdef0123", "Shelbi · bravo · fix");
        let agent = concat!(
            r#"{"model":"anthropic/claude-sonnet-5-5","permissions":["#,
            r#"{"action":"edit","effect":"allow","resource":"*"},{"action":"shell","effect":"ask","resource":"*"},"#,
            r#"{"action":"external_directory","effect":"ask","resource":"*"},{"action":"edit","effect":"deny","resource":"*"}]}"#,
        );
        assert_eq!(
            launch_command(&spec),
            format!(
                r#"env SHELBI_PANE_TTY="$(tty)" SHELBI_OPENCODE_SESSION=ses_shelbi0123456789abcdef0123 SHELBI_OPENCODE_TITLE='Shelbi · bravo · fix' {KEEP_USER} OPENCODE_CONFIG_CONTENT='{{"agents":{{"build":{agent},"plan":{agent}}}}}' {OC_ENV} opencode --standalone --session ses_shelbi0123456789abcdef0123"#
            )
        );
        let bypass = with_permission_mode(&opencode(&[]), Some("bypassPermissions"));
        assert_eq!(
            launch_command(&bypass),
            format!(
                r#"env SHELBI_PANE_TTY="$(tty)" {KEEP_USER} OPENCODE_CONFIG_CONTENT='{{"agents":{{"build":{{"permissions":[{{"action":"*","effect":"allow","resource":"*"}}]}},"plan":{{"permissions":[{{"action":"*","effect":"allow","resource":"*"}}]}}}}}}' {OC_ENV} opencode --standalone --auto"#
            )
        );
    }

    #[test]
    fn opencode_adapter_runs_the_hook_contract() {
        let adapter = RunnerAdapter::for_spec(&opencode(&[]));
        assert!(adapter.is_opencode());
        assert_eq!(adapter.message_channel(), MessageChannel::Hooks);
        assert!(!adapter.polls_for_messages());
        assert_eq!(adapter.resume_strategy(), ResumeStrategy::NamedSession);
        assert!(!adapter.needs_claude_readiness_probe());
        assert_eq!(
            adapter.orchestrator_plugin_injection(true),
            OrchestratorPluginInjection::None
        );
        // An explicit `integration:` pins a wrapper binary to the adapter.
        let mut wrapped = opencode(&[]);
        wrapped.command = "my-oc-wrapper".into();
        wrapped.integration = Some(RunnerKind::OpenCode);
        assert!(RunnerAdapter::for_spec(&wrapped).is_opencode());
    }

    #[test]
    fn opencode_resume_and_effort_map_onto_its_flags() {
        // OpenCode never resumes by `--continue` (the directory's newest
        // session, which can be the user's): it names its session.
        assert!(with_continue(&opencode(&[]), true).flags.is_empty());
        assert!(with_continue(&opencode(&[]), false).flags.is_empty());
        // Effort rides a logical flag (the config layer turns it into the
        // model's variant), and a newer effort replaces an older one.
        let high = with_reasoning_effort(&opencode(&[]), Some(ReasoningEffort::High));
        assert_eq!(opencode_effort(&high), Some(ReasoningEffort::High));
        let low = with_reasoning_effort(&high, Some(ReasoningEffort::Low));
        assert_eq!(low.flags, vec!["--shelbi-effort".to_string(), "low".to_string()]);
        assert_eq!(opencode_effort(&low), Some(ReasoningEffort::Low));
        assert_eq!(with_reasoning_effort(&high, None), high);
        assert_eq!(opencode_effort(&opencode(&[])), None);
    }

    /// The named session replaces any session or `--continue` a user wrote,
    /// and only the real `--session` reaches the argv: the title rides the
    /// environment.
    #[test]
    fn opencode_session_replaces_continue_and_exports_its_id() {
        let spec = opencode(&["--continue", "-s", "ses_old", "-c", "--session=ses_older"]);
        let named = with_opencode_session(&spec, "ses_shelbiaaaa", "Shelbi · w · t");
        assert_eq!(
            named.flags,
            vec!["--session", "ses_shelbiaaaa", "--shelbi-session-title", "Shelbi · w · t"]
        );
        // Idempotent: naming it again leaves one session.
        assert_eq!(with_opencode_session(&named, "ses_shelbiaaaa", "Shelbi · w · t").flags, named.flags);
        assert_eq!(
            launch_command(&named),
            format!(
                "env SHELBI_PANE_TTY=\"$(tty)\" SHELBI_OPENCODE_SESSION=ses_shelbiaaaa \
                 SHELBI_OPENCODE_TITLE='Shelbi · w · t' {OC_ENV} opencode --standalone --session ses_shelbiaaaa"
            )
        );
        // A hand-written `--continue` with no named session is the user's call.
        assert_eq!(
            launch_command(&opencode(&["--continue"])),
            format!(r#"env SHELBI_PANE_TTY="$(tty)" {OC_ENV} opencode --standalone --continue"#)
        );
        // Other runners are untouched.
        assert_eq!(with_opencode_session(&runner("claude"), "ses_x", "t").flags, Vec::<String>::new());
    }

    /// The plugin's role rides the environment, never the argv, and naming
    /// it again replaces it.
    #[test]
    fn opencode_role_is_exported_for_the_plugin() {
        let spec = with_opencode_session(&opencode(&[]), "ses_shelbiorchaaaa", "Shelbi · p · orchestrator");
        let spec = with_opencode_role(&with_opencode_role(&spec, "worker"), OPENCODE_ORCHESTRATOR_ROLE);
        assert_eq!(
            launch_command(&spec),
            format!(
                "env SHELBI_PANE_TTY=\"$(tty)\" SHELBI_OPENCODE_SESSION=ses_shelbiorchaaaa \
                 SHELBI_OPENCODE_TITLE='Shelbi · p · orchestrator' SHELBI_OPENCODE_ROLE=orchestrator \
                 {OC_ENV} opencode --standalone --session ses_shelbiorchaaaa"
            )
        );
        assert_eq!(with_opencode_role(&runner("claude"), "orchestrator").flags, Vec::<String>::new());
    }

    /// Regression: the orchestrator and worker launchers run the line as
    /// `exec {launch}`, where a bare `VAR=value` prefix becomes the command
    /// name ("no such file or directory: SHELBI_PANE_TTY=…"). Run a real shell
    /// over the exact line, with `true` standing in for the OpenCode binary.
    #[test]
    fn opencode_launch_line_survives_exec() {
        let spec = with_model(&opencode(&[]), Some("p/m"));
        let spec = with_edits_denied(&with_permission_mode(&spec, Some("bypassPermissions")), true);
        let mut spec = with_opencode_session(&spec, "ses_shelbi00", "Shelbi · it's · t");
        spec.command = "true".into();
        spec.integration = Some(RunnerKind::OpenCode);
        let line = format!("exec {}", launch_command(&spec));
        let status = std::process::Command::new("sh")
            .args(["-c", &line])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{line}");

        // The file form, with `env` standing in for OpenCode so the line
        // prints what the OpenCode process would see: Shelbi's layer in the
        // variable, the user's own value kept beside it.
        let dir = std::env::temp_dir().join(format!("shelbi-oc-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("layer.json"), r#"{"plugins":["/p"]}"#).unwrap();
        let mut spec = opencode(&[]);
        spec.command = "env".into();
        spec.integration = Some(RunnerKind::OpenCode);
        let line = opencode_launch_command_with(&spec, OpenCodeConfigSource::File("layer.json"))
            .replace(" --standalone", "");
        let run = |env: &[(&str, &str)]| {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", &format!("exec {line}")]).current_dir(&dir);
            cmd.env_remove("OPENCODE_CONFIG_CONTENT").env_remove("SHELBI_OPENCODE_USER_CONFIG");
            cmd.envs(env.iter().copied());
            let out = cmd.stdin(std::process::Stdio::null()).output().unwrap();
            assert!(out.status.success(), "{line}");
            String::from_utf8(out.stdout).unwrap()
        };
        let out = run(&[("OPENCODE_CONFIG_CONTENT", r#"{"theme":"x y"}"#)]);
        assert!(out.lines().any(|l| l == r#"OPENCODE_CONFIG_CONTENT={"plugins":["/p"]}"#), "{out}");
        assert!(out.lines().any(|l| l == r#"SHELBI_OPENCODE_USER_CONFIG={"theme":"x y"}"#), "{out}");
        // A nested launch keeps the original user value, not the outer layer.
        let out = run(&[
            ("OPENCODE_CONFIG_CONTENT", r#"{"outer":1}"#),
            ("SHELBI_OPENCODE_USER_CONFIG", r#"{"theme":"x"}"#),
        ]);
        assert!(out.lines().any(|l| l == r#"SHELBI_OPENCODE_USER_CONFIG={"theme":"x"}"#), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
