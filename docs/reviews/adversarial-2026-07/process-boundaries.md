# Adversarial review: process boundaries (tmux/ssh/agent/palette crates)

Reviewed:

- crates/shelbi-tmux/src/lib.rs (523 lines)
- crates/shelbi-ssh/src/lib.rs (365 lines)
- crates/shelbi-agent/src/lib.rs (235 lines)
- crates/shelbi-palette/src/lib.rs (148 lines)

Method notes: findings marked `certain` were either traced end-to-end in the code
or reproduced empirically against tmux 3.5a and `sh`/`zsh` on this machine
(scratch sessions namespaced `advrev-*`, created and destroyed during the review).
No source files were modified.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | `pane_title` is broken over SSH: `#{pane_title}` is stripped as a shell comment by the remote shell | medium | certain | bug |
| F2 | `build_command` performs no shell escaping for SSH; args with spaces/metachars silently re-tokenize on the remote shell | medium | certain | assumption |
| F3 | No tmux target is exact-matched (`=` prefix missing) — session/window names resolve by prefix, so keystrokes can land in the wrong session | medium | certain | hardening |
| F4 | `send_line`'s shared paste buffer (`shelbi-send`) races across concurrent senders — a message can be pasted into the wrong pane or lost | medium | certain | bug |
| F5 | Local `send-keys -l` fast path lacks `--`: any message starting with `-` fails (or is parsed as tmux flags) | medium | certain | bug |
| F6 | `has_session` conflates SSH transport failure (exit 255) with "session doesn't exist" | medium | certain | failure-scenario |
| F7 | Reverse-forward failure is fully silenced (`LogLevel=ERROR` + `ExitOnForwardFailure=no`), including on master open; a stale remote `/tmp/shelbi-hub.sock` silently kills worker→hub messaging | medium | likely | failure-scenario |
| F8 | `run_with_stdin` leaks a zombie child and masks the real error when the stdin write fails; whole-payload-then-read shape can deadlock on chatty commands | low | certain | bug |
| F9 | `with_permission_mode` idempotence check misses the `--permission-mode=<mode>` form, defeating the documented "respect YAML-pinned mode" behavior | low | certain | bug |
| F10 | `is_claude_runner` is private, so callers re-implement the basename check by hand (drift risk) | low | certain | best-practice |
| F11 | `search()` re-parses the pattern once per entry, re-implements `Pattern::score`, and clones every entry per keystroke | low | certain | simplification |
| F12 | ControlPath can exceed the 104-byte `sun_path` limit for long `$HOME` + user + hostname combinations | low | speculative | failure-scenario |
| F13 | `kill_window` returns `Err` for already-gone windows, so every caller does `let _ =` and swallows transport errors too | low | certain | best-practice |

---

## F1: `pane_title` is broken over SSH — `#{pane_title}` is comment-stripped by the remote shell

- **Where:** crates/shelbi-tmux/src/lib.rs:151-164 (root cause shared with F2 at crates/shelbi-ssh/src/lib.rs:139-148)
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:** `pane_title` splices a raw tmux format string into the argv:

  ```rust
  let raw = shelbi_ssh::run_capture(
      host,
      [ "tmux", "display-message", "-p", "-t", &addr.target(), "#{pane_title}" ],
  )?;
  ```

  For `Host::Ssh`, `build_command` hands this argv to `ssh host -- tmux display-message -p -t <target> #{pane_title}`. ssh joins the command words with single spaces and the remote shell re-parses the string — and in POSIX sh, zsh, and fish, an unquoted word beginning with `#` starts a comment. Reproduced locally:

  ```
  $ sh -c 'printf "[%s]\n" tmux display-message -p -t foo:agent #{pane_title}'
  [tmux] [display-message] [-p] [-t] [foo:agent]      # format arg gone
  ```

  What the stripped command actually prints (verified against tmux 3.5a):

  ```
  $ tmux display-message -p -t advrev-foo-bar:agent
  [advrev-foo-bar] 1:agent, current pane 1 - (23:29 01-Jul-26)
  ```

  So over SSH `pane_title` succeeds (exit 0) but returns the default status message instead of the pane title. `parse_pane_title_marker` never finds a `shelbi:*` marker in that string.
