import GithubSlugger from "github-slugger"
import { humanizeSection, sortedDocs, type Doc } from "@/lib/docs"

/**
 * One searchable chunk of the docs. Pages are split into a page-level record
 * (title + summary + intro prose) plus one record per H2/H3 heading, so the
 * command palette can deep-link a result straight to the matched section
 * anchor. `id` is unique across the whole corpus; `url` already carries the
 * `#anchor` for heading records.
 */
export type SearchRecord = {
  id: string
  /** Destination link, including the `#anchor` for heading records. */
  url: string
  /** The owning page's URL (no anchor), used to group results. */
  pageUrl: string
  pageTitle: string
  /** Section directory name beneath `docs/` ("" for top-level docs). */
  section: string
  /** Human label for the section ("Docs" for the sectionless group). */
  sectionLabel: string
  /** The matched heading's text, or null for a page-level record. */
  heading: string | null
  /** Plain-text body of this chunk, used for matching and snippets. */
  content: string
  /** The page summary — present on the page-level record only. */
  summary: string | null
}

// Bound the stored body per chunk so the generated JSON stays small; matches
// and snippets only need the leading prose, not an entire long section.
const MAX_CONTENT_CHARS = 1200

/** Strip a heading line's inline markdown down to its display text. */
function stripInlineMarkdown(text: string): string {
  return text
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .trim()
}

/**
 * Collapse a block of MDX into plain, searchable text: drop JSX/HTML tags,
 * reduce links to their label, and remove markdown punctuation. Code content
 * is preserved (command and flag names are worth searching) — only the fence
 * markers were already dropped while chunking.
 */
function toPlainText(md: string): string {
  return md
    .replace(/<[^>]+>/g, " ")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/[*_~`>#|]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
}

type Chunk = { heading: string | null; id: string | null; lines: string[] }

/**
 * Split a doc's raw MDX body into chunks at each H2/H3 heading. The leading
 * chunk (before the first heading) has `heading: null` and folds into the
 * page-level record. Heading slugs are generated with the same
 * `github-slugger` that backs `rehype-slug`, so the anchors resolve against
 * the IDs stamped onto the rendered headings. Fenced code is kept as text but
 * its ``` markers are dropped, and MDX comment blocks (`{/* … *\/}`) are
 * skipped entirely — a heading commented out that way isn't rendered (and so
 * carries no anchor), so indexing it would strand a broken deep-link.
 */
function chunkBody(doc: Doc): Chunk[] {
  const slugger = new GithubSlugger()
  const chunks: Chunk[] = []
  let current: Chunk = { heading: null, id: null, lines: [] }
  let inFence = false
  let inComment = false

  for (const raw of doc.body.raw.split("\n")) {
    let line = raw
    if (!inFence) {
      // Drop self-contained inline comments, then track block comments that
      // open (or continue) across line boundaries. Fenced code is left alone.
      line = line.replace(/\{\/\*[\s\S]*?\*\/\}/g, "")
      if (inComment) {
        const close = line.indexOf("*/}")
        if (close === -1) continue
        inComment = false
        line = line.slice(close + 3)
      }
      const open = line.indexOf("{/*")
      if (open !== -1) {
        inComment = true
        line = line.slice(0, open)
      }
    }
    if (/^\s{0,3}```/.test(line)) {
      inFence = !inFence
      continue
    }
    if (inFence) {
      current.lines.push(line)
      continue
    }
    const match = /^(#{2,3})\s+(.+?)\s*$/.exec(line)
    if (match) {
      chunks.push(current)
      const text = stripInlineMarkdown(match[2])
      current = { heading: text, id: slugger.slug(text), lines: [] }
      continue
    }
    current.lines.push(line)
  }
  chunks.push(current)
  return chunks
}

/**
 * Build the flat search corpus from every `content/docs/**` page. Consumed at
 * build time by the `/docs-search` route, which the client palette loads
 * lazily on first open and indexes with Fuse.js — no external search service.
 */
export function buildSearchRecords(): SearchRecord[] {
  const records: SearchRecord[] = []

  for (const doc of sortedDocs) {
    const sectionLabel = doc.section ? humanizeSection(doc.section) : "Docs"
    const chunks = chunkBody(doc)

    const intro = chunks.find((chunk) => chunk.heading === null)
    const introText = intro ? toPlainText(intro.lines.join("\n")) : ""
    records.push({
      id: doc.url,
      url: doc.url,
      pageUrl: doc.url,
      pageTitle: doc.title,
      section: doc.section,
      sectionLabel,
      heading: null,
      content: introText.slice(0, MAX_CONTENT_CHARS),
      summary: doc.summary,
    })

    for (const chunk of chunks) {
      if (chunk.heading === null || !chunk.id) continue
      const content = toPlainText(chunk.lines.join("\n")).slice(
        0,
        MAX_CONTENT_CHARS,
      )
      records.push({
        id: `${doc.url}#${chunk.id}`,
        url: `${doc.url}#${chunk.id}`,
        pageUrl: doc.url,
        pageTitle: doc.title,
        section: doc.section,
        sectionLabel,
        heading: chunk.heading,
        content,
        summary: null,
      })
    }
  }

  return records
}
