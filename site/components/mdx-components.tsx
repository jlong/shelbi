import type { MDXComponents } from "mdx/types"
import Link from "next/link"
import { AppMockup } from "./KanbanMockup"
import { InstallCommand } from "./InstallCommand"

/**
 * Element overrides applied to rendered docs MDX. Strict-monochrome per
 * `site/AGENTS.md`: state via weight and the gray ramp, never hue. Fenced code
 * blocks are highlighted at build time by rehype-pretty-code (vesper theme), so
 * `pre`/`code` here only carry layout and the surrounding chrome.
 */
export const mdxComponents: MDXComponents = {
  // Shared marketing/docs components addressable from MDX by tag name.
  // `<AppMockup preset="starter" activeView="chat" />` renders a Shelbi TUI
  // scenario import-free; see `KanbanMockup.tsx` for `AppState`/presets.
  InstallCommand,
  AppMockup,
  h1: (props) => (
    <h1 className="mt-6 mb-3 scroll-mt-8 text-3xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h2: (props) => (
    <h2 className="mt-6 mb-2 scroll-mt-8 text-xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h3: (props) => (
    <h3 className="mt-4 mb-2 scroll-mt-8 text-lg font-semibold tracking-tight text-fg" {...props} />
  ),
  p: (props) => <p className="my-2 leading-relaxed text-gray-7" {...props} />,
  ul: (props) => (
    <ul className="my-2 list-disc space-y-1 pl-3 text-gray-7" {...props} />
  ),
  ol: (props) => (
    <ol className="my-2 list-decimal space-y-1 pl-3 text-gray-7" {...props} />
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
        className="w-full border-collapse text-sm text-gray-7 [&_th]:text-fg [&_tbody_td:first-child]:text-fg [&_tbody_td:first-child]:font-medium"
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
      className="my-3 border-l-2 border-gray-5 pl-3 text-gray-7 italic"
      {...props}
    />
  ),
}