- **Failure scenario:** the hub sidebar poller (crates/shelbi-tui/src/poller.rs:340-346) reads the title and bails when no marker parses — for every remote workspace, on every poll, forever. Remote workspace working/idle/blocked state silently never updates; no error is logged because the command "succeeded". `wait_for_prompt_submitted` (crates/shelbi-orchestrator/src/workspace.rs:720) loses its title signal too and survives only via its pane-body fallback. The crate's own test `remote_pane_title_argv` (lib.rs:419-450) pins exactly this broken wire format — it asserts the raw `#{pane_title}` lands in the ssh argv, which is precisely what the remote shell then eats. Note the same stripping hits `list-windows -F #W` in `workspace_pane_alive` (crates/shelbi-orchestrator/src/workspace.rs:386, outside this slice) — so remote pane-aliveness reads as false and `shelbi send` to a remote workspace refuses to send at all.
- **Recommendation:** escape SSH-routed argv (see F2), or at minimum single-quote format-string arguments before they cross the boundary. Add an integration-shaped test that round-trips through `sh -c` (the moral equivalent of the remote shell) rather than only asserting local argv shape.
- **Effort:** S (targeted quoting) / M (as part of the F2 fix)

## F2: `build_command` does no shell escaping for SSH — the "callers pass pre-escaped arguments" contract is not honored by its main caller

