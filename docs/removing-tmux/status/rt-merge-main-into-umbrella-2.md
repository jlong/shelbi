# rt-merge-main-into-umbrella-2

Status: **done** — `origin/main`'s one new commit `0a599940` (#1493, the Zen
hub-target cap / probe-worktree sweep / low-disk warning) merged into
`jlong/remove-tmux` with a real merge commit. Every conflict resolved so both
sides' intent survives, and #1493's behavior survives whole on the session
backend. `cargo build --workspace`, `cargo clippy --workspace --all-targets --
-D warnings`, and the targeted #1493 tests are green locally; `site/` `npm run
lint` and `npm run build` green.

## Main commit carried in

`0a599940` (#1493) — the only commit `origin/main` was ahead of the umbrella.
Its behavior (probe cap/sweep in `zen.rs`, low-disk heartbeat tokens + `shelbi
status` warning, the `disk:` project config + sniffer, and all its tests) is
fully present.

## Conflicted files and how each was resolved

| File | Resolution |
| --- | --- |
| `crates/shelbi-cli/src/commands/open/pane.rs` | **modify/delete → kept deleted** (`git rm`). The umbrella deleted `open --as-pane` (`rt-cutover-delete`); #1493 only added `disk: DiskConfig::default()` to a `Project` test-helper literal in a deleted-anyway file. No replacement needs it — the surviving session-backend `Project` literals get the field below. |
| `crates/shelbi-orchestrator/src/lib.rs` | Took the umbrella's version (`--ours`). The whole conflict region was the umbrella-deleted tmux test modules (`reload_target_tmux_tests` / `reload_workspace_tmux_tests`); #1493's only two changes to this file were `disk:` lines inside those deleted helpers, so nothing of value was lost. |
| `crates/shelbi-state/src/lib.rs` | Union — kept the umbrella's module decls (`change_bus`, `daemon_lifecycle`, `daemon_poller`) **and** #1493's `pub mod disk;`, in alphabetical order. #1493's disk/lock helpers (`acquire_file_lock_shared`, `try_acquire_file_lock_exclusive`, `ZenTargetLock`, etc.) and the `format_gib` re-export auto-merged outside the conflict. |
| `site/content/docs/configuration/project.mdx` | Kept #1493's new `disk` table row (and its `## disk` section, which auto-merged) **and** the umbrella's reworded `git` row ("the review merge and Zen's auto-merge"). #1493 never touched the `git` wording; the conflict was only the adjacency of the inserted `disk` row to the umbrella's edited `git` row. |

## #1493's new `disk` field threaded into umbrella-only `Project` literals

`#1493` added a required `disk: DiskConfig` field to `shelbi_core::Project` and
updated every full `Project {…}` literal that existed on main. Two full literals
live in files the umbrella added after main's branch point, so #1493 never saw
them and `cargo clippy --all-targets` flagged both as missing the field. Added
`disk: shelbi_core::DiskConfig::default(),` (matching #1493's placement, right
after `issue_tracker`) to:

- `crates/shelbi-orchestrator/src/migration.rs` (migration test helper)
- `crates/shelbi-orchestrator/src/review_session.rs` (review-session test helper)

## Poller rename auto-merge

Git rename-detected main's `crates/shelbi-tui/src/poller.rs` →
the umbrella's `crates/shelbi-orchestrator/src/poller.rs` and auto-merged
#1493's heartbeat/low-disk block (`low_disk_free_bytes`, the `disk_low` arg to
`append_heartbeat_event`, and two tests) cleanly. Unlike #1507 last merge, these
hunks use fully-qualified `shelbi_core::` / `shelbi_state::` paths and the new
tests probe `temp_dir` without the test lock, so **no crate-path or
test-convention rewrite was needed** — verified: no `shelbi_tui::` /
`test_support::ENV_LOCK` / `TmuxAddr` references remain in the file, and the
pre-existing `local_project` helper merged with both the umbrella's `session:`
field and #1493's `disk:` field.

## Verification

- `cargo build --workspace` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `#1493` tests, all green (run with `env -u TASK_ID -u PROJECT … --test-threads=1`):
  - `shelbi-orchestrator`: `cap_clears_target_over_cap_keeps_under_and_skips_when_in_use`,
    `sweep_removes_stale_probe_worktree_but_keeps_a_live_one`,
    `probe_worktree_is_registered_on_add_and_removed_on_signal_cleanup`,
    `probe_worktree_owner_pid_parses_only_a_numeric_owner`, and both
    `low_disk_free_bytes_*` poller tests.
  - `shelbi-state`: `free_space_*` (3), `heartbeat_low_disk_appends_warning_tokens_and_stays_quiet_above`, `format_gib_renders_one_decimal`.
  - `shelbi` (cli): `disk_warning_fires_below_threshold_and_is_quiet_above`; `config_upgrade` suite (118 pass, incl. the `disk:` sniffer).
- `site/`: `npm run lint` and `npm run build` — clean.
- `Cargo.lock` unchanged by the merge (#1493's `Cargo.toml` change is a
  `[profile.dev.package."*"]` debug override, not a new dependency) → no MSRV
  re-check needed.

## Notes

- AC1 is content-based (the squash-merge erases this merge commit's ancestry):
  `git merge-tree --write-tree origin/main origin/jlong/remove-tmux` should
  merge cleanly once this subtask lands on the umbrella. The orchestrator then
  records ancestry with a content-free `-s ours` merge.
