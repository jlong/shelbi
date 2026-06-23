# Sketch

> Research note for the `/vs/sketch` page on shelbi.dev. Factual snapshot
> as of the read dates in **Sources**. No advocacy — just what Sketch's
> own surfaces and a couple of independent posts say.

> [!IMPORTANT]
> **Sketch is no longer maintained.** As of **2026-01-08**, its makers
> (Bold Software, which also operates exe.dev) retired Sketch and moved
> its ideas into a successor coding agent named **Shelley**. The
> `sketch.dev` landing page now reads "Goodbye Sketch, Hello Shelley!" and
> the GitHub repo banner states "Sketch has evolved into Shelley. Sketch
> is no longer maintained." This note documents Sketch as the named
> competitor, with the Shelley evolution captured where it bears on the
> comparison.

## Positioning

In its own words, Sketch is "an agentic coding tool. It draws the 🦉" —
a terminal-first AI coding agent with a web UI that, per the project,
"runs in your terminal, has a web UI, understands your code, and helps
you get work done." The README frames it as an "autonomous software
apprentice." Its defining design choice: every agent session runs inside
a fresh Docker container, and the agent's work lands on `sketch/*`
branches in the host git repository rather than mutating the working
tree directly.

In plain prose: Sketch is an open-source (Apache 2.0) coding agent from
Bold Software. You run `sketch` inside a git repo; it builds a Docker
container, copies your code in, and runs a single-agent LLM loop (driven
primarily by Claude) that edits code, runs shell commands, and makes git
commits. Those commits are auto-pushed back to your repo on `sketch/*`
branches. A browser-based chat-and-diff UI opens alongside the terminal,
and because each run is sandboxed in its own container you can fan out
several agents in parallel on different tasks. Go is its first-class
language, though it works with most stacks. The hosted variant at
sketch.dev ran the same agent on gVisor-based containers in the cloud.
The whole effort has since been superseded by Shelley.

## Model

- **Architecture style:** Single-agent loop, not multi-agent. The core
  loop is deliberately minimal — receive user input, send it to an LLM,
  execute any tool calls the LLM returns, append results, repeat. The
  Sketch team describes the kernel as roughly "9 lines" of code and
  titled a blog post "The Unreasonable Effectiveness of an LLM Agent Loop
  with Tool Use."
