# Shelbi Website

## Context

Shelbi has no public web presence today. Discovery and onboarding live entirely in `README.md` and the `wizard` CLI flow. There's a recognizable visual identity in flight — the block-character banner that the wizard prints on first run — but no logo system, palette, or typography decisions outside the terminal.

The need is twofold:

- **Marketing surface.** A landing page that explains what Shelbi is, who it's for, and gives a clear path to "install + try your first task." Same role the `contextstore-website` plays for ContextStore.
- **Documentation surface.** Getting-started walkthrough, mental-model docs (workers / columns / events log / orchestrator), and a CLI reference. The README is the current single source of truth and is already straining under that load — getting-started, concepts, and reference all rolled into one scroll.

These two surfaces want to share styling, navigation, and deploy pipeline. One Next.js app, two content modes (marketing pages + MDX docs).

The 32pixels Next.js sites give us a tooling template: `~/Workspaces/32pixels/contextstore-website` (Next 16, marketing-only) and `~/Workspaces/32pixels/website` (Next 15 + contentlayer + MDX blog). We borrow the file conventions, dependency choices, and 8px spacing system — but the visual identity is Shelbi's own (see §3).

## Design

### 1. Repository layout

New `site/` subdirectory at the repo root, alongside `crates/`:

```
shelbi/
├── crates/                # existing Rust workspace
├── scripts/install.sh
├── site/                  # NEW — Next.js app
│   ├── app/
│   │   ├── (marketing)/   # route group for landing + future marketing pages
│   │   │   ├── page.tsx
│   │   │   └── layout.tsx
│   │   ├── docs/
│   │   │   └── [[...slug]]/
│   │   │       ├── page.tsx           # catch-all renders MDX
│   │   │       └── opengraph-image.tsx  # @vercel/og — per-page card
│   │   ├── opengraph-image.tsx        # @vercel/og — homepage card
│   │   ├── layout.tsx
│   │   ├── globals.css
│   │   └── favicon.ico
│   ├── components/
│   ├── content/
│   │   └── docs/                      # MDX source
│   │       ├── getting-started/
│   │       │   ├── install.mdx
│   │       │   ├── first-project.mdx
│   │       │   └── first-task.mdx
│   │       ├── concepts/
│   │       │   ├── workers.mdx
│   │       │   ├── columns.mdx
│   │       │   ├── events-log.mdx
│   │       │   └── orchestrator.mdx
│   │       └── cli/
│   │           ├── task.mdx
│   │           ├── worker.mdx
│   │           ├── review.mdx
│   │           ├── events.mdx
│   │           ├── merge.mdx
│   │           └── reload.mdx
│   ├── styles/
│   ├── lib/
│   ├── public/
│   ├── contentlayer.config.ts
│   ├── next.config.ts
│   ├── package.json
│   ├── tsconfig.json
│   ├── AGENTS.md          # mirrors 32pixels conventions for agents working in site/
│   └── README.md
├── Cargo.toml
└── README.md              # trimmed to "what + link to docs"
```

Trade-off: the Rust repo gains a Node toolchain. We isolate it to `site/`; CI for `crates/` ignores `site/**` and vice versa via path filters. Cargo workspace is unaffected.

### 2. Stack

Pinned to the contextstore-website lineage:

- **Next 16** with the App Router and Turbopack
- **React 19**
- **Tailwind v4** via `@tailwindcss/postcss`, with the 8px base spacing override (`--spacing: 8px` in `globals.css`)
- **contentlayer2** + `@next/mdx` for typed MDX docs (lifted directly from the 32pixels blog setup)
- **rehype-pretty-code** + Shiki for code blocks
- **Heroicons** (`@heroicons/react/24/outline`)
- **motion** for animations
- **clsx** + **tailwind-merge**
- **@vercel/analytics**
- **@vercel/og** for programmatic OG cards
- **asciinema-player** for the hero cast embed

Critical reminder mirrored into `site/AGENTS.md`: this is Next 16. Read `node_modules/next/dist/docs/` before writing route handlers, server actions, or layouts — APIs and conventions differ from training data.

### 3. Visual identity

**Strict monochrome.** No accent colors anywhere — the site lives on pure black, pure white, and a deliberate grayscale ramp. Modeled after the Vercel homepage but committing further: where Vercel uses subtle blue for links and CTAs, shelbi uses pure white and weight/treatment changes. Identity work goes into typography, the block-character wordmark, and motion — not into color.

