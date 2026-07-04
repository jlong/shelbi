"use client"

import { useState } from "react"
import { CheckIcon, ClipboardIcon } from "@heroicons/react/24/outline"

/**
 * Color variant. `neutral` is the strict-mono default (code-block hover copy,
 * standalone use). The rest match a colored callout/banner container so the
 * button reads as "part of" it — `violet` for the `CopyPromptBanner`, and
 * `info`/`tip`/`warning`/`danger` for the four `Callout` tones. Each non-neutral
 * tone draws its idle/hover/focus chrome from the matching
 * `--color-*-button-*` token family in `app/globals.css`.
 */
type CopyButtonTone =
  | "neutral"
  | "violet"
  | "info"
  | "tip"
  | "warning"
  | "danger"

type CopyButtonProps = {
  /** Static text copied on click. Ignored when {@link getText} is provided. */
  text?: string
  /**
   * Lazily resolves the text to copy at click time — used when the source is
   * rendered markup (e.g. rehype-pretty-code output) whose plain text is only
   * available from the DOM. Takes precedence over {@link text}.
   */
  getText?: () => string
  /** Idle button label. Defaults to `copy`. */
  label?: string
  /** Label shown briefly after a successful copy. Defaults to `copied`. */
  copiedLabel?: string
  /** Overrides the accessible name. Falls back to the install-command wording. */
  ariaLabel?: string
  /** Color variant. Defaults to `neutral` (strict mono). */
  tone?: CopyButtonTone
  /** Overrides the layout/positioning classes (e.g. to render inline vs. overlaid). */
  className?: string
}

/** Shared structure — layout-agnostic; the `border` here is colored per {@link TONE_CHROME}. */
const BASE_CLASSNAME =
  "flex items-center gap-1 rounded-sm border py-1 font-mono text-xs transition-colors focus:outline-none"

/** Default positioning: overlaid in the top-right of a code block. */
const DEFAULT_LAYOUT = "absolute top-2 right-2 px-1"

/**
 * Per-tone border/background/text chrome (idle + hover + focus-visible).
 * `neutral` keeps the original gray chrome unchanged; the colored tones resolve
 * through their matching `--color-*-button-*` token family so light and dark
 * mode follow automatically.
 */
const TONE_CHROME: Record<CopyButtonTone, string> = {
  neutral:
    "border-gray-4 bg-gray-2 text-gray-7 hover:border-gray-5 hover:text-fg focus-visible:border-gray-6",
  violet:
    "border-copy-prompt-button-border bg-copy-prompt-button-bg text-copy-prompt-button-text hover:border-copy-prompt-button-hover-border hover:bg-copy-prompt-button-hover-bg focus-visible:border-copy-prompt-button-hover-border",
  info: "border-callout-info-button-border bg-callout-info-button-bg text-callout-info-button-text hover:border-callout-info-button-hover-border hover:bg-callout-info-button-hover-bg focus-visible:border-callout-info-button-hover-border",
  tip: "border-callout-tip-button-border bg-callout-tip-button-bg text-callout-tip-button-text hover:border-callout-tip-button-hover-border hover:bg-callout-tip-button-hover-bg focus-visible:border-callout-tip-button-hover-border",
  warning:
    "border-callout-warning-button-border bg-callout-warning-button-bg text-callout-warning-button-text hover:border-callout-warning-button-hover-border hover:bg-callout-warning-button-hover-bg focus-visible:border-callout-warning-button-hover-border",
  danger:
    "border-callout-danger-button-border bg-callout-danger-button-bg text-callout-danger-button-text hover:border-callout-danger-button-hover-border hover:bg-callout-danger-button-hover-bg focus-visible:border-callout-danger-button-hover-border",
}

export function CopyButton({
  text,
  getText,
  label = "copy",
  copiedLabel = "copied",
  ariaLabel,
  tone = "neutral",
  className = DEFAULT_LAYOUT,
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false)

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(getText ? getText() : (text ?? ""))
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
      className={`${BASE_CLASSNAME} ${className} ${TONE_CHROME[tone]}`}
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