- **Where:** crates/shelbi-ssh/src/lib.rs:120-150 (contract stated at lines 121-123)
- **Category:** assumption
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  /// The argv is treated as a single command line for SSH (joined with
  /// spaces, no shell escaping yet — callers are expected to pass pre-escaped
  /// arguments for now).
  pub fn build_command<I, S>(host: &Host, argv: I) -> Command
  ```

  The `Host::Ssh` arm pushes each arg verbatim (`cmd.arg(a.as_ref())`, lines 144-146). ssh concatenates argv with spaces and the remote login shell re-tokenizes. But shelbi-tmux — the primary caller — passes raw, unescaped arguments throughout: targets, window names, format strings, and optional command strings. `crates/shelbi-orchestrator/src/git.rs:40-57` explicitly documents and works around this trap ("SSH is the trap: `shelbi_ssh::run` joins our argv with literal spaces"), proving the contract is known yet only honored in one corner of the codebase.
- **Failure scenario:** concrete in-repo example: shelbi-tmux's own test `remote_new_session_argv` (crates/shelbi-tmux/src/lib.rs:324-357) builds `ssh m2.local -- tmux new-session -d -s shelbi-w-fix-login -n agent "cd /work/… && claude"`. If that command were actually run, the remote shell would split the final argument: tmux would receive `cd /work/…` as the window command and then `&& claude` would launch claude **in the SSH session, outside tmux**, blocking the invocation. Production callers currently dodge this by passing `command: None` and delivering the launch line via the stdin-fed buffer path (crates/shelbi-orchestrator/src/workspace.rs:603-605) — but the API remains a loaded gun, and the test enshrines the dangerous form as the expected wire format. Similarly, any session/window/project name containing a space, `;`, `$`, or glob character is silently corrupted or executed on the remote shell (names come from user-authored project YAML; nothing validates them — see F3's note). A secondary papering-over: `run_capture` (lines 202-207) space-joins argv for its error string, so `["a b"]` and `["a","b"]` produce identical diagnostics, hiding exactly this class of bug.
- **Recommendation:** make the `Host::Ssh` arm shell-escape each argument (the escaper already exists: `shelbi_agent::shell_escape` — move it into shelbi-ssh or shelbi-core to fix the dependency direction). This requires a coordinated pass: `git.rs` currently pre-escapes and would double-escape, so migrate its call sites in the same change. Alternatively, validate at the boundary and reject argv elements containing shell metacharacters when the host is SSH.
- **Effort:** M

## F3: tmux targets are never exact-matched — prefix/fnmatch resolution can silently address the wrong session or window

- **Where:** crates/shelbi-tmux/src/lib.rs:10 (`has-session -t name`), :51 (`new-window -t {session}:`), :67, :132, :134, :142, :158, :172, :189 (every `-t` splice of `addr.target()`)
- **Category:** hardening
- **Severity / Confidence:** medium / certain
- **Evidence:** tmux resolves a `target-session` that isn't an exact match by prefix, then by fnmatch pattern; only a `=`-prefixed target forces exact matching. Reproduced against tmux 3.5a:

  ```
  $ tmux new-session -d -s advrev-foo-bar -n agent
  $ tmux has-session -t advrev-foo; echo $?     # no session with this exact name
  0                                              # matched advrev-foo-bar by prefix
  $ tmux display-message -p -t advrev-foo-bar:age '#{window_name}'
  agent                                          # window names prefix-match too
  ```

  None of the nine `-t` usages in this crate use the `=` prefix.
- **Failure scenario:** workspace sessions are named `shelbi-w-{workspace.name}` and project sessions `shelbi-{project.name}` (crates/shelbi-orchestrator/src/workspace.rs:41-47). Declare workspaces `bob` and `bob-2` (or projects `app` and `app-2`): while `shelbi-w-bob` is torn down between tasks, `has_session(host, "shelbi-w-bob")` returns **true** because `shelbi-w-bob-2` prefix-matches. `start_workspace_on_task` then skips session creation and `send_line` delivers the `cd … && claude` launch line and the full task prompt **into workspace bob-2's pane** — keystroke injection into another live agent, which will dutifully act on it. Same mechanism applies to `kill_window` (killing another workspace's window) and `capture` (leaking another pane's contents into a task transcript).
- **Recommendation:** prefix every session-name target with `=` (`has-session -t "=name"`, `-t "={session}:{window}"`). Note for callers that build shell strings by hand: `=name` must be quoted in zsh (equals-expansion) — inside `Command::arg` argv it's safe.
- **Effort:** S

## F4: `send_line`'s fixed paste-buffer name races across concurrent senders

- **Where:** crates/shelbi-tmux/src/lib.rs:74 (`const PASTE_BUFFER: &str = "shelbi-send"`), :112-130
- **Category:** bug
- **Severity / Confidence:** medium / certain (mechanism traced; concurrent trigger is likely, not proven in production)
- **Evidence:** the buffer path is two separate tmux invocations against server-global state:

  ```rust
  shelbi_ssh::run_with_stdin(host, ["tmux", "load-buffer", "-b", PASTE_BUFFER, "-"], text.as_bytes())?;
  shelbi_ssh::run_capture(host, ["tmux", "paste-buffer", "-p", "-d", "-b", PASTE_BUFFER, "-t", &target])?;
  ```

  tmux named buffers are per-server, and the name is a compile-time constant. Every multi-line send (all hosts) and every single-line SSH send funnels through the same buffer name.
- **Failure scenario:** sender A (hub dispatching a task prompt, crates/shelbi-orchestrator/src/workspace.rs:632) and sender B (`shelbi send`, a separate process — crates/shelbi-cli/src/commands/send.rs:52) interleave on the same tmux server: A `load-buffer`, B `load-buffer` (overwrites), A `paste-buffer -d` → **B's message is pasted into A's target pane and the buffer deleted**, B `paste-buffer -d` → `Error::Command` ("no buffer shelbi-send"). Net effect: the wrong text goes to the wrong agent (which then receives A's trailing `Enter` and executes on it), one message is lost entirely, and one sender gets an opaque error. Two remote workspaces on the same SSH host share one tmux server, so remote dispatch is equally exposed. The orchestrator is itself an agent that shells out `shelbi send` while the hub polls and dispatches — concurrent sends are a matter of time, not design.
- **Recommendation:** derive a per-invocation buffer name (e.g. `shelbi-send-{pid}-{counter}`) so concurrent sends can't collide; `-d` on paste keeps cleanup automatic. Alternatively serialize sends per host behind a lock, but unique names are simpler and cover the multi-process case.
- **Effort:** S

## F5: local `send-keys -l` fast path breaks on messages starting with `-`

- **Where:** crates/shelbi-tmux/src/lib.rs:132
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &target, "-l", text])?;
  ```

  There is no `--` terminator before `text`, and tmux keeps parsing flags after `-l`. Reproduced against tmux 3.5a:

  ```
  $ tmux send-keys -t advrev-foo-bar:agent -l "-R hello"
  command send-keys: invalid flag -
  $ tmux send-keys -t advrev-foo-bar:agent -l "-q oops"
  command send-keys: unknown flag -q
  ```

  A payload of exactly `-R` (a valid send-keys flag) would be consumed as the reset-terminal flag rather than sent as text.
