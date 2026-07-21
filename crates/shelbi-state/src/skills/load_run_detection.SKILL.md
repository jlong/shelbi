---
name: load-run-detection
description: >
  How the Review agent boots a branch for review: run the workflow's serve
  recipe (handed to you in your dispatch prompt, fully resolved) verbatim,
  health-check it, and hand a URL to the human. Use when a review workspace
  has a branch checked out and you need to stand up its dev server.
---

# Running the review serve recipe

You are on a review workspace and need to make the checked-out branch
**runnable** for a human. The *how* is not yours to guess — it is declared
by the workflow and handed to you, already resolved, in your dispatch
prompt.

## The recipe is the source of truth

Look in your dispatch prompt for a **`## Review serve recipe`** section. It
carries, with the review slot's port **already substituted in**:

- a **working directory** (relative to the worktree root),
- an optional **setup** command (install/build — must exit 0),
- the **serve** command (the dev server, bound to your slot's port),
- an optional **ready** probe, and
- the **reviewable URL**.

Run it **verbatim**. Do not auto-detect the framework, do not rewrite the
port flag, and do not read a `PORT` environment variable to pick a port —
the workflow already made these decisions, workflow-scoped, so a monorepo's
app / site / docs each serve correctly and concurrent review slots don't
collide on a port.

## No recipe → diff-only

If your dispatch prompt has **no** `## Review serve recipe` section, the
workflow declares no way to serve this branch. That is a **diff-only**
review: report it in your summary, emit the ready signal with no URL, and
stop. **Never** fabricate a server or fall back to a framework default
port — an unrequested server on port 3000 is the exact failure this
prevents.

## Unresolved port → stop and report

If the serve command still contains a literal `$SLOT` / `$PORT`, your
review workspace has no assigned slot. Do **not** guess a port — stop and
report the misconfiguration.

## Launching (managed pane)

Hand the serve command to shelbi so it runs in a **managed server pane**
shelbi can watch and reap, not a bare background job:

```sh
shelbi workspace serve "$SHELBI_WORKSPACE" --serve '<the recipe serve command>'
```

Prepend the recipe's working directory when it names one (e.g.
`--serve 'cd site && npm run dev -- -p 4310'`). The port is already in the
command — there is nothing to fill in. Running it again tears down any
existing server and starts fresh, which is also how you refresh for a new
branch or a follow-up tweak. shelbi runs the HTTP ready-probe for you and
prints the URL.

## Readiness

`shelbi workspace serve` blocks on the HTTP ready-probe before it returns,
so for a normal web app you just wait for it to report the URL. When the
recipe supplies its own `ready` probe, that is the authoritative check —
poll it until it exits 0 (or times out). Your fallbacks when the app has no
HTTP surface (`--no-wait`, then confirm manually):

1. **HTTP probe** — poll the reviewable URL for any non-connection-refused
   response (200s ideal; a 3xx/4xx still means it's listening). Retry with
   backoff up to a timeout (~90s default).
2. **Log match** — if there's no HTTP surface, watch the server log for its
   "listening on…" / "compiled successfully" line.
3. **Settle** — as a last resort, wait a fixed interval and proceed.

## Guardrails

- The setup command is one-shot and must exit 0 before you serve.
- If setup or the build **fails**, that is a finding — report it, don't
  patch the project. Loading is your job; fixing is not.
- Run the recipe verbatim; never invent a port.
