<!-- BEGIN:nextjs-agent-rules -->
# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` before writing any code. Heed deprecation notices.
<!-- END:nextjs-agent-rules -->

# Spacing system

This project uses an **8px base spacing** (`--spacing: 8px` in `app/globals.css`). All Tailwind spacing values are 2× the default 4px base:

- `p-1` = 8px, `p-2` = 16px, `p-3` = 24px, `p-4` = 32px, `p-6` = 48px, `p-8` = 64px
- `w-3 h-3` = 24px, `w-4 h-4` = 32px, `w-6 h-6` = 48px, `w-8 h-8` = 64px

Think in **target pixels ÷ 8**:

- Want 24px padding? Use `p-3` (not `p-6`)
- Want 16px gap? Use `gap-2` (not `gap-4`)
- Want a 64px header? Use `h-8` (not `h-16`)

# Color system

Strict monochrome — no accent colors. State is communicated via dot fill, weight, indentation, iconography, and motion, never hue.

- `bg-bg` / `text-fg` — pure black background, pure white foreground.
- `text-gray-1` through `text-gray-7` — the grayscale ramp (near-black to secondary text). See `app/globals.css`.

Do not introduce additional color tokens without updating the plan in `Shelbi/Plans/shelbi-website.md` §3.

## Docs reading experience — sanctioned exceptions

The monochrome rule holds everywhere (landing page, header/footer, kanban
mockup, all UI chrome). The **docs reading surface** carries two narrow,
intentional departures — keep them; do not revert to strict-mono here:

- **Higher-contrast body text.** Docs prose (paragraphs, lists, blockquotes,
  table cells in `components/mdx-components.tsx`) renders at `text-prose`, a
  value nearer `fg` than the gray ramp, tuned for long-form legibility in both
  themes. Headings stay at pure `text-fg` so they still lead the hierarchy.
- **Callouts use hue by type.** The `Callout` component
  (`components/Callout.tsx`) tints by admonition flavor — note/info → blue,
  warning → yellow, danger/error → red, tip → green — with a soft background,
  saturated border, and readable title/body text in both themes.
- **Copy-prompt banner uses a violet accent.** The `CopyPromptBanner`
  component (`components/CopyPromptBanner.tsx`) — the "set this up with an AI
  agent" affordance — carries a purple/violet tint: a soft tinted background,
  a saturated violet border, and a violet sparkle icon, so it stands out as
  the "do this with an agent" call to action. Title/body text stay on the
  monochrome ramp for legibility. Keep the violet here; do not revert it to
  strict-mono.

- **Buttons inside a colored container match its hue.** The `CopyButton`
  `tone` prop (`components/CopyButton.tsx`) lets a copy button sitting inside a
  colored callout/banner draw its border/background/text from the matching hue
  so it reads as "part of" that container — `violet` for the copy-prompt
  banner, `info`/`tip`/`warning`/`danger` for the callout tones. Each tone
  fills a step stronger than the container tint so it still reads as a button.
  The default `neutral` tone stays strict-mono (code-block copy button, etc.).

These are driven by `--color-prose`, the `--color-callout-*` tokens, the
`--color-copy-prompt-*` tokens, and the per-family `--color-*-button-*` tints
in `app/globals.css` (defined per theme). This hue is scoped to docs callouts,
the copy-prompt banner, and buttons inside those colored containers only — do
not spread accent color into the rest of the monochrome brand.

# Typography

- **Sans (everything UI):** Geist Sans, via `geist/font/sans`. Weights in use: 400, 500, 600.
- **Mono (code, CLI, wordmark text mode):** Source Code Pro, via `next/font/google`. Weights in use: 400, 500, 600.

Both fonts are loaded as Next.js font variables in `app/layout.tsx` and exposed as `--font-sans` / `--font-mono` Tailwind tokens.

# Icons

Use [Heroicons](https://heroicons.com) (`@heroicons/react/24/outline`) for UI icons. Size them with the 8px spacing scale — `w-3 h-3` for 24px, `w-4 h-4` for 32px.