**Palette (Tailwind v4 design tokens, defined in `globals.css`):**

```css
--color-bg:       #000000;   /* pure black                */
--color-fg:       #FFFFFF;   /* pure white                */
--color-gray-1:   #0A0A0A;   /* near-black                */
--color-gray-2:   #141414;   /* surface                   */
--color-gray-3:   #1F1F1F;   /* raised surface            */
--color-gray-4:   #2B2B2B;   /* border / divider          */
--color-gray-5:   #444444;   /* muted text                */
--color-gray-6:   #666666;   /* dim text                  */
--color-gray-7:   #999999;   /* secondary text            */
```

**Communicating state without color.** Where the product surface would normally lean on color (kanban columns, worker states, success/error glyphs), use:

- **Dot fill.** Filled / hollow / dashed circles for column states.
- **Weight.** Bolder labels for active items; semibold-vs-regular for hierarchy.
- **Indentation and density.** Active rows pull left or gain a leading marker.
- **Iconography.** Heroicons paired with text labels; never icon-only.
- **Animation.** Pulsing border or scanning underline for "live" items rather than a colored badge.

This constraint sharpens the rest of the design system — every decision has to carry its weight in form, not hue.

**Typography: Geist Sans + Geist Mono.**

- **Display + body:** Geist Sans for everything sans — headings, hero, body, nav, UI chrome. Loaded via the `geist` npm package (Next.js font subset, self-hosted, no external request).
- **Code + monospace UI:** Geist Mono for all code blocks, inline code, CLI examples, file paths, and the SHELBI wordmark's typographic mode (when it's text, not the block-character SVG).
- **Weights:** 400, 500, 600 for sans; 400 and 500 for mono. Drop the rest to keep page weight down.
- **Pairing rationale:** Vercel designed Geist Sans and Geist Mono together — vertical metrics, x-height, and letter widths harmonize without manual nudging. Both are SIL OFL licensed (free for OSS use). The 32pixels parent website already ships Geist, keeping the family ecosystem consistent.

Why not the alternatives:

- **Inter + JetBrains Mono.** Both free, both well-trodden. Inter is too neutral — reads as generic SaaS. Loses the dev-tool register the rest of the design commits to.
- **Söhne + Berkeley Mono.** The most distinctive option. Both commercial licenses ($200+ for web). Premium feel, but expensive for an OSS project and license-fragile when contributors fork. The block-character wordmark gives shelbi its distinctiveness; the body type doesn't need to.

Geist lets the wordmark be the identity carrier without the typography fighting for attention.

**Logo.** Lift the block-character "SHELBI" banner into an SVG wordmark; render in `--color-fg` against `--color-bg`. Reuse the same block geometry as section dividers, hero callouts, and the favicon. Mono palette makes the block motif the primary identity carrier — no color crutch.

**Code blocks.** Shiki `vesper` theme for Phase 1 — mostly-mono, pairs naturally with the strict palette and Geist Mono. Swap if it doesn't read in practice.

The 8px spacing system carries over from the 32pixels parent — independent of branding, and matches the broader system ergonomics.

### 4. Marketing landing (`app/(marketing)/page.tsx`)

Sections, top to bottom:

1. **Hero** — wordmark + tagline + primary CTA ("Install now" → `/docs/getting-started/install`) + secondary CTA ("Read the docs"). Background uses the block-character motif as a subtle pattern. Short value statement: one sentence on what shelbi is, one on who it's for. No email field.
2. **The pitch in 30 seconds** — three-up grid: *Plan tasks*, *Workers do them*, *You review and merge*. Each cell is an icon + ~30 words + a deep link into the docs.
3. **Live preview (asciinema)** — embedded asciinema cast of the actual TUI showing the multi-worker scenario (full storyboard below). Player loaded async on viewport entry. Re-record when the TUI surface changes materially. OG cards are not derived from the cast — they're rendered separately by `@vercel/og` (see §6).
4. **Feature grid** — six cells covering Kanban board, multi-machine workers, orchestrator auto-dispatch, review flow, events log, tmux-native. Style: small icon top-left, label, single-sentence elaboration.
5. **Install CTA (closer)** — second install pitch near the bottom: code block of the install command + link to "Build from source" in docs.
6. **Footer** — repo link, license, copyright, links to docs / changelog.

No newsletter signup. CTAs are direct: install or read.

**Asciinema specifics:**

