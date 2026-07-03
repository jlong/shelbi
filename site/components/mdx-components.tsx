import type { MDXComponents } from "mdx/types"
import Link from "next/link"
import { AppMockup } from "./KanbanMockup"
import { Callout } from "./Callout"
import { CodeTab, CodeTabs } from "./CodeTabs"
import { CopyPromptBanner } from "./CopyPromptBanner"
import { InstallCommand } from "./InstallCommand"
import { Steps, Step } from "./Steps"

/**
 * Element overrides applied to rendered docs MDX. Body copy (paragraphs,
 * lists, table cells, blockquotes) renders at `text-prose` — a higher-contrast
 * value than the gray ramp, tuned for long-form legibility in both themes (see
 * `AGENTS.md`). Structure is still carried by weight and the gray ramp, not
 * hue; the one sanctioned exception is the `Callout` component, which tints by
 * type. Fenced code blocks are highlighted at build time by rehype-pretty-code
 * (vesper theme), so `pre`/`code` here only carry layout and the surrounding
 * chrome.
 */
export const mdxComponents: MDXComponents = {
  // Shared marketing/docs components addressable from MDX by tag name.
  // `<AppMockup preset="starter" activeView="chat" />` renders a Shelbi TUI
  // scenario import-free; see `KanbanMockup.tsx` for `AppState`/presets.
  CopyPromptBanner,
  InstallCommand,
  AppMockup,
  Steps,
  Step,
  Callout,
  // Synced tabbed code blocks (package-manager / shell switcher). Selection
  // syncs across same-`group` blocks on the page; see `CodeTabs.tsx`.
  CodeTabs,
  CodeTab,
  h1: (props) => (
    <h1 className="mt-6 mb-3 scroll-mt-8 text-3xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h2: (props) => (
    <h2 className="mt-6 mb-2 scroll-mt-8 text-xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h3: (props) => (
    <h3 className="mt-4 mb-2 scroll-mt-8 text-lg font-semibold tracking-tight text-fg" {...props} />
  ),
  p: (props) => <p className="my-2 leading-relaxed text-prose" {...props} />,
  ul: (props) => (
    <ul className="my-2 list-disc space-y-1 pl-3 text-prose" {...props} />
  ),
  ol: (props) => (
    <ol className="my-2 list-decimal space-y-1 pl-3 text-prose" {...props} />
  ),
  li: (props) => <li className="leading-relaxed" {...props} />,
  a: ({ href = "", ...props }) => (
    <Link
      href={href}
      className="text-fg underline underline-offset-4 transition-colors hover:text-gray-7"
      {...props}
    />
  ),
  strong: (props) => <strong className="font-semibold text-fg" {...props} />,
  // Inline code only — rehype-pretty-code emits block code inside <pre>.
  code: (props) => (
    <code
      className="rounded-sm border border-gray-4 bg-gray-1 px-1 py-0.5 font-mono text-sm text-fg [pre_&]:border-0 [pre_&]:bg-transparent [pre_&]:p-0"
      {...props}
    />
  ),
  pre: (props) => (
    <pre
      className="my-3 overflow-x-auto rounded-md border border-gray-4 bg-gray-1 px-3 py-3 font-mono text-sm leading-relaxed [&_code]:font-mono"
      {...props}
    />
  ),
  // Tables — strict-mono ruling. Cells inherit `text-gray-7` from the table
  // wrapper; headers and the leftmost label column bump to `text-fg` so the
  // axis of comparison reads first.
  table: (props) => (
    <div className="my-3 overflow-x-auto rounded-md border border-gray-4">
      <table
        className="w-full border-collapse text-sm text-prose [&_th]:text-fg [&_tbody_td:first-child]:text-fg [&_tbody_td:first-child]:font-medium"
        {...props}
      />
    </div>
  ),
  thead: (props) => (
    <thead className="border-b border-gray-4 bg-gray-1" {...props} />
  ),
  tr: (props) => (
    <tr className="border-b border-gray-4 last:border-0" {...props} />
  ),
  th: (props) => (
    <th
      className="px-2 py-1 text-left text-xs font-semibold tracking-wide uppercase"
      {...props}
    />
  ),
  td: (props) => (
    <td className="px-2 py-1 align-top leading-relaxed" {...props} />
  ),
  hr: (props) => <hr className="my-6 border-gray-4" {...props} />,
  blockquote: (props) => (
    <blockquote
      className="my-3 border-l-2 border-gray-5 pl-3 text-prose italic"
      {...props}
    />
  ),
}
