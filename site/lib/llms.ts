import { INSTALL_COMMAND } from "@/components/InstallCommand"
import { getSections, humanizeSection, sortedDocs, type Doc } from "@/lib/docs"

// Canonical origin for absolute links in the agent-facing artifacts. Kept in
// step with the `SITE_URL` in `app/layout.tsx`; both point at production.
export const SITE_URL = "https://shelbi.dev"
const SITE_TAGLINE =
  "Do more with your agents — an open source, multi-machine orchestrator built on tmux. Dispatch tasks to a team of agents locally or over SSH."

/**
 * Turn one contentlayer doc into clean, agent-ready markdown.
 *
 * Frontmatter is already split off by contentlayer (`body.raw` is the body
 * only), so we reconstruct a normalized `# Title` + summary lead and then
 * strip the handful of JSX components the docs embed — none of which carry
 * markdown-meaningful text an agent needs:
 *
 * - `<InstallCommand />` → the actual install command as a fenced block.
 * - `<CopyPromptBanner prompt={`…`} … />` → the prompt itself as a fenced
 *   block (the whole point of the AI-prompts page survives the strip).
 * - any other self-closing PascalCase component (e.g. `<AppMockup … />`) is a
 *   purely-visual mockup and is dropped.
 *
 * All-caps placeholder tokens like `<ID>` or `<NAME>` live inside code fences
 * and are *not* JSX; the component patterns below require a lowercase letter
 * in the tag name and/or a self-closing `/>`, so those are left untouched.
 */
export function docToMarkdown(doc: Doc): string {
  let body = doc.body.raw

  // `<CopyPromptBanner … />` → blockquote lead (title — description) plus the
  // prompt as a fenced block. Attributes may span several lines because the
  // prompt is a multi-line template literal, so match across newlines. Both
  // authoring forms are supported: a `prompt={`…`}` template literal and a
  // `prompt="…"` double-quoted string (the latter is the form the rest of the
  // docs use, and may itself contain backticks — harmless inside the fence).
  body = body.replace(
    /<CopyPromptBanner\b([\s\S]*?)\/>/g,
    (_match, attrs: string) => {
      const prompt = (
        /prompt=\{`([\s\S]*?)`\}/.exec(attrs)?.[1] ??
        /prompt="([^"]*)"/.exec(attrs)?.[1]
      )?.trim()
      if (!prompt) return ""
      const title = /title="([^"]*)"/.exec(attrs)?.[1]
      const description = /description="([^"]*)"/.exec(attrs)?.[1]
      const lead = [title && `**${title}**`, description]
        .filter(Boolean)
        .join(" — ")
      const heading = lead ? `> ${lead}\n\n` : ""
      return `${heading}\`\`\`text\n${prompt}\n\`\`\``
    },
  )

  // `<InstallCommand />` → the canonical install command, single source of
  // truth shared with the rendered page.
  body = body.replace(
    /<InstallCommand\b[^>]*\/>/g,
    `\`\`\`bash\n${INSTALL_COMMAND}\n\`\`\``,
  )

  // Any remaining self-closing PascalCase component on its own line (the tag
  // name must contain a lowercase letter, which excludes all-caps code
  // placeholders like `<ID>`). These are visual-only, so drop the line.
  body = body.replace(
    /^[ \t]*<[A-Z][A-Za-z0-9]*[a-z][A-Za-z0-9]*\b[^>]*\/>[ \t]*$/gm,
    "",
  )

  // Collapse the blank runs left behind by removed components.
  body = body.replace(/\n{3,}/g, "\n\n").trim()

  return `# ${doc.title}\n\n${doc.summary}\n\n${body}\n`
}

/**
 * The `/llms.txt` index — project header, then one bullet per doc grouped by
 * section, each linking to the page's `.md` endpoint so an agent fetches the
 * clean markdown directly. Follows the https://llmstxt.org layout.
 */
export function renderLlmsTxt(): string {
  const lines: string[] = [`# Shelbi`, ``, `> ${SITE_TAGLINE}`, ``]
  for (const { section, docs } of getSections()) {
    lines.push(`## ${section ? humanizeSection(section) : "Docs"}`, ``)
    for (const doc of docs) {
      lines.push(`- [${doc.title}](${SITE_URL}${doc.url}.md): ${doc.summary}`)
    }
    lines.push(``)
  }
  return `${lines.join("\n").trim()}\n`
}

/**
 * The `/llms-full.txt` corpus — every doc's clean markdown concatenated in
 * sidebar reading order, one `---`-delimited block each, for RAG ingestion or
 * pasting a whole knowledge base into an agent.
 */
export function renderLlmsFullTxt(): string {
  const preamble = `# Shelbi — Full Documentation\n\n> ${SITE_TAGLINE}\n\n> Generated from ${SITE_URL}/docs — see ${SITE_URL}/llms.txt for the index.`
  const blocks = sortedDocs.map(
    (doc) => `${docToMarkdown(doc)}\n[Source](${SITE_URL}${doc.url})`,
  )
  return `${[preamble, ...blocks].join("\n\n---\n\n")}\n`
}
