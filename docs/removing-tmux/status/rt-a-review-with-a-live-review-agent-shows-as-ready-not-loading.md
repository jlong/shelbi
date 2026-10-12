# rt-a-review-with-a-live-review-agent-shows-as-ready-not-loading

Done. A live review whose workflow declares no health-checkable server (no
`ready:` probe — e.g. ContextStore's URL-less review) now reads **Ready for
Review** (bold cyan ✓) instead of stuck on the loading glyph.

- New shared decision in `shelbi-orchestrator::workspace`: `review_slot_serving`
  (pure) + `review_slot_is_serving` (wires marker + liveness) +
  `review_workflow_has_health_check` / `workflow_declares_health_check`. A slot
  is serving when its `.claude/shelbi-review-loaded` marker names the task **or**
  the workflow declares no `ready:` health check and the review-agent session is
  live. A review that *does* declare a health check keeps waiting for its marker.
- Poller `handle_review_slot`: a URL-less review with a live session now writes
  the marker + records `serving` (so `review-ready` fires and the sub-state is
  correct), instead of falling through to a perpetual loading observation.
- Both sidebars (shell `SidebarModel` + legacy `app.rs`) route through the shared
  predicate so they can't drift.
- Loading glyph recolored yellow → muted `#7a7a7a` via new
  `DecorationColor::Muted`; serving ✓ now renders bold cyan to match the review
  panel's "Ready for review" header.
- Tests: pure decision truth table, health-check predicate across review shapes,
  decoration colors, and a styled-cell render test for bold-cyan ✓ / muted ▶.
