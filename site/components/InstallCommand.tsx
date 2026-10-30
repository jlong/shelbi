import { ClientVariant } from "./ClientVariant"
import { CodeBlock } from "./CodeBlock"

/**
 * Canonical first-run command: the CLI (TUI). Imported by anywhere the raw
 * string is needed (e.g. OG cards, page metadata), which can't follow the
 * site's client-side TUI | Desktop switch and so always name the CLI.
 * Change it here and every surface updates in one edit.
 */
export const INSTALL_COMMAND = "brew install jlong/shelbi/shelbi"

/**
 * The desktop app's install command. The cask depends on the CLI formula, and
 * Homebrew's tap trust needs both named, so it lists the formula and the cask
 * (the same command as the install guide's "macOS desktop app" tab).
 */
export const DESKTOP_INSTALL_COMMAND =
  "brew install jlong/shelbi/shelbi jlong/shelbi/shelbi-desktop"

/**
 * The install command for the chosen client (the site-wide TUI | Desktop
 * switch). Both are server-rendered and CSS shows one, so it switches with no
 * reload or flash, and each copy button copies the command it sits on.
 */
export function InstallCommand() {
  return (
    <ClientVariant
      tui={<CodeBlock code={INSTALL_COMMAND} lang="bash" />}
      desktop={<CodeBlock code={DESKTOP_INSTALL_COMMAND} lang="bash" />}
    />
  )
}