- **Failure scenario:** `shelbi send bravo "--help output looks wrong"` on a local workspace takes the fast path (`uses_buffer_path` is false for local single-line text, lib.rs:88-90) and fails with `Error::Command` — the message never reaches the agent. The error at least propagates, but the failure is input-dependent and will read as random flakiness. Remote sends are immune only because they always take the buffer path.
- **Recommendation:** `["tmux", "send-keys", "-t", &target, "-l", "--", text]` — tmux honors `--` as end-of-flags. One-line change plus a regression test with a dash-leading payload.
- **Effort:** S

## F6: `has_session` reports "no session" for SSH transport failures

- **Where:** crates/shelbi-tmux/src/lib.rs:9-13
- **Category:** failure-scenario
- **Severity / Confidence:** medium / certain
- **Evidence:**

  ```rust
  pub fn has_session(host: &Host, name: &str) -> Result<bool> {
      let out = shelbi_ssh::run(host, ["tmux", "has-session", "-t", name])
          .map_err(shelbi_core::Error::Io)?;
      Ok(out.status.success())
  }
  ```

  `Err` is only returned when the local `ssh`/`tmux` binary fails to spawn. When ssh cannot reach the host (network down, `ConnectTimeout=5` expiry, `BatchMode=yes` rejecting an auth prompt) it exits 255 — indistinguishable here from tmux's exit 1 for "session not found". A dead remote tmux server ("error connecting to socket") is also folded into `false`, which happens to be the desired answer, but transport failure is not.
