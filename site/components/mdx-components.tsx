import type { MDXComponents } from "mdx/types"
import Link from "next/link"

/**
 * Element overrides applied to rendered docs MDX. Strict-monochrome per
 * `site/AGENTS.md`: state via weight and the gray ramp, never hue. Fenced code
 * blocks are highlighted at build time by rehype-pretty-code (vesper theme), so
 * `pre`/`code` here only carry layout and the surrounding chrome.
 */
export const mdxComponents: MDXComponents = {
  h1: (props) => (
    <h1 className="mt-6 mb-3 text-3xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h2: (props) => (
    <h2 className="mt-6 mb-2 text-xl font-semibold tracking-tight text-fg" {...props} />
  ),
  h3: (props) => (
    <h3 className="mt-4 mb-2 text-lg font-semibold tracking-tight text-fg" {...props} />
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
}