- **Primary tool surface:** `bash` is the agent's main general-purpose
  tool — the LLM drives the codebase almost entirely through shell
  commands. A small number of supplementary tools are layered on,
  notably dedicated text-editing tools (the team notes that "tools that
  let the LLM edit text correctly are surprisingly tricky"), plus browser
  / screenshot tools and image-reading.
- **Where it runs:** Locally on the developer's machine, but the agent
  itself executes inside a Docker container, not on the host. On startup
  Sketch (1) generates a Dockerfile, (2) builds the image, (3) copies the
  repository into it, and (4) starts the container. The host environment
  stays pristine; multiple containers can run concurrently for parallel
  sketches. A hosted/cloud version of Sketch existed at sketch.dev and
  used **gVisor-based containers** for isolation (per the Shelley
  retrospective).
- **Container access:** The running container is reachable via an SSH
  host alias (`ssh sketch-[hostname]`), a Terminal tab in the web UI, VS
  Code remote, and SSH port-forwarding for dev servers
  (e.g. `ssh -L8000:localhost:8888`).
- **Model providers:** Claude (Anthropic) is the documented and primary
  backend. The team used **Claude 3.7 Sonnet** extensively. The only
  credential the README requires to run locally is `ANTHROPIC_API_KEY`.
  No first-class OpenAI / Gemini support is documented for Sketch.
  (Multi-model support is a *Shelley* feature, not a Sketch one — see
  below.)
- **Git as the integration boundary:** The agent is trained to make git
  commits; on each commit Sketch auto-pushes to the origin repo under
  `sketch/*` branch names, so output is consumed through ordinary git
  (merge, cherry-pick, rebase, reset) rather than a bespoke apply step.

### How a session flows (end to end)

1. You run `sketch` from inside a git repository on your machine.
2. Sketch generates a Dockerfile, builds the image, copies the repo into
   it, and boots a container; the web UI opens in the browser (unless
   `-open=false`).
3. You describe a task in the chat (text, or a screenshot plus a short
   description for UI work).
4. The single-agent loop runs: the LLM issues tool calls — mostly `bash`,
   plus edit / browser / screenshot tools — and Sketch feeds results back
   each turn. The agent installs missing dependencies, runs builds and
   tests, reads compiler/test output, and self-corrects.
5. As it reaches checkpoints the agent makes git commits; Sketch pushes
   them to a `sketch/*` branch on your origin and fires a browser
   notification when a turn completes.
6. You review in the diff view — leaving inline PR-style comments to steer
   it, or typing fixes directly into the right-hand side of the diff,
   which get committed and pushed for you. For deeper edits you SSH into
   the container, open it over VS Code remote, or port-forward a dev
   server.
7. You integrate the `sketch/*` branch with ordinary git (merge,
   cherry-pick, rebase). When the container is torn down, its environment
   is gone — only the pushed commits persist.

### Install & requirements

- **Platforms:** macOS, Linux, and WSL2. No native Windows.
- **Hard dependency:** a working Docker runtime — Colima, OrbStack, or
  Docker Desktop on macOS; `docker.io` (or distro equivalent) on Linux;
  Docker Desktop for Windows under WSL2.
- **Install paths:** Homebrew tap
  (`brew install boldsoftware/tap/sketch`), GitHub nightly release
  binaries, or build from source with `make` then `./sketch`.
- **Updates:** `sketch -update` (binary) or
  `brew upgrade boldsoftware/tap/sketch` (Homebrew).
- **Credentials:** `ANTHROPIC_API_KEY` in the environment for local runs.
- **Maintenance:** container images accumulate; users periodically run
  `docker system prune -a` to reclaim space.
- **Community:** a Discord (`discord.gg/6w9qNRUDzS`) is linked from the
  README.

### Notable CLI flags / commands seen in docs

- `sketch` — start the agent in the current git repo (opens web UI).
- `sketch -open=false` — CLI-only, no browser.
- `sketch -update` — self-update the binary.
- `ssh sketch-[hostname]` — SSH into the running container.
- `ssh -L8000:localhost:8888 …` — port-forward a dev server out of the
  container.
- `git branch -a --sort=creatordate | grep sketch/ | tail` — list the
  branches Sketch has pushed.

## Key features

- **Containerized agent sandbox** — each session runs in its own Docker
  container built from a generated Dockerfile; host machine and
  credentials stay isolated from the agent.
- **Terminal CLI + auto-opening web UI** — `sketch` starts a chat
  interface in the browser; `-open=false` keeps it CLI-only.
- **Single-agent bash-centric loop** — the agent operates mostly through
  shell commands, installing dependencies and adapting on its own between
  turns.
- **Automatic git commits + push to `sketch/*` branches** — work is
  delivered as real commits on dedicated branches in the host repo.
- **Editable diff view** — you can type directly into the right-hand side
  of Sketch's diff; edits get folded into the commit and pushed for you.
- **Code-review-style inline comments on diffs** — leave comments on the
  diff to steer the agent, GitHub-PR style.
- **Multiple container access paths** — Web UI Terminal tab, SSH
  (`ssh sketch-[hostname]`), VS Code remote, and port-forwarding to reach
  dev servers running inside the container.
- **Browser + screenshot tools** — the agent can browse pages and capture
  screenshots, useful for verifying UI work.
- **Image/vision input** — you can hand the agent a screenshot plus a
  short description (e.g. "make this UI nicer") and it works from the
  image.
- **Parallel / async agents** — because each run is sandboxed, you can
  open a second Sketch on another task while the first works (the
  canonical example: kicking off a UI tweak from a screenshot while a
  separate agent implements auth).
- **Browser notifications** — a bell icon / browser notification fires
  when the agent finishes a turn.
- **Go-first, broadly compatible** — works with most languages and
  environments, with "extra goodies for Go."
- **Self-update + easy install** — Homebrew tap
  (`brew install boldsoftware/tap/sketch`), GitHub nightly releases, or
  build from source (`make`); updates via `sketch -update` or
  `brew upgrade`.

## Pricing

- **Open source, free to self-host.** Sketch is licensed **Apache 2.0**
  and published at `github.com/boldsoftware/sketch`. Run it locally for
  free; you supply your own `ANTHROPIC_API_KEY`, so the real cost is the
  Anthropic API usage (the team notes a single request can burn "tens of
  thousands of intermediate tokens" and take minutes per turn).
- **Hosted sketch.dev:** A hosted version ran on Bold Software's
  gVisor-based container infrastructure. No public, documented pricing
  table for the hosted Sketch agent was found during this research —
  treat hosted pricing as unconfirmed.
- **No per-seat / per-task tier published** for the agent. (Searches for
  "Sketch pricing" surface the unrelated **Sketch.com** design tool —
  $12–$44/editor/month — which is a different company and product; do not
  conflate the two.)
- **Successor (Shelley / exe.dev):** Also open source
  (`github.com/boldsoftware/shelley`); built for exe.dev's per-user VM
  hosting. No public pricing was found as of the read date.

## Strengths

- **Clean isolation model.** Running the agent in a throwaway container
  means it can't reach the host's production credentials or deploy
  scripts, and a bad run is discarded with the container — a genuine
  safety improvement over agents that operate directly on your machine.
- **Git-native delivery.** Output is plain git commits on `sketch/*`
  branches, so it slots into existing review/merge workflows without a
  proprietary patch format or lock-in.
- **Effective minimal loop.** The bash-first, single-agent design is
  simple and, by the team's account, surprisingly robust at "gluing
  well-known APIs together" and grinding through integration tedium
  (their example: GitHub App auth in ~a day vs. ~a week by hand).
- **Tight feedback loop with real tooling.** The agent sees compiler
  errors, test failures, and existing code, which measurably lifts output
  quality versus a bare LLM, and it can verify UI work via the
  browser/screenshot tools.
- **Pleasant review ergonomics.** Editable diffs plus inline PR-style
  comments make human-in-the-loop correction low-friction.

## Limitations / gaps

- **Discontinued.** No longer maintained as of 2026-01-08; superseded by
  Shelley. Adopting Sketch today means adopting an EOL'd tool.
- **Ephemeral environments by design — the fatal flaw.** Sketch tied a
  conversation's lifetime to its container's lifetime, so the environment
  was wiped after each session. The team's own retrospective: "Imagine
  having IT wipe your laptop clean every day?" This is the central reason
  they rebuilt as Shelley (which persists state in SQLite on a durable
  per-user VM).
- **Single provider.** Practically Claude-only; no documented first-class
  support for OpenAI, Gemini, or local models in Sketch itself.
- **Docker dependency.** Requires a working Docker runtime (Colima /
  OrbStack / Docker Desktop / docker.io); macOS, Linux, and WSL2 only —
  no native Windows, and container build/maintenance overhead
  (`docker system prune -a`) is on the user.
- **Quality still needs a human.** The team openly documents agent output
  containing security vulnerabilities and performance issues, and the
  agent missing project-specific conventions until they were written into
  the schema/docs — human review remains mandatory.
- **No published hosted SLA / pricing.** The cloud sketch.dev offering had
  no public, documented pricing or guarantees found in this research,
  making it hard to scope for team adoption even before the shutdown.
- **Slow, token-heavy turns.** A single request can emit "tens of
  thousands of intermediate tokens" and take several minutes, which the
  team attributes to the bash-heavy loop and current hardware/model
  costs.

## Independent reception

- The "Unreasonable Effectiveness of an LLM Agent Loop with Tool Use" post
  and Sketch's launch drew significant discussion on Hacker News
  (item 44166815), where the minimal bash-driven loop and the
  container-per-session isolation were the most-noted design points.
- Independent coverage consistently frames Sketch as a Go-centric,
  developer-tool-flavored agent (its makers are heavy Go users; the agent
  is itself distributed as a Go binary and `sketch.dev` is a Go module),
  distinguishing it from browser-first or IDE-plugin agents.
- Reception of the Shelley pivot centered on the persistence argument —
  that ephemeral per-session environments were the wrong default for
  real, multi-day development work.

## Where shelbi differs

> Factual axes of difference only — not a value judgment.

- **Maintenance status:** shelbi is an actively developed product; Sketch
  is retired and frozen (its lineage continues as the separate Shelley
  project).
- **Environment persistence:** Sketch's defining trait was ephemeral,
  per-session containers that were wiped each run; the comparison axis is
  whether shelbi persists workspace/agent state across sessions rather
  than discarding it with the sandbox.
- **Model breadth:** Sketch was effectively single-provider (Claude /
  `ANTHROPIC_API_KEY`); the comparison axis is which model providers
  shelbi supports.
- **Isolation mechanism:** Sketch's isolation boundary is a per-session
  Docker (locally) / gVisor (hosted) container with output handed back as
  `sketch/*` git branches; the axis is how shelbi sandboxes agent work
  and how it returns results.
- **Distribution / licensing:** Sketch shipped as an Apache-2.0
  open-source CLI you self-host with your own API key; the axis is
  shelbi's licensing and hosting model (self-host vs. managed) and how
  cost is structured.

## Appendix — successor (Shelley)

For context, since Sketch's surfaces now point here. Shelley is Bold
Software's replacement coding agent, announced 2026-01-08 on
blog.exe.dev and open-sourced at `github.com/boldsoftware/shelley`. It is
described as "mobile-friendly, web-based, multi-conversation,
multi-modal, multi-model, single-user." Key deltas from Sketch:

- **Persistent per-user VM** instead of throwaway containers — fixes
  Sketch's "wiped every session" problem.
- **SQLite-backed state** — past conversations are reviewable like shell
  history.
- **Multi-conversation** (several parallel threads), **multi-modal**
  (screenshots/visuals), and **multi-model** (more than one LLM backend)
  — the latter two notably broader than Sketch.
- **Single-user**, runs on your own VM rather than shared infrastructure.
- Retains Sketch's web-proxy feature for immediately testing spun-up dev
  servers.
- Built for, but not exclusive to, exe.dev.

## Sources

- https://sketch.dev/ — "Goodbye Sketch, Hello Shelley!" landing/redirect notice (read 2026-06-23)
- https://github.com/boldsoftware/sketch — Sketch README: overview, install, container model, git integration, tools, Apache-2.0 license, "no longer maintained" banner (read 2026-06-23)
- https://sketch.dev/blog/agent-loop — "The Unreasonable Effectiveness of an LLM Agent Loop with Tool Use": single-agent loop, bash-first tools, Claude 3.7 Sonnet (read 2026-06-23)
- https://sketch.dev/blog/programming-with-agents — "How I program with Agents": parallel agents, container isolation, editable diffs, inline review comments, strengths/limitations (read 2026-06-23)
- https://blog.exe.dev/shelley — "Goodbye Sketch, Hello Shelley!" retrospective: Sketch's ephemeral-container flaw, gVisor hosting, Shelley's persistent-VM / SQLite / multi-model design, dated 2026-01-08 (read 2026-06-23)
- https://github.com/boldsoftware/shelley — Shelley repo: successor coding agent, open source (read 2026-06-23)
- https://news.ycombinator.com/item?id=44166815 — "Sketch, an Agentic Coding Assistant" HN discussion: independent reception of the minimal-loop / container-per-session design (surfaced via search index, 2026-06-23)
- https://pkg.go.dev/sketch.dev — `sketch.dev` Go module listing, corroborating the Go-binary distribution (read 2026-06-23)