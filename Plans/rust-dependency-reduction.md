# Shelbi Rust dependency reduction plan

Status: proposed, 2026-11-09. Analysis of `~/Workspaces/shelbi` at `95d0427d`, covering the root workspace (13 crates under `crates/`) and the separate `desktop/` workspace. Nothing was edited or built during the analysis.

Goal (from the owner): drop dependencies used for something minor that we can replace with a few lines of our own code. Keep dependencies that carry major functionality.

## Summary

- **Direct dependencies:** 49 distinct external crates across normal, dev and build edges. The root has 32 registry crates plus the vendored `alacritty_terminal`, and desktop adds 16 more (gpui, gpui-kit, the objc2 family and others). The root `[workspace.dependencies]` also declares 3 crates that no crate uses: `tokio` (`features = ["full"]`), `gray_matter` and `strip-ansi-escapes`.
- **Transitive size:** the root production build (normal and build edges, aarch64-apple-darwin) reaches **221** external crates. Desktop reaches **570**, mostly through gpui.
- **Proposed removals:** 10 crates leave the root workspace's direct dependencies.
  - Phase 1 and 2 (low risk): `which`, `dirs`, `globset`, and `tempfile` from production code (it stays as a dev-dependency).
  - Phase 4 (decision needed): `termwiz`, `tungstenite`, `chrono-tz`, `similar`, and `inquire`, which is either replaced or upgraded so it shares crossterm.
  - Phase 1 also drops 6 declared-but-unused dependency edges.
- **Transitive crates eliminated**, simulated on the resolved graph:
  - Phase 1 and 2 alone: 8.
  - `termwiz`, `tungstenite` and `chrono-tz`: 70.
  - Everything proposed: **about 88 of 221 (about 40%)** in the root production build.
  - Desktop: about 47 fewer (termwiz accounts for 38).
- **Duplicate versions that disappear:** `syn` 1, `fancy-regex` 0.11, `nix` 0.29 and 0.30, `rand` 0.8 and 0.9, `phf` 0.11 and 0.12, `bitflags` 1, `crossterm` 0.25, `mio` 0.8, `bit-set` and `bit-vec` 0.x, `getrandom` 0.3, `cfg_aliases` 0.2, and `unicode-width` 0.1 (via Phase 3).
- **Compile time and binary size (estimates, not measured):**
  - Several of the heaviest artifacts in the current release target go away: `chrono_tz` (18.7 MB rlib), `zerocopy` (15.2 MB, pulled in through tungstenite's `rand`), `termwiz` (13.9 MB), `terminfo` (7.7 MB), `pest` and `pest_meta` (9.7 MB), and `nom` (4.0 MB). Several proc-macro chains also go.
  - Expect a clean root build to be roughly 15 to 30% faster.
  - The 22 MB release binary probably shrinks less, because thin LTO already strips most of termwiz. The chrono-tz tables and the WebSocket and HTTP code are linked in today.
  - Measure with `cargo build --timings` and `cargo bloat` before trusting any of these figures.

## Method

1. Listed every direct dependency from `cargo metadata --offline` in both workspaces.
2. Counted call sites by grepping `\bname::` and `use name` in each crate's `src`, `tests` and `build.rs`, splitting test-only references from production ones.
3. Measured each dependency's unique transitive cost: removed its edges from every workspace member in `resolve.nodes` (aarch64-apple-darwin, normal and build edges) and counted the crates that became unreachable. Combined removals were simulated the same way.
4. Found duplicate versions with `cargo tree --duplicates` and traced where each one comes from with `cargo tree -i`.
5. Estimated relative weight from rlib sizes in `target/release/deps`.
6. Checked upgrade feasibility with `cargo info`: inquire 0.9.4, ratatui 0.30.2, ratatui-crossterm 0.1.2.

## Direct dependency table

"Unique / closure" is the number of crates removed from the root production build if only this dependency goes, counting itself, followed by the size of its full transitive set.

### Root workspace

| Dependency | Version | Used by | Usage | Unique / closure | Verdict |
|---|---|---|---|---|---|
| anyhow | 1 | cli, orchestrator, session, tui (+client dev) | 112 refs | 1 / 1 | KEEP |
| thiserror | 1 | app, client, core, orchestrator, proto, state; session and ssh **unused** | 17 refs | 0 (portable-pty needs v1) | KEEP, drop 2 unused edges |
| serde (+derive, rc in app) | 1 | 8 crates | pervasive | 0 / 7 | KEEP |
| serde_json | 1 | 7 crates + tui build/dev | pervasive | 2 / 5 | KEEP |
| serde_yaml | 0.9.34+deprecated | cli, core, orchestrator, state | 439 refs | 4 / 14 | KEEP (deprecated upstream, see open questions) |
| serde_ignored | 0.1 | cli | 2 refs (`config_surfaces.rs:1181,1256`) | 1 / 2 | KEEP |
| clap (derive, env) | 4 | cli | 35+ files | 10 / 17 | KEEP (feature `color` is an open question) |
| chrono (serde) | 0.4 | 7 crates | 288 refs | 0 / 12 | KEEP |
| chrono-tz | 0.10 | orchestrator | 1 type (`ready.rs:797,813`) | 3 / 16, 18.7 MB rlib | REPLACE-INLINE (Phase 4) |
| tracing | 0.1 | 8 crates (session unused) | many | 0 / 9 | KEEP, drop the session edge |
| tracing-subscriber (env-filter, fmt) | 0.3 | cli, state dev | 1 init site (`main.rs:780`), 2 test captures | 5 / 23 | FEATURE-TRIM (Phase 1), replace optionally in Phase 4 |
| dirs | 5 | cli, orchestrator, state | `home_dir()` only, 20 refs | 3 / 4 | REPLACE-WITH-STD |
| which | 6 | ssh (normal dependency) | 1 call, **test only** (`shelbi-ssh/src/lib.rs:2549`) | 1 / 8 | REPLACE-INLINE |
| globset | 0.4 | orchestrator | 1 function (`zen.rs:45`, `11287-11304`) | 2 / 8 | REPLACE-INLINE |
| tempfile | 3 | cli (normal), dev in 5 crates | 1 production site (`issue_editor.rs:89`) | 2 / 10 | REPLACE-INLINE in production, KEEP as dev |
| regex | 1 | app, orchestrator; cli unused | 3 refs | 0 / 5 | KEEP, drop the cli edge |
| nucleo-matcher | 0.3 | palette; cli unused | 5 refs | 1 / 3 | KEEP, drop the cli edge |
| similar (text) | 2 | app | 1 Myers call (`diff/intraline.rs:6,164`) | 1 / 1 | REPLACE-INLINE (optional) |
| syntect (no defaults) | 5.3 | app | `diff/highlight.rs` | 0 / 29 | KEEP (already trimmed) |
| two-face | 0.5 | app | 1 call | 1 / 30 | KEEP |
| pulldown-cmark (no defaults) | 0.13 | app | `review.rs:266-320` | 2 / 5 | KEEP |
| ratatui | 0.28 | tui, cli | 274 refs | 21 / 51 | KEEP, align versions (Phase 3) |
| crossterm | 0.28 | tui, cli | 102 refs | 0 / 18 | KEEP, align versions (Phase 3) |
| inquire | 0.7 | cli | 15 prompts in 8 files | 7 / 28, pulls in crossterm 0.25 | CONSOLIDATE via upgrade, or REPLACE |
| unicode-width | 0.1 | tui, cli | 15 refs | 0 (pinned by ratatui 0.28) | CONSOLIDATE to 0.2 |
| signal-hook | 0.3 | cli, orchestrator | `flag::register`, `iterator::Signals` | 0 (crossterm needs it) | KEEP |
| libc | 0.2 | 8 crates | 207 refs | 1 / 1 | KEEP |
| tungstenite | 0.29 | orchestrator | WebSocket client and relay (`codex_rpc.rs`, `wake.rs`) | 11 / 29 | REPLACE-INLINE (Phase 4) |
| termwiz (no defaults) | 0.23 | term | **1 call** (`input.rs:180`) | **47 / 89** | REPLACE-INLINE (Phase 4, recommended) |
| portable-pty | 0.9 | session | 4 refs | 6 / 19 | KEEP |
| alacritty_terminal (vendored) | 0.26 fork | session, term, tui, cli | emulator core | 8 / 33 | KEEP, gate its unused `tty` module |
| syn, proc-macro2 | 2 / 1 | cli dev | source-gate test | dev only | KEEP |
| *tokio (full)* | 1 | **no crate** | 0 | n/a | delete the declaration |
| *gray_matter* | 0.2 | **no crate** | 0 | n/a | delete the declaration |
| *strip-ansi-escapes* | 0.2 | **no crate** | 0 | n/a | delete the declaration |

### Desktop workspace

| Dependency | Usage | Unique in desktop | Verdict |
|---|---|---|---|
| gpui (`gpui-pre` =0.3.9), gpui_platform | GUI framework | 0 | KEEP |
| gpui-kit =0.7.3 | 59 refs in 40 files | 77 | KEEP |
| objc2, block2, objc2-foundation, objc2-app-kit, raw-window-handle | AppKit integration | 0 (in gpui's tree) | KEEP |
| futures | `channel::mpsc` and `oneshot` only | 0 | optional CONSOLIDATE with async-channel |
| async-channel | 24 refs | 0 | KEEP |
| serde, serde_json, chrono, tracing | general use | 0 | KEEP |
| tracing-subscriber | 1 init site | 6 | follow the root feature trim |
| markdown, ttf-parser, fontdb, unicode-width 0.2, sha2, image, flate2, libc | features | 0 | KEEP |

Desktop savings come from the shared crates: shelbi-term carries termwiz (38 unique crates), and shelbi-orchestrator carries tungstenite and chrono-tz (9).

## Phase 1: zero-risk cleanup, std replacements and feature trims

**1.1 Remove declared-but-unused dependencies.**
- Root `Cargo.toml:46` `gray_matter`, `:54` `tokio`, `:55` `strip-ansi-escapes`.
- shelbi-cli `nucleo-matcher` and `regex`.
- shelbi-session `thiserror` and `tracing`.
- shelbi-ssh `thiserror`.
- Risk: none.
- Test: `cargo build --workspace --all-targets` and clippy with `-D warnings`. Optionally run `cargo machete` once to confirm nothing else is unused.

**1.2 Turn `which` into a test helper.**
- Its only call is the `#[cfg(test)]` code at `shelbi-ssh/src/lib.rs:2549`, but it is a normal dependency, so it ships in the binary.
- Replace it with an 8-line PATH lookup (`std::env::split_paths` + `is_file`), then delete `which` from the manifests.
- Risk: none.

**1.3 Replace `dirs` with `std::env::home_dir`.**
- Call sites:
  - `shelbi-state/src/root.rs:195,266,282,287`
  - `shelbi-state/src/tools.rs:573`
  - `shelbi-orchestrator/src/project_create.rs:244,246`
  - `shelbi-cli/src/wizard.rs:1632`, `commands/config.rs:574`, `commands/daemon/lifecycle.rs:75,84`
  - tests
- Add a 3-line `shelbi_core::home_dir()` as the single choke point. On Unix, std matches what `dirs` does today (`$HOME`, then `getpwuid_r`), and MSRV 1.88 is past the deprecation fix.
- Match `dirs` on an empty `HOME`: it returns `None`.
- This removes `dirs`, `dirs-sys` and `option-ext`. Update `shelbi-app/src/licenses.rs:108`, which lists `option-ext`.

**1.4 Feature-trim `tracing-subscriber`.**
- Change the dependency to `default-features = false, features = ["std", "fmt", "ansi", "env-filter"]` in both workspaces. This drops `tracing-log` and `smallvec`.
- Optional: switch `EnvFilter` to `filter::Targets` parsed from `SHELBI_LOG`, which removes `matchers`. `SHELBI_LOG` would then accept only plain `target=level` syntax.
- Tests: `probe_stdout_stays_json_when_cap_cleanup_logs` (cli `main.rs:839`) and the shelbi-state capture tests.

## Phase 2: small inline reimplementations

**2.1 Replace `globset` with an in-house glob matcher (about 70 to 90 lines plus tests).**
- Call site: `match_danger_paths` at `shelbi-orchestrator/src/zen.rs:11287-11304`. Patterns come from `BUILTIN_DANGER_PATHS`, shape presets and user YAML.
- Preserve globset's default `literal_separator(false)` exactly: `*` crosses `/`, so `*.yaml` matches `a/b/c.yaml`. The built-in patterns depend on this.
- Design: tokens for literal, `?`, `*`, `**`, `[..]` / `[!..]` and `{a,b}`, matched by a backtracking matcher. An invalid pattern is an `Err` that the caller skips, as today.
- Before deleting globset, write a parity table test that runs both engines over the built-in patterns, shape patterns and edge cases (`**/x`, `x/**`, `{a,b}`, `[!a]`, `?`).
- Removes `globset` and `bstr`.

**2.2 Replace production `tempfile` with a 20-line temp-dir guard.**
- The only production call is `shelbi-cli/src/commands/issue_editor.rs:89`.
- Guard design: a `TempDir { path, keep }` whose `Drop` removes the directory unless `keep` is set. Create it by looping `create_dir(temp_dir()/prefix-pid-nanos-n)` until the call doesn't return `AlreadyExists`.
- Move `tempfile` to dev-dependencies. Tests use it heavily (330 refs), so it stays there.
- Removes `tempfile` and `fastrand` from the shipped binary.

**2.3 (optional) Drop `regex` from shelbi-app.**
- `diff/search.rs:25` is a smart-case literal search that could be a 25-line char walker.
- Saves no crates, because the orchestrator's user-facing `--sub-regex` and syntect still need regex. Low priority.

## Phase 3: version alignment

**3.1 Align the TUI stack on one crossterm and one unicode-width.**
- Today ratatui 0.28 uses crossterm 0.28 and unicode-width 0.1, while inquire 0.7 uses crossterm 0.25, mio 0.8 and bitflags 1.
- Target versions:
  - `ratatui = "0.30"` (MSRV 1.88)
  - `crossterm = "0.29"`
  - `unicode-width = "0.2"`
  - `inquire = { version = "0.9", default-features = false, features = ["crossterm"] }`
- Effect:
  - Removes about 6 to 8 crates.
  - Fixes two raw-mode state machines (one per crossterm version) running in one process.
- Risk: medium.
  - ratatui 0.30 split into `ratatui-core` and `ratatui-widgets`.
  - The upgrade touches 274 ratatui and 102 crossterm references.
- Test: the full TUI suite, plus a manual pass over the wizard and the shell.

**3.2 thiserror 1 and 2: leave both.** Moving our crates to v2 removes nothing while portable-pty needs v1.

**3.3 (desktop, optional) futures vs async-channel.** Consolidating gives a consistent channel API but saves no crates.

## Phase 4: larger candidates that need a decision

**4.1 Replace termwiz with an in-house key encoder (recommended; the biggest single win).**
- Usage: one call at `shelbi-term/src/input.rs:180`, `KeyCode::encode(mods, KeyCodeEncodeModes, true)`, wrapped by `Key::to_termwiz`, `Modifiers::to_termwiz` and `KeyEncoding::modes`.
- Cost today: 47 unique crates in the root build and 38 in desktop, including terminfo, pest, nom, phf 0.11, syn 1, fancy-regex 0.11, nix 0.29 and 0.30, rand 0.8, uuid and the wezterm-* family. That is over 40 MB of rlibs.
- Replacement (about 150 to 200 lines) implements a fixed, well-specified table.
- **Legacy and xterm mode:**
  - Characters: Ctrl maps to C0 control codes, and Alt prefixes ESC.
  - Enter: `\r`, or `\r\n` under LNM.
  - Tab: `\t`. Shift+Tab: `ESC[Z`. Backspace: `0x7f`. Escape: `0x1b`.
  - Arrows: `ESC[A`, or `ESC O A` under DECCKM. With modifiers: `ESC[1;{m}A`.
  - Home and End: `ESC[H` / `ESC[F`, using SS3 under DECCKM.
  - Insert, Delete, PgUp and PgDn: `ESC[{2,3,5,6}~`.
  - F1 to F4: SS3 `P` to `S`. F5 to F24: `ESC[{15..24}~`.
  - Modifier parameter: `m = 1 + shift + 2*alt + 4*ctrl + 8*super`.
- **CSI-u (kitty) mode:** `ESC[{codepoint};{m}u` for keys that are ambiguous in legacy encoding. Keys with legacy CSI forms keep those forms, matching termwiz's `CsiU` behavior.
- Risk: medium. Key encoding is user-visible, for example Claude Code's Shift+Enter and Tab completion.
- Test plan:
  1. While termwiz is still a dependency, check in a golden fixture of termwiz's output for every key, all 16 modifier sets and all 8 encoding modes.
  2. Assert the new encoder matches the fixture byte for byte.
  3. Keep the 27 existing input tests.
  4. Remove termwiz.
- Also update the Acknowledgements lists and their assertions in `shelbi-app/src/licenses.rs`, `shelbi-tui/build.rs` and `shelbi-tui/src/licenses.rs`.

**4.2 Replace tungstenite with a minimal WebSocket over `UnixStream` (about 300 lines).**
- Usage:
  - The Codex app-server client (`codex_rpc.rs:17,90`).
  - The wake relay, which acts as both client and server (`wake.rs:34,1314-1410`).
  - Test servers that use `accept`.
- Cost: 11 unique crates, including http, httparse, bytes, sha1, data-encoding, rand 0.9 and zerocopy (15 MB). There is no feature trim available.
- Replacement:
  - The HTTP/1.1 Upgrade exchange.
  - `Sec-WebSocket-Accept`, which needs SHA-1 (about 60 lines) and base64 encoding (about 15 lines).
  - RFC 6455 framing: 7/16/64-bit lengths, client masking, continuation frames, Ping→Pong, and the Close handshake.
  - The `WouldBlock`/timeout behavior at `codex_rpc.rs:291` and `wake.rs:1450` must keep working. Buffer partial frames instead of losing bytes on a timeout.
- Risk: medium-high. This is a production wire protocol with an external peer.
- Test plan:
  - Port the existing tests.
  - Add fragmentation, large-frame and timeout-mid-frame tests.
  - Do a live end-to-end run against a real `codex app-server` before merging.

**4.3 Remove chrono-tz.**
- `ready.rs:797,813` parses an IANA zone name from Claude's reset hint (for example `"7:20am (America/Los_Angeles)"`). Today that compiles the whole tz database into the binary.
- **Option A: read the system tzdb (about 150 to 200 lines).**
  - Read the file from `/usr/share/zoneinfo`, rejecting names with `..` or a leading `/`.
  - Parse TZif v2+, plus the POSIX TZ footer for instants after the last transition.
- **Option B: local zone only (about 25 lines; recommended if acceptable).**
  - If the hint names the hub's local zone (`$TZ`, else the `/etc/localtime` symlink target), use `chrono::Local`.
  - Otherwise return `HintZone::Unrecognized`, which is already the documented "don't guess" path.

**4.4 Replace inquire entirely (an alternative to 3.1's upgrade).**
- Usage: 15 prompts built from `Confirm`, `Select` and `Text`, using `with_default`, `with_starting_cursor`, one validator and one help message.
- Replacement: about 200 to 250 lines on crossterm, with a raw-mode drop guard.
- Recommendation: do 3.1 first, and replace inquire only if what remains still bothers you.

**4.5 Replace similar with an in-house Myers diff (about 80 to 100 lines; optional).**
- Saves 1 crate and about 1 MB. Intraline highlights could shift on ties.
- Before removing similar, add a golden comparison test.

**4.6 Compile out the unused PTY layer in the vendored alacritty_terminal.**
- Neither workspace uses `tty`, `event_loop` or `sync`. Gating them drops `polling`, `rustix-openpty` and `signal-hook` 0.4, and removes the signal-hook duplicate.
- Cost: a second divergence from upstream, to record in `vendor/alacritty_terminal/VENDORING.md`. That is a policy decision.

**4.7 Replace tracing-subscriber with our own subscriber (about 100 to 120 lines; optional).**
- Removes 5 crates.
- Risk: medium, from span handling and global-default semantics.
- Lean: no, unless the goal is maximal reduction.

**4.8 portable-pty: keep.** It owns controlling-terminal, group-kill and file-descriptor-leak behavior that the runtime spike proved. A rewrite would save 6 crates at real correctness risk.

## Keep, and why

- **serde, serde_json, serde_yaml:** core config and state formats.
- **anyhow, thiserror:** the standard error pair.
- **clap:** the whole CLI.
- **chrono:** 288 refs.
- **tracing:** pervasive logging.
- **ratatui, crossterm:** the TUI toolkit.
- **alacritty_terminal:** the terminal emulator.
- **portable-pty:** PTY ownership.
- **libc:** syscalls.
- **signal-hook:** crossterm keeps it in the tree anyway.
- **syntect, two-face:** syntax highlighting, already trimmed.
- **pulldown-cmark:** CommonMark parsing.
- **nucleo-matcher:** fuzzy scoring.
- **regex:** the user-facing `--sub-regex`.
- **serde_ignored:** has no dependencies of its own.
- **tempfile, syn, proc-macro2:** dev-only.
- **Desktop:** gpui, gpui-kit, the objc2 family, fontdb, ttf-parser, markdown, sha2, async-channel, futures, image and flate2. All except gpui-kit are already in gpui's tree.

## Open questions for the owner

1. **termwiz (4.1):** OK with a roughly 200-line in-house key encoder, gated by a golden table generated from termwiz first? It is the largest single reduction (47 crates).
2. **tungstenite (4.2):** is a roughly 300-line WebSocket implementation acceptable for the Codex client and relay?
3. **chrono-tz (4.3):** Option A (system tzdb) or Option B (local zone only)?
4. **TUI stack (3.1 vs 4.4):** upgrade to ratatui 0.30, crossterm 0.29 and inquire 0.9, or replace inquire outright?
5. **Vendored alacritty (4.6):** may the fork take a second divergence to compile out `tty` and `event_loop`?
6. **`SHELBI_LOG` syntax (1.4):** is plain `target=level` enough, so `env-filter` can go?
7. **clap `color`:** keep colored help and errors? Dropping the feature removes 4 crates.
8. **serde_yaml** is deprecated upstream. Should a later, separate project move to a maintained fork such as `serde_yaml_ng` or `serde_norway`? That touches 439 refs.
9. **similar (4.5) and tracing-subscriber (4.7):** pursue these too, or stop at the high-payoff items?

## Implementation tasks

Each task is one PR. T1 to T5 are independent and not gated. T6 to T9 depend on the open questions.

- **T1 `deps-prune-unused` (1.1):**
  - Remove the declared-but-unused dependencies.
  - Acceptance: workspace build, clippy and tests pass, and the desktop builds locally.
- **T2 `deps-std-home-which` (1.2, 1.3):**
  - Add `shelbi_core::home_dir()`, inline the PATH lookup, remove `dirs` and `which`, and update the licenses list.
  - Acceptance: `cargo tree -i dirs` and `cargo tree -i which` are empty, and the `root.rs` and Acknowledgements tests pass.
- **T3 `deps-tracing-subscriber-trim` (1.4):**
  - Make the feature trim in both workspaces.
  - Acceptance: `tracing-log` is gone from the production trees, the stdout-JSON and capture tests pass, and TUI-mode logs still reach the log file.
- **T4 `deps-globset-inline` (2.1):**
  - Write the parity table first, then add the in-house matcher and remove globset.
  - Acceptance: the parity table passes and `cargo tree -i globset` is empty.
- **T5 `deps-tempfile-prod` (2.2):**
  - Add the temp-dir guard and move `tempfile` to dev-dependencies.
  - Acceptance: `cargo tree -e normal -i tempfile` is empty for the binary, the guard's tests pass, and a manual `shelbi issue edit` works.
- **T6 `deps-tui-stack-align` (3.1, gated on Q4):**
  - Do the version alignment.
  - Acceptance: no crossterm, mio or unicode-width duplicates remain, the TUI suite passes, and manual wizard and shell passes look right.
- **T7 `deps-termwiz-encoder` (4.1, gated on Q1):**
  - Check in the golden fixture first, then the encoder, then remove termwiz.
  - Acceptance: the fixture and existing tests pass, `cargo tree -i termwiz` is empty in both workspaces, and a manual Claude Code check passes (Shift+Enter, Tab, Shift+Tab, DECCKM arrows in vim and less).
- **T8 `deps-websocket-inline` (4.2, gated on Q2):**
  - Add the `ws` module, port `codex_rpc` and the `wake.rs` relay, and add the new framing tests.
  - Acceptance: a live Codex app-server run delivers a wake and relays a session.
- **T9 `deps-chrono-tz` (4.3, gated on Q3):**
  - Implement Option A or B and remove chrono-tz.
  - Acceptance: the zone tests pass.
- **T10 (optional) `deps-alacritty-tty-gate` (4.6, gated on Q5).**
- **T11 (optional) `deps-similar-inline` (4.5).**
- **T12 (optional) `deps-own-subscriber` (4.7).**

After T7 to T9 land, record before and after numbers in the PR body:
- `cargo build --release --timings` clean-build time
- the size of `target/release/shelbi`
- the crate count from `cargo tree -e normal --prefix none | sort -u | wc -l`

Those replace this plan's estimates with real figures.