- **Failure scenario:** remote host briefly unreachable during a poll or dispatch: `kill_workspace_pane` (crates/shelbi-orchestrator/src/workspace.rs:432-433) sees `has_session == false` and returns `Ok(())` **without killing the old pane** — the "kill pane to clear agent context" invariant is silently skipped. If connectivity returns before the next step, `new_session` then fails with tmux's "duplicate session" — or worse, the stale agent session with the previous task's context survives and receives the new task's prompt. Callers that gate creation on `!has_session` (crates/shelbi-orchestrator/src/review.rs:174, lib.rs:292) similarly misread an outage as "needs creating".
- **Recommendation:** distinguish the exit codes: treat ssh's 255 (and spawn-level failures) as `Err`, tmux exit 1 as `Ok(false)`. Checking stderr for `no server running` / `can't find session` vs ssh's connection diagnostics is cruder but versions-proof; exit-code discrimination (255 vs 1) is the cheap 90% fix.
- **Effort:** S

## F7: reverse-forward failures are fully silenced — a stale remote socket file kills worker→hub messaging with no diagnostic anywhere

- **Where:** crates/shelbi-ssh/src/lib.rs:54-66 (opts + rationale comment), :101-110 (`-R` spec)
- **Category:** failure-scenario
- **Severity / Confidence:** medium / likely (OpenSSH behavior is documented; not reproduced against a live remote in this review)
- **Evidence:** every SSH invocation carries `-R /tmp/shelbi-hub.sock:<local hub.sock>` plus:

  ```rust
  "-o", "ExitOnForwardFailure=no",
  "-o", "LogLevel=ERROR",
  ```

  The comment claims "the only real 'forwarding failed' case worth surfacing is the master open, which falls through to the command's regular stderr" — but the master open uses these **same** options: `build_ssh_control_opts()` is applied unconditionally (lines 114-118), so ssh's "Warning: remote port forwarding failed for listen path …" is suppressed on the master too. For Unix-socket remote forwards, sshd only unlinks a pre-existing socket file when the **remote** `sshd_config` sets `StreamLocalBindUnlink yes` (default `no`) — the client cannot force it.
- **Failure scenario:** master opens, forward binds `/tmp/shelbi-hub.sock` on the remote. `ControlPersist=600` lapses (or the hub machine sleeps / the connection drops) and the master dies **without removing the remote socket file** — Unix sockets aren't cleaned up on abnormal teardown. The next master's `-R` bind fails on the leftover file; `ExitOnForwardFailure=no` keeps the connection alive and `LogLevel=ERROR` swallows the warning. Every subsequent command works fine, but remote workers writing to `/tmp/shelbi-hub.sock` talk to a dead file — hub-bound messages are silently lost until someone manually deletes it. Separately, the fixed, predictable path in world-writable `/tmp` lets any other user on a shared remote host pre-create the file and permanently deny the forward (a squat; reading hub traffic is blocked by socket permissions, denial is not).
- **Recommendation:** before opening a master (or on hub startup per remote host), run a cheap `rm -f /tmp/shelbi-hub.sock` guarded by a liveness probe, or place the socket under a per-user directory (`/tmp/shelbi-<uid>/hub.sock`, mode 0700) to kill both the staleness and the squat. Also verify the forward actually works after master open (e.g. remote `test -S` + a ping write) and surface failure to events.log instead of relying on suppressed ssh warnings.
- **Effort:** M

## F8: `run_with_stdin` leaks a zombie and masks the real error when the stdin write fails; payload-then-read shape can deadlock

- **Where:** crates/shelbi-ssh/src/lib.rs:242-252
- **Category:** bug
- **Severity / Confidence:** low / certain (zombie + masked error traced; deadlock is speculative for current callers)
- **Evidence:**

  ```rust
  let mut child = cmd.spawn().map_err(shelbi_core::Error::Io)?;
  {
      let mut child_stdin = child.stdin.take().expect("stdin was piped");
      child_stdin.write_all(stdin).map_err(shelbi_core::Error::Io)?;   // early return
  }
  let output = child.wait_with_output().map_err(shelbi_core::Error::Io)?;
  ```

  If the child dies early (ssh: host unreachable, auth refused — exits within milliseconds), `write_all` on a payload larger than the pipe buffer hits `EPIPE`. The `?` returns before `wait_with_output`: (a) the child is never reaped — `std::process::Child`'s `Drop` does not wait, and the hub is a long-lived daemon, so defunct ssh processes accumulate until hub exit; (b) the returned error is `Io(BrokenPipe)`, while the child's stderr — which holds the actual diagnostic ("Connection refused", "Permission denied") — is discarded.
- **Failure scenario:** remote host goes down; every `send_line` to it returns "broken pipe" with no mention of ssh, and each attempt parks one `<defunct>` ssh process in the hub's process table. Secondary hazard: because the code writes the entire payload before reading any output, a command that emits > ~64 KiB to stdout/stderr before draining stdin deadlocks against a > ~64 KiB payload. Current callers (`tmux load-buffer -`) are quiet on stdout, so this is latent, but the function is a general-purpose public API.
- **Recommendation:** on write error, still `wait_with_output()` and fold the child's stderr into the returned error (a `BrokenPipe` after ssh exited nonzero should surface ssh's message, not the pipe's). For the general case, write stdin from a thread or use non-blocking drain like `std::process`-based helpers typically do.
- **Effort:** S

## F9: `with_permission_mode` idempotence check misses `--permission-mode=<mode>`, silently overriding YAML-pinned modes

- **Where:** crates/shelbi-agent/src/lib.rs:50
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  if spec.flags.iter().any(|f| f == "--permission-mode") {
      return spec.clone();
  }
  ```

  Only the two-token spelling is detected. Claude's CLI (clap-style) equally accepts the single-token `--permission-mode=plan`. The function's own doc comment states the contract: "If the YAML pins a specific mode, respect it rather than silently overriding … quiet override would be surprising" (lines 40-45, test at lines 202-213).
