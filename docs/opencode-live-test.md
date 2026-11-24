# OpenCode live test recipe

How to check Shelbi's OpenCode runner against a real OpenCode, end to end,
without touching your own OpenCode config, your Shelbi home, or the live hub.
Run it when you bump the supported OpenCode version, change the plugin
(`crates/shelbi-orchestrator/src/opencode_plugin.js`), the launch line or the
config layer, or when an OpenCode release lands that might have renamed an
event. The user-facing description of the runner is the site page
[OpenCode](https://shelbi.dev/docs/concepts/opencode)
(`site/content/docs/concepts/opencode.mdx`); keep both in step.

OpenCode isn't installed in CI, so this recipe is the only place a real
OpenCode runs. CI covers everything that doesn't need one (see
[Automated checks](#1-automated-checks)).

## Ground rules

- Never run this from a plain shell that has your real hub in its environment
  without the `env -u` below, and never stop or reload the live daemon. Every
  Shelbi process in this recipe runs under its own `SHELBI_ROOT`.
- OpenCode gets a scratch `HOME` **and** scratch `XDG_*` directories. With the
  `XDG_*` variables alone, OpenCode 2.x still created directories in the real
  home during the original spike.
- Install OpenCode into a scratch npm prefix, not globally. Use the
  `@opencode/cli` package: `opencode-ai` installs 1.x, which Shelbi refuses.
- Keep paths short. A Unix socket path is limited to about 100 bytes on
  macOS, and the sandbox hub's socket lives under `SHELBI_ROOT`.

## 0. Sandbox

```sh
export S=/tmp/shoc                       # short: socket paths live under it
rm -rf "$S" && mkdir -p "$S"/{home,xdg/config,xdg/data,xdg/cache,xdg/state,npm,bin,root}

# OpenCode 2.x in a scratch prefix.
npm install --prefix "$S/npm" @opencode/cli
"$S/npm/node_modules/.bin/opencode" --version      # expect "opencode v2.x.y", 2.3 or later

# The shelbi under test, from this worktree.
cargo build -p shelbi
ln -sf "$PWD/target/debug/shelbi" "$S/bin/shelbi"
ln -sf "$S/npm/node_modules/.bin/opencode" "$S/bin/opencode"

# One shell with everything pointed at the sandbox. Run every later step in it.
env -u SHELBI_HUB_SOCK -u SHELBI_HUB_ADDR -u SHELBI_CONTROL_SOCK \
    -u PROJECT -u TASK_ID -u SHELBI_WORKSPACE -u OPENCODE_CONFIG_CONTENT \
  HOME="$S/home" \
  XDG_CONFIG_HOME="$S/xdg/config" XDG_DATA_HOME="$S/xdg/data" \
  XDG_CACHE_HOME="$S/xdg/cache" XDG_STATE_HOME="$S/xdg/state" \
  SHELBI_ROOT="$S/root" \
  PATH="$S/bin:$PATH" \
  zsh -f
```

`zsh -f` (or `bash --norc`) keeps your rc files from putting the real PATH
back in front. Check `which shelbi opencode` points into `$S/bin`.

**Provider.** OpenCode's free `opencode/*` models work without a key, but they
hit a daily free limit after a few turns. That limit is useful: it is the
provider-limit case in step 3. For a longer run, log in inside the sandbox
(`opencode auth login`, which writes under the scratch `XDG_DATA_HOME`) or
export a provider key in the sandbox shell only.

## 1. Automated checks

These run without OpenCode and are what CI runs. Run them first: a failure
here is cheaper to read than a live one.

```sh
# Golden fixtures: the orchestrator and worker launch lines, the config layer
# per permission ceiling, model variant, review edit deny, user-config merge,
# the instructions file and the plugin's package manifest.
cargo test -p shelbi-orchestrator --lib golden

# The plugin's unit tests, under Node, against a fake OpenCode plugin context.
node --test crates/shelbi-orchestrator/tests/opencode-plugin/plugin.test.mjs

# The plugin under Node from the Rust suite: the single-instance lock, the
# orchestrator's awareness contract, and the replay of a recorded OpenCode
# event stream (tests/fixtures/opencode/events-2.4.1.sse).
cargo test -p shelbi-orchestrator --lib workspace::opencode
```

A golden that drifts on purpose is regenerated with
`UPDATE_GOLDEN=1 cargo test -p shelbi-orchestrator --lib golden`; review the
diff in `crates/shelbi-orchestrator/tests/golden/opencode/` before committing.

## 2. A project on OpenCode

```sh
mkdir -p "$S/proj" && cd "$S/proj" && git init -q && git commit -q --allow-empty -m init
shelbi init -y --runner opencode
```

`--runner opencode` with `-y` refuses an OpenCode below 2.3. To check the
refusal, point `$S/bin/opencode` at a script that prints `1.19.6`: `init`,
the requirements check and a dispatch should all say
"OpenCode 1.19.6 isn't supported. Shelbi needs OpenCode 2.3 or later: run
`opencode upgrade`." and a dispatch should log one `status=opencode-unsupported`
line in `$S/root/events.log`. Put the real binary back afterwards.

To exercise a ceiling, set `workspace_permissions_mode: default` in the
project's config before dispatching (with none set, OpenCode allows
everything and no ask ever shows).

Start the TUI in the sandbox shell (`shelbi`), launch the orchestrator, and
add a small issue (for example "Add a HELLO.md with one line"). Let it
dispatch, or start it with `shelbi issue start <id>`.

## 3. Worker checks

With the worker running, look at its worktree under
`$S/proj/.shelbi/wt/<workspace>/` (or the project's work dir) and at
`$S/root/events.log`.

- **Files.** `.shelbi/opencode/` holds `config.json`, `instructions.md`,
  `skills/` (when the agent has skills), `plugins/shelbi/{index.js,package.json}`,
  `plugin.lock` and `plugin.alive`. Nothing under `.opencode/`, no
  `opencode.json`, no `AGENTS.md`, and `$S/xdg/config/opencode` is unchanged.
  `git status` in the worktree is clean of all of it.
- **Launch line.** `shelbi session snapshot <workspace> --history 5` shows the
  OpenCode TUI. The pane's command (`ps -o command= -p <pid>`) matches
  `tests/golden/opencode/worker-argv.txt` with this task's session id and
  title.
- **Session.** `status.yaml` under
  `$S/root/projects/<project>/workspaces/<ws>/` names `opencode_session:
  ses_shelbi…`. In the OpenCode TUI's session list the session is titled
  `Shelbi · <workspace> · <task>`; rename it and the title comes back.
- **States.** The sidebar goes working → blocked (on the first `edit` or shell
  ask under the `default` ceiling) → working once you answer → idle at the end.
  `runner-state.json` beside `status.yaml` holds each latest report. With the
  free models, run a few turns until the limit: the sidebar shows Paused with
  `provider.free-limit`, not idle.
- **Messages.** While the worker is idle, `shelbi send <workspace> "say hi"`
  reports a verified send and the session starts a turn on it. Type a draft
  into the composer first: it is still there after the send.
- **Resume.** Kill the worker's session from the TUI, then
  `shelbi issue resume <id>`: the pane comes back on the same `ses_shelbi…` id
  and the conversation is intact. A fresh `shelbi issue start` gets a new id.
- **Archive.** Move the issue to done. Within about a minute the session drops
  out of the OpenCode TUI's session list. It is archived, not deleted:
  `opencode api --standalone GET /api/session/<id>` in the sandbox shell still
  returns it, marked archived. The archive call runs a private server for the
  one request, so no extra `opencode` process is left behind.
- **Plugin silent.** Break the plugin's syntax in
  `crates/shelbi-orchestrator/src/opencode_plugin.js`, rebuild, and dispatch.
  Ten seconds after launch the events log shows `status=integration` with
  `state_observation=degraded reason=opencode-plugin-silent` in its detail, and
  a permission ask still shows the workspace as blocked, read off the screen
  ("Permission required"). A `shelbi send` to that pane is pasted, and its send
  line says `status=degraded`. Revert the plugin and rebuild.

## 4. Orchestrator checks

Set `orchestrator.awareness: awareness` on the project and run the
orchestrator on OpenCode (`shelbi init -y --runner opencode` already puts it
there), then start it with `shelbi -p <repo> orchestrate`. Read what the
orchestrator received with
`opencode api --standalone GET /api/session/<id>/message` (the newest page of
messages).

- The orchestrator's pane runs on `ses_shelbiorch…`, titled
  `Shelbi · <project> · orchestrator`; a reload reopens the same session.
- Its config layer (inline on the launch line) names
  `<config dir>/.shelbi/opencode/instructions.md`,
  `.claude/agent-instructions.md`, the `update-shelbi-configuration` skill and,
  with awareness, the plugin; the launch sets `SHELBI_OPENCODE_ROLE=orchestrator`.
  Compare with `tests/golden/opencode/orchestrator-awareness-argv.txt`.
- With the orchestrator idle, move an issue to ready: the orchestrator starts a
  turn on the board event without any input from you. Slow wakes show as
  "Failed to drain Session" in `$HOME/.local/share/opencode/log/opencode.log`
  (the sandbox `HOME`).
- On the free models, once turns start failing on the limit, later event
  prompts stay queued in the TUI ("N queued") and never reach the transcript.
  That is OpenCode holding them, not the plugin dropping them; use a real
  provider for this check.

## 5. Re-recording the event fixture

The contract test replays `crates/shelbi-orchestrator/tests/fixtures/opencode/events-2.4.1.sse`
through the plugin's handler table. When a new OpenCode release might rename
or reshape an event, record a fresh stream and replay it:

The free models can't produce most of these turns on demand, so the 2.4.1
fixture was recorded against a scripted OpenAI-compatible provider: a small
local HTTP server that answers each request from a script. It returns a 429
with `retry-after` for the retry, a tool call to the shell tool for the
permission ask, a question tool call, a 429 whose body is OpenCode's
`FreeUsageLimitError` for the free limit, and a task tool call for the
subagent. Wire it in the sandbox config as a provider with
`"package": "aisdk:@ai-sdk/openai-compatible"` and its `baseURL`.

```sh
# In the sandbox shell, in a scratch project directory.
export OPENCODE_PASSWORD=$(openssl rand -hex 16)
opencode serve --port 49400 &           # a private server, not the background service
curl -sN -u "opencode:$OPENCODE_PASSWORD" http://127.0.0.1:49400/api/event > "$S/events.sse" &

# Drive one session (create it with a Shelbi-shaped id, e.g. ses_shelbirec00000000000001,
# from a TUI attached with `opencode --server http://127.0.0.1:49400 --session <id>`)
# through, in order: a rate-limit retry, a shell permission ask answered once,
# a question form, a free-usage-limit failure, a user interrupt, and a
# subagent. Then stop curl and the server (kill the pids you started).
```

Rewrite absolute paths to `/sandbox`, drop the heartbeat lines, keep the
header comment (version, date, what the turns are), save it as
`events-<version>.sse`, and point `opencode_plugin_replays_recorded_events`
at it. If the expected state sequence changes, the plugin's `handlers` table
needs the matching change.

## Cleanup

```sh
shelbi -p <repo> quit -y   # in the sandbox shell: stops the sandbox hub only
exit                    # leave the sandbox shell
rm -rf /tmp/shoc
```

Confirm nothing landed outside the sandbox: `~/.config/opencode`,
`~/.local/share/opencode`, `~/.local/state/opencode` and `~/.cache/opencode`
should be exactly as they were before (absent, if you don't use OpenCode).
