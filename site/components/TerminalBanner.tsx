import { BANNER_LINES } from "./Wordmark"

/**
 * The block-character SHELBI banner as the CLI prints it on first run, framed
 * as terminal output. The half-block glyphs (`▀`/`▄`) only read as letters when
 * the rows tile with no vertical gap, so the banner `<pre>` sets `line-height: 1`
 * and `white-space: pre` — a scoped override of the docs' `leading-relaxed`
 * code-block default, which splits the halves apart. The art is sourced from the
 * canonical {@link BANNER_LINES} (mirrors `crates/shelbi-cli/src/wizard.rs`) so
 * it can't drift from what the terminal actually shows.
 *
 * Dark terminal styling with a light foreground keeps the "SHELBI" wordmark
 * legible in both light and dark docs themes; the tagline sits beneath it, the
 * way the wizard prints it.
 */
export function TerminalBanner() {
  return (
    <div className="my-3 overflow-x-auto rounded-md border border-gray-4 bg-[#1e1e1e] p-4 text-[#e5e5e5]">
      <pre
        aria-label="SHELBI"
        className="m-0 w-fit border-0 bg-transparent p-0 font-mono text-[10px] leading-none whitespace-pre sm:text-xs"
      >
        {BANNER_LINES.join("\n")}
      </pre>
      <p className="mt-3 font-mono text-xs text-[#b8b8b8]">
        an open-source agent orchestrator for the terminal
      </p>
    </div>
  )
}
