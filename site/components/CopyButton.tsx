"use client"

import { useState } from "react"
import { CheckIcon, ClipboardIcon } from "@heroicons/react/24/outline"

type CopyButtonProps = {
  text: string
  /** Idle button label. Defaults to `copy`. */
  label?: string
  /** Label shown briefly after a successful copy. Defaults to `copied`. */
  copiedLabel?: string
  /** Overrides the accessible name. Falls back to the install-command wording. */
  ariaLabel?: string
  /** Overrides the layout/chrome classes (e.g. to render inline vs. overlaid). */
  className?: string
}

const DEFAULT_CLASSNAME =
  "absolute top-2 right-2 flex items-center gap-1 rounded-sm border border-gray-4 bg-gray-2 px-1 py-1 font-mono text-xs text-gray-7 transition-colors hover:border-gray-5 hover:text-fg focus:outline-none focus-visible:border-gray-6"

export function CopyButton({
  text,
  label = "copy",
  copiedLabel = "copied",
  ariaLabel,
  className = DEFAULT_CLASSNAME,
}: CopyButtonProps) {
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
      aria-label={ariaLabel ?? (copied ? "Copied" : "Copy install command")}
      className={className}
    >
      {copied ? (
        <>
          <CheckIcon className="h-2 w-2" />
          <span>{copiedLabel}</span>
        </>
      ) : (
        <>
          <ClipboardIcon className="h-2 w-2" />
          <span>{label}</span>
        </>
      )}
    </button>
  )
}