- **Scenario:** multi-worker showcase. Three workers (alpha/bravo/charlie) declared and idle. Three small tasks added in quick succession, all promoted to todo. The orchestrator dispatches each to a different worker; cards animate across columns in parallel; review markers appear in sequence at the end.
- **Storyboard timing (target ~45s):**
  - 0:00 — TUI open, three workers idle in sidebar.
  - 0:05 — `shelbi task add` three times: small labeled tasks (e.g., "Update README", "Bump dependency", "Add license header").
  - 0:10 — promote all three to todo via the kanban view.
  - 0:12 — auto-dispatch lines fire; alpha picks A, bravo picks B, charlie picks C.
  - 0:25 — cards visibly move through `in_progress` in parallel as each worker reports.
  - 0:40 — review markers land; all three tasks land in `review` column.
  - 0:45 — pause on the final board state.
- **Authenticity caveat:** real workers can't realistically complete tasks in ~15s. Two options to make the cast work: (a) pre-script a rehearsed run with shorter "dummy" tasks that genuinely complete fast, (b) edit the cast minimally to trim long pauses. Prefer (a) — the cast must remain an honest recording.
- **Player:** `asciinema-player` via npm; loaded only when the section enters the viewport.
- **Theme:** matches the strict-mono palette via the player's CSS variable hooks.

### 5. Docs surface (`app/docs/[[...slug]]/page.tsx`)

Layout:

- Left sidebar (sticky, scrollable, collapsed nav on mobile) — sections are *Getting Started* / *Concepts* / *CLI Reference* / *Changelog*, with the current page highlighted.
- Main content (max-width ~720px, generous line-height) — MDX rendered through contentlayer.
- Right "On this page" rail — auto-generated from H2/H3 headings, sticky on lg+ viewports.
- Footer per page — prev/next links, "Edit this page on GitHub" link.

Content scoping for v1:

**Getting Started**
- `install.mdx` — prereqs (Rust, tmux, claude CLI), `scripts/install.sh`, verifying with `shelbi --version`.
- `first-project.mdx` — `shelbi wizard`, project YAML, declaring workers / machines.
- `first-task.mdx` — opening the TUI, adding a task, promoting to todo, watching auto-dispatch, accepting the review.

**Concepts**
- `workers.mdx` — what a worker is (machine + worktree + runner), states (idle/working/awaiting_input/awaiting_review), the pool model.
- `columns.mdx` — the five Kanban columns, what each transition means, where reviews fit.
- `events-log.mdx` — `~/.shelbi/events.log` shape, both line kinds (`worker=…` and `task=…`), `shelbi events tail`.
- `orchestrator.mdx` — the orchestrator's role (you-are-the-scheduler), how the prompt is wired, how to customize.

**CLI Reference**
- One page per top-level subcommand (`shelbi task`, `shelbi worker`, `shelbi review`, `shelbi events`, `shelbi merge`, `shelbi reload`). Each page is hand-authored MDX for v1; structure mirrors `--help` output. Auto-generation deferred until the surface stabilizes.

**Changelog**
- Single `changelog.mdx` page at `/docs/changelog`. Hand-maintained: when cutting a release, prepend a new entry. Format: `### v0.x.0 — YYYY-MM-DD` header, prose summary, code samples where useful. Workflow tradeoff is one new ritual at release time in exchange for narrative control.

Each MDX file ships with frontmatter:

```yaml
---
title: First task
order: 3        # sort key within the section
summary: One-paragraph description used in nav previews and OG cards.
---
```

`contentlayer.config.ts` defines a `Doc` document type with these fields plus computed `url` and `section`.

### 6. Hosting & deployment (Vercel)

Project linkage: new Vercel project `shelbi-website` pointing at `jlong/shelbi`, with **Root Directory** set to `site/`. Vercel handles framework detection; production deploys on push to `main`, preview deploys for PRs.

CI workflow concerns:
- Path filter the Rust CI workflow (`crates/**`, `Cargo.toml`, `scripts/**`) so site-only PRs don't trigger Rust builds.
- Vercel's own build only runs when files under `site/**` change (its built-in "Ignored Build Step" — script returns 0 to skip).

Domain: `shelbi.dev`, registered ahead of Phase 1. Vercel-generated URL as the staging fallback. `.dev` TLD is on the HSTS preload list — HTTPS is mandatory and free.

Analytics: `@vercel/analytics` mounted in the root layout.

**OG cards (per-page):** programmatic via `@vercel/og`. One reusable `OgCard` component renders at build/edge time, parameterized by page title and section. Strict-mono palette, Geist Sans for the title, block-character "SHELBI" wordmark on the left, page title and `shelbi.dev` on the right.