- **Failure scenario:** a project YAML declares `flags: ["--permission-mode=plan"]`. The check misses it, the helper appends `--permission-mode auto`, and the launched command line is `claude --permission-mode=plan --permission-mode auto` — rightmost wins, so the workspace runs in `auto` despite the user's explicit `plan` pin. Exactly the surprise the docstring promises not to deliver, and invisible unless the user inspects the pane's command line.
- **Recommendation:** `f == "--permission-mode" || f.starts_with("--permission-mode=")`. Add the equals-form twin of the existing `with_permission_mode_idempotent_even_when_yaml_mode_differs` test.
- **Effort:** S

## F10: `is_claude_runner` is private, so callers re-implement the basename check

- **Where:** crates/shelbi-agent/src/lib.rs:26-31
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:** the crate keys two behaviors off `is_claude_runner` (`with_permission_mode`, `polls_for_messages`) but keeps it private. `require_auto_mode_supported` in crates/shelbi-orchestrator/src/workspace.rs:787 re-implements it inline:

  ```rust
  if std::path::Path::new(&runner.command).file_name().and_then(|s| s.to_str()) != Some("claude") {
  ```

- **Failure scenario:** the definitions drift — e.g. shelbi-agent later learns to recognize `claude.exe` or a `claude-code` wrapper, the inline copy doesn't, and the version gate and the flag-injection logic disagree about which runners are claude. The existing `claude.exe` test comment (lib.rs:230-233) shows classification edge cases are already being reasoned about in one place but enforced in two.
- **Recommendation:** make `is_claude_runner` `pub` (it's already the crate's semantic core) and use it from workspace.rs.
- **Effort:** S

## F11: palette `search()` re-parses the pattern per entry, re-implements `Pattern::score`, and clones all entries per keystroke

