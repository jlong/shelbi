"use client"

import { useRef } from "react"
import type { ComponentPropsWithoutRef } from "react"
import { CopyButton } from "./CopyButton"

/**
 * Client wrapper the MDX `pre` maps to. Fenced code is highlighted at build
 * time by rehype-pretty-code, so the plain text to copy is only available from
 * the rendered DOM — {@link CopyButton}'s `getText` reads it at click time from
 * the `<pre>` ref. The button sits on the wrapper (not inside the scrolling
 * `<pre>`) so it stays put on wide code, and reveals on hover/focus.
 */
export function Pre(props: ComponentPropsWithoutRef<"pre">) {
  const ref = useRef<HTMLPreElement>(null)

  return (
    <div className="group relative my-3">
      <pre
        ref={ref}
        className="overflow-x-auto rounded-md border border-gray-4 bg-gray-1 px-3 py-3 font-mono text-sm leading-relaxed [&_code]:font-mono"
        {...props}
      />
      <CopyButton
        getText={() => (ref.current?.innerText ?? "").replace(/\n$/, "")}
        ariaLabel="Copy code"
        className="absolute top-2 right-2 flex items-center gap-1 rounded-sm border border-gray-4 bg-gray-2 px-1 py-1 font-mono text-xs text-gray-7 opacity-0 transition group-hover:opacity-100 hover:border-gray-5 hover:text-fg focus:outline-none focus-visible:border-gray-6 focus-visible:opacity-100"
      />
    </div>
  )
}
