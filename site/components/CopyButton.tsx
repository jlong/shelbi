"use client"

import { useState } from "react"
import { CheckIcon, ClipboardIcon } from "@heroicons/react/24/outline"

export function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard API unavailable — silently no-op.
    }
  }

  return (
    <button
      type="button"
      onClick={handleCopy}
      aria-label={copied ? "Copied" : "Copy install command"}
      className="absolute top-2 right-2 flex items-center gap-1 rounded-sm border border-gray-4 bg-gray-2 px-1 py-1 font-mono text-xs text-gray-7 transition-colors hover:border-gray-5 hover:text-fg focus:outline-none focus-visible:border-gray-6"
    >
      {copied ? (
        <>
          <CheckIcon className="h-2 w-2" />
          <span>copied</span>
        </>
      ) : (
        <>
          <ClipboardIcon className="h-2 w-2" />
          <span>copy</span>
        </>
      )}
    </button>
  )
}
