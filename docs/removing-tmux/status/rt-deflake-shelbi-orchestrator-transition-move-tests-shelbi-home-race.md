# rt-deflake-shelbi-orchestrator-transition-move-tests-shelbi-home-race

Done. Fixed a drop-order race that let a sibling test observe a cleared
`SHELBI_HOME` and resolve the real `~/.shelbi`.

- Root cause: `supervise_restart_orchestrator_tests::seed_project` (in
  `crates/shelbi-orchestrator/src/lib.rs`) returned
  `(Fixture, MutexGuard<'static, ()>)` and the two restart tests bound it as
  `let (_fx, _lock) = seed_project(...)`. Destructured bindings drop in reverse
  declaration order, so `_lock` (the crate `test_lock`) released **before**
  `_fx` ran its `Drop`, which is where `SHELBI_HOME` is removed. In that window
  another lock-holding test — e.g. the `transition_move_tests` accept-edge test
  — could run with `SHELBI_HOME` unset and fall through the `shelbi_state`
  resolver to the `~/.shelbi` fallback, loading the built-in default workflow
  and emitting the `could not be loaded ... using built-in default` warning the
  test asserts against. Verified the drop order empirically with a throwaway
  program: for `let (a, b) = (A, B)`, `b` drops first.
- Fix (tests/test-helpers only): fold the lock into `Fixture` as a `_lock`
  field and return just the `Fixture`. `Fixture::drop` restores the env in its
  body, which runs before the struct's fields drop, so the lock is still held
  through the restore. This matches the established idiom in the crate
  (`quit::tests::Home`, `transition_move_tests::Fixture`,
  `migration::tests::HomeGuard`), which embed the lock in the guard for exactly
  this reason. Call sites now `let _fx = seed_project(...)`.
- Audit: swept every `shelbi-orchestrator` test (src unit + integration) that
  mutates `SHELBI_HOME`/`HOME`. All others already hold `crate::test_lock`
  across the restore — either inline (`let _g = acquire(); ...; remove_var()`
  before the function returns, lock still in scope) or via a guard struct whose
  `Drop` body restores env before the lock field drops. `seed_project` was the
  only broken case.
- Detection: the suite-wide `assert_test_home_is_isolated()` guard already
  exists in `shelbi-state`'s `root::resolve()` under `#[cfg(test)]`, but it only
  compiles into **shelbi-state's own** test binary. When `shelbi-orchestrator`
  tests call through, `shelbi-state` is a non-test dependency build, so the
  guard is absent — which is why this leak was silent. Making it fire for the
  orchestrator binary would require an always-compiled, runtime-gated check in
  the production resolver, which the task's "no production behavior change"
  constraint forbids, so I did not add one. The structural fix (single guard,
  lock-held-through-restore) removes the failure class instead.
- Verified: `cargo clippy -p shelbi-orchestrator --all-targets -- -D warnings`
  clean; the two restart tests pass; the full `shelbi-orchestrator` lib test
  binary passed 20 consecutive runs at default parallel threads.

## Port to `main`

Correction to the task's port note: the failing test
(`accept_edge_merges_before_the_status_write_then_deletes_the_branch`) exists on
`main`, but the **offender** does not. `seed_project` /
`supervise_restart_orchestrator` and the `tests_support_restart::Fixture` helper
are umbrella-only (not present in `origin/main`'s `shelbi-orchestrator/src/lib.rs`
— `git grep` reports 0 matches there, 3 on `origin/jlong/remove-tmux`). So the
drop-order race I fixed cannot occur on `main`, and there is nothing to port for
this specific fix.

If `accept_edge_merges_...` ever flakes on `main` with the same
`using built-in default` warning, it is a *different* `SHELBI_HOME` offender and
needs its own audit of `main`'s env-mutating tests (the same technique: find any
test that releases `crate::test_lock` before restoring `SHELBI_HOME`, e.g. via a
guard dropped after the lock). I did not find such an offender on `main`, but I
did not exhaustively stress-run `main` since this task's branch is the umbrella.