- **Where:** crates/shelbi-palette/src/lib.rs:73-95
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  pub fn score(matcher: &mut Matcher, pattern: &str, label: &str) -> Option<u16> {
      ...
      let needle = Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart);  // per call
      ...
      let scored = needle.atoms.iter().try_fold(0u16, |acc, atom| {
          atom.score(haystack, matcher).map(|s| acc.saturating_add(s))
      });
  ```

  `search()` calls `score()` once per entry (line 91), so the pattern is parsed N times per keystroke, and the atom-fold hand-rolls what `nucleo_matcher::pattern::Pattern::score(haystack, matcher)` already provides. `search()` also clones each matching `Entry` (five `String`-bearing fields) into the result vector on every keystroke.
- **Failure scenario:** none user-visible at current entry counts (dozens); this is cleanliness, not correctness. Sorting is `sort_by_key(Reverse(score))`, which is stable — tie order is preserved, verified fine.
- **Recommendation:** parse the `Pattern` once in `search()` and pass it down (change `score` to take `&Pattern`); use `Pattern::score` instead of the manual fold; consider returning `Vec<(usize, u16)>` indices to avoid the clones.
- **Effort:** S

## F12: ControlPath can exceed the 104-byte `sun_path` limit on long home/user/host combinations

- **Where:** crates/shelbi-ssh/src/lib.rs:97-100
- **Category:** failure-scenario
- **Severity / Confidence:** low / speculative
- **Evidence:**

  ```rust
  let cp = shelbi_state::ssh_control_path_template()
      .unwrap_or_else(|_| "~/.shelbi/ssh/%r@%h".to_string());
  ```

  The template expands to `<$SHELBI_HOME>/ssh/<user>@<host>`. macOS caps `sun_path` at ~104 bytes — a limit the codebase itself knows about (crates/shelbi-state/src/ssh_control.rs:261-265 works around it *in tests* by using `/tmp` instead of the deep `$TMPDIR`). A long `$HOME` (network homes, deeply nested `SHELBI_HOME` overrides) plus a 32-char username and a long FQDN can cross 104; ssh then fails every invocation with `ControlPath too long`, which — one mitigating point — does surface on stderr via `run_capture`. To close the seed question directly: project and workspace names never enter any socket path (`%r@%h` only, and the hub socket paths are fixed), so long *names* cannot trigger this — only long environment-derived paths can.
- **Failure scenario:** user sets `SHELBI_HOME` under a deep project tree; every SSH-routed command to a long-hostname box fails with an ssh error about ControlPath. Recoverable and diagnosable, but the failure lands far from the cause.
- **Recommendation:** use `%C` (hash of local host, remote host, port, user — designed for exactly this) instead of `%r@%h` in the template, or validate the expanded length at startup and warn.
- **Effort:** S

## F13: `kill_window` conflates "already gone" with real failures, so callers swallow everything

- **Where:** crates/shelbi-tmux/src/lib.rs:66-69
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**

  ```rust
  pub fn kill_window(host: &Host, addr: &TmuxAddr) -> Result<()> {
      shelbi_ssh::run_capture(host, ["tmux", "kill-window", "-t", &addr.target()])?;
      Ok(())
  }
  ```

  `kill-window` on a missing window exits nonzero, so idempotent teardown requires ignoring the error — and that's what both callers do (`let _ = shelbi_tmux::kill_window(…)` at crates/shelbi-cli/src/commands/archive.rs:18 and merge.rs:331). But `let _ =` also discards SSH transport failures and dead-server errors, so "we could not reach the host to kill the agent" reads identically to "it was already dead" — an orphaned remote agent keeps running (and burning tokens) after its task was archived or merged.
- **Failure scenario:** `shelbi merge` against a workspace whose SSH host is briefly unreachable: kill fails, error discarded, task completes locally, remote claude session lives on unattended.
- **Recommendation:** have `kill_window` treat tmux's "can't find window/session" stderr as `Ok(())` and return real errors, so callers can drop the `let _ =` and surface transport failures.
- **Effort:** S

---

## Seed questions and checked assumptions that held

- **Shell escaping of `send-keys` payloads:** the buffer path (`load-buffer -` fed via stdin, `paste-buffer -p -d`) is a sound design — payloads with newlines, quotes, `&&`, `$vars` never touch argv or a shell, locally or over SSH (verified by trace and by the crate's stdin round-trip test, shelbi-ssh/src/lib.rs:357-364). The residual holes are the local fast path's `--` (F5) and the shared buffer name (F4), not the transport.
- **`shell_escape` (shelbi-agent/src/lib.rs:72-90):** verified correct POSIX single-quote escaping, including the empty string (`''`), embedded quotes (`'\''`), and newlines; the unquoted-passthrough allowlist (`[A-Za-z0-9._/:=-]`) contains no shell metacharacters. No finding.
- **Socket path length vs long project/workspace names:** names never appear in any Unix-socket path — ControlPath uses `%r@%h`, the hub socket is a fixed name under `SHELBI_HOME`, the remote landing path is fixed. Only environment-derived lengths can overflow (F12).
- **Zombie handling on kill paths:** killing windows/sessions delegates process teardown to tmux (SIGHUP to the pane's process group), which is correct; the one zombie source found is `run_with_stdin`'s early-return path (F8).
- **`pane_title` trimming:** `trim_end_matches(['\n','\r'])` correctly strips CRLF from SSH-routed output without touching interior whitespace.
