---
name: load-run-detection
description: >
  How the Review agent figures out how to install and serve an unknown
  project — the auto-detect heuristics and their precedence. Use when a
  review workspace has a branch checked out and you need to boot its dev
  server on $PORT so a human can run it.
---

# Load / run auto-detection

You are on a review workspace and need to make the checked-out branch
**runnable** on your assigned `$PORT`. This skill is the decision
procedure for *how*. It is heuristics, not Rust — adapt per repo.

## Precedence (first match wins)

1. **Declared commands** — `review.setup` / `review.serve` in the project
   config. If present, use them **verbatim** and skip everything below.
   Declared always beats detection.
2. **Framework heuristic** — inferred from manifest + lockfile (below).
3. **Generic runner** — `Makefile` `dev`/`serve` target, then `Procfile`
   `web:` line.
4. **Diff-only** — nothing runnable detected. Report it; do not fabricate
   a server.

Always bind to `$PORT`. Never hardcode a port — concurrent review
workspaces share a host and will collide.

## Node / JavaScript — `package.json` present

Pick the package manager from the lockfile, not from habit:

| Lockfile            | Install            | Run dev            |
| ------------------- | ------------------ | ------------------ |
| `pnpm-lock.yaml`    | `pnpm install`     | `pnpm dev`         |
| `yarn.lock`         | `yarn install`     | `yarn dev`         |
| `package-lock.json` | `npm install`      | `npm run dev`      |
| none                | `npm install`      | `npm run dev`      |

Read `package.json` `scripts` to confirm the serve script — it is usually
`dev`, sometimes `start` or `serve`. Prefer `dev` for a dev server.

**Passing the port.** Most JS dev servers accept `--port`:

- Next.js: `next dev --port $PORT` → `npm run dev -- --port $PORT`
- Vite: `vite --port $PORT` → `npm run dev -- --port $PORT`
- With pnpm/yarn the `--` is not needed: `pnpm dev --port $PORT`.

Some frameworks read `PORT` from the environment instead (Create React
App, many Express apps). If `--port` is not recognized, export
`PORT=$PORT` before the serve command. When in doubt, set both — export
`PORT` **and** pass `--port $PORT`.

## Rust — `Cargo.toml` present

1. `cargo build` (setup — must exit 0).
2. Serve: `cargo run` for a single binary; `cargo run --bin <name>` or
   `cargo run --example <name>` when there are several. Inspect
   `Cargo.toml` `[[bin]]` / `[[example]]` and `src/bin/` to choose.
3. Port: most Rust web servers read `PORT` from the environment — export
   `PORT=$PORT`. If the crate takes a `--port` flag, pass it after `--`:
   `cargo run -- --port $PORT`.

## Generic runners

- **Makefile** with a `dev` or `serve` target → `make dev` / `make serve`.
  Grep the Makefile for the target before assuming it exists.
- **Procfile** with a `web:` process line → run that command, substituting
  `$PORT` (Procfile-style apps already expect `$PORT` in the environment).

## Other ecosystems (quick reference)

- `pyproject.toml` / `requirements.txt` → create/activate a venv, install,
  then the project's documented run command (`uvicorn app:app --port $PORT`,
  `flask run --port $PORT`, `python manage.py runserver 0.0.0.0:$PORT`).
- `go.mod` → `go run .`, port via `PORT` env or a documented flag.
- `Gemfile` → `bundle install`, then `bundle exec rails server -p $PORT`
  or the Procfile `web:` line.

If you recognize the ecosystem but not the exact serve command, read the
project's README for the canonical "how to run locally" instructions
before guessing.

## Readiness

After starting the server, confirm it is actually serving before you
declare ready:

1. **HTTP probe** — poll `http://localhost:$PORT` (or the declared
   `review.ready_probe.http`) for any non-connection-refused response
   (200s ideal; a 3xx/4xx still means it's listening). Retry with backoff
   up to a timeout (~90s default).
2. **Log match** — if there's no HTTP surface, watch the server log for
   its "listening on…" / "compiled successfully" line.
3. **Settle** — as a last resort, wait a fixed interval and proceed.

## When you can't run it

If none of the above resolves — no manifest, no Makefile/Procfile, no
documented run command — it's a **diff-only** review. Do not fabricate a
server. Report clearly in your ready summary what you looked for and why
nothing booted, emit the ready signal without a `url`, and stop.

## Guardrails

- Setup commands are one-shot and must exit 0 before you serve.
- If setup or the build **fails**, that is a finding — report it, don't
  patch the project. Loading is your job; fixing is not.
- Bind to `$PORT`, always.
