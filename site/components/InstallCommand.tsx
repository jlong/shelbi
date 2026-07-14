import { CodeBlock } from "./CodeBlock"

/**
 * Canonical first-run command. Imported by the marketing closer and anywhere
 * else the raw string is needed (e.g. OG cards, page metadata).
 * Change it here and every surface updates in one edit.
 */
export const INSTALL_COMMAND = "brew install jlong/shelbi/shelbi && shelbi"

export function InstallCommand() {
  return <CodeBlock code={INSTALL_COMMAND} lang="bash" />
}