```tsx
// app/opengraph-image.tsx (homepage)
// app/docs/[...slug]/opengraph-image.tsx (per docs page)
export default async function OG({ params }) {
  // pull title from MDX frontmatter, render OgCard
}
```

Every URL — homepage, docs index, every docs page — gets its own card with the page's own title. No static `og.png` fallback to maintain.

### 7. Migrating README content

The README today is doing four jobs at once. Once the site ships:

- **Install + getting started** → moves to `docs/getting-started/`.
- **Concepts** → moves to `docs/concepts/`.
- **CLI surface** → moves to `docs/cli/`.
- **README** keeps a 5–10 line elevator pitch + links to the site + repo metadata (license, contribution guide).

Done as a single PR after the site ships so docs aren't in two places at once.

## Rollout

Three phases, each independently shippable:

**Phase 1 — Skeleton & landing**
- Register `shelbi.dev` and point at Vercel.
- Scaffold `site/` borrowing file structure and dependency conventions from contextstore-website (no visual treatment — shelbi's identity is its own, per §3).
- Wire Tailwind v4 with 8px spacing, Heroicons, motion, Geist Sans + Geist Mono.
- Build the marketing landing page in its final layout (hero, three-up, asciinema, feature grid, install closer, footer).
- Record and embed the asciinema cast.
- Stub docs route with a "Coming soon" placeholder.
- Link the Vercel project, ship on `shelbi.dev`.

**Phase 2 — Docs surface**
- contentlayer2 + MDX wiring.
- Sidebar + on-this-page rail, with the Changelog slot reserved.
- Migrate getting-started content from the README.
- Concepts pages (workers / columns / events log / orchestrator).
- CLI reference pages (hand-authored).
- Changelog page (`/docs/changelog`, hand-maintained MDX) with an initial entry.

**Phase 3 — Polish**
- Trim the README to elevator-pitch + links.
- Re-record asciinema cast against the final TUI surface if it changed.
- Iterate on the marketing copy after the docs land.
- Revisit Shiki theme if `vesper` doesn't read in practice.

## Decisions

- **Domain:** `shelbi.dev` — confirmed available via RDAP and DNS, registered ahead of Phase 1. The `.dev` TLD is on the HSTS preload list, so HTTPS is mandatory and free.
- **Hosting:** Vercel, with **Root Directory** set to `site/`.
- **Repo layout:** `site/` subdirectory in `jlong/shelbi`, alongside `crates/`. Single repo; CI path-filtered both directions.
- **Palette:** strict monochrome — pure black background (`#000000`), pure white foreground (`#FFFFFF`), 7-step gray ramp. No accent color. State communicated via dot fill, weight, indentation, iconography, and motion. Full spec in §3.
- **Typography:** Geist Sans + Geist Mono. Both SIL OFL, loaded via the `geist` npm package. Sans weights 400/500/600, mono 400/500.
- **Logo:** block-character "SHELBI" wordmark as SVG, in `--color-fg` against `--color-bg`. Reused at favicon, hero, and section-divider scales.
- **Hero treatment:** asciinema cast of the actual TUI (no static screenshot, no animated SVG mockup). Player loaded async on viewport entry; theme matched to the strict-mono palette.
- **Asciinema scenario:** multi-worker showcase — 3 workers, 3 small fast-completing tasks dispatched in parallel, ~45s. Honest recording with rehearsed tasks rather than post-edited pauses.
- **Marketing CTAs:** "Install now" (primary, links to `/docs/getting-started/install`) and "Read the docs" (secondary). No email signup, no newsletter form.
- **Docs sections (v1):** Getting Started, Concepts, CLI Reference, Changelog — in that sidebar order.
- **CLI reference:** hand-authored MDX for v1. Auto-generation deferred until the `--help` surface stabilizes.
- **Changelog:** single page at `/docs/changelog`, hand-maintained MDX. Prepend a new entry at each release; format `### v0.x.0 — YYYY-MM-DD` + prose.
- **Shiki theme:** `vesper` for Phase 1; swap if it doesn't read against the strict-mono palette.
- **OG cards:** `@vercel/og` programmatic. One reusable component, per-page titles, strict-mono + Geist on every URL.

## Open questions

_None remaining — plan is ready to execute. Phase 1 sub-tasks (asciinema recording specifics, exact install command, OG component design, tagline copy) will surface during implementation but don't need pre-commitment._
