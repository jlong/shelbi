import { CodeBlock } from "./CodeBlock"

/**
 * Canonical install command for the hosted install script. Imported by the
 * marketing closer, the docs install page (via the MDX components map), and
 * anywhere else the raw string is needed (e.g. OG cards, page metadata).
 * Change it here and every surface updates in one edit.
 */
export const INSTALL_COMMAND = "curl -fsSL https://shelbi.dev/install.sh | sh"

export function InstallCommand() {
  return <CodeBlock code={INSTALL_COMMAND} lang="bash" />
}
