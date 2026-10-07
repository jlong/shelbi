# rt-sidebar-focus-rules

Done. Nav sidebar focus rules now consistent across row types.

- `show()` moves focus to the main area for every opened target (the
  `RowTarget::Native` arm now calls `focus_main()`, matching Session/Review).
- `handle_sidebar_key`: added `j`/`k` (select down/up) and `Space` (open, like
  Enter). Arrows/Tab/BackTab still move the selection only; `q`/palette/Ctrl+H-L
  unchanged.
- Opening via click/Space/Enter now lands focus on main for native views too, so
  navigation after an open drives the main view (updated two existing tests that
  assumed Enter kept sidebar focus; they re-focus the sidebar between opens).
- Tests added: j/k navigate without opening/focusing main; Space opens + focuses
  main (native + session); wheel keeps sidebar focus. Click-native test now
  asserts `Focus::Main`.

`cargo build`/`clippy`/shell tests green. No `Cargo.lock` change.
