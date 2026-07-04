import { SparklesIcon } from "@heroicons/react/24/outline"
import { CopyButton } from "./CopyButton"

type CopyPromptBannerProps = {
  /** Raw prompt text copied to the clipboard when the button is clicked. */
  prompt: string
  /** Optional heading override. */
  title?: string
  /** Optional supporting-copy override. */
  description?: string
}

const DEFAULT_TITLE = "Set this up with an AI agent"
const DEFAULT_DESCRIPTION =
  "Copy a prompt describing this guide and paste it into Claude Code (or your agent of choice)."

/**
 * Docs banner that hands the reader a ready-made LLM prompt for the current
 * guide. Mirrors Clerk's quickstart affordance: one click copies the raw
 * prompt so it can be pasted into an AI coding tool. Renders at the top of a
 * doc page; the clipboard state is owned by the reused {@link CopyButton}.
 *
 * Deliberate exception to the strict-monochrome rule in `site/AGENTS.md`
 * (same spirit as the colored callouts): the banner carries a violet accent
 * — tinted background, saturated border, violet sparkle icon, and a matching
 * violet Copy-prompt button (`tone="violet"`) — so the "set this up with an
 * AI agent" affordance stands out. Title/body text stay on the monochrome
 * ramp for legibility. Colors resolve through the `--color-copy-prompt-*`
 * tokens in `app/globals.css`, so light and dark mode follow automatically.
 */
export function CopyPromptBanner({
  prompt,
  title = DEFAULT_TITLE,
  description = DEFAULT_DESCRIPTION,
}: CopyPromptBannerProps) {
  return (
    <div className="my-3 flex flex-col gap-2 rounded-md border border-copy-prompt-border bg-copy-prompt-bg p-3 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex items-start gap-2">
        <SparklesIcon
          className="mt-0.5 h-3 w-3 shrink-0 text-copy-prompt-icon"
          aria-hidden="true"
        />
        <div className="space-y-1">
          <p className="font-medium text-fg">{title}</p>
          <p className="text-sm leading-relaxed text-gray-7">{description}</p>
        </div>
      </div>
      <CopyButton
        text={prompt}
        label="Copy prompt"
        copiedLabel="Copied"
        ariaLabel="Copy prompt"
        tone="violet"
        className="shrink-0 self-start px-2 sm:self-auto"
      />
    </div>
  )
}
