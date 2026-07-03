import GithubSlugger from "github-slugger"
import { allDocs, type Doc } from "contentlayer/generated"

export { allDocs }
export type { Doc }

/** Docs sorted by their `order` frontmatter, ascending. */
export const sortedDocs: Doc[] = [...allDocs].sort((a, b) => a.order - b.order)

/**
 * Resolve a doc from the `[[...slug]]` route segments, e.g.
 * `["getting-started", "install"]` → the doc whose flattened path is
 * `docs/getting-started/install`.
 */
export function getDocBySlug(slug: string[] | undefined): Doc | undefined {
  const path = ["docs", ...(slug ?? [])].join("/")
  return allDocs.find((doc) => doc._raw.flattenedPath === path)
}

export type DocSection = {
  /** The directory name beneath `docs/` (empty string for top-level docs). */
  section: string
  docs: Doc[]
}

/**
 * Canonical order of the top-level nav groups, keyed by directory name. The IA
 * puts Guides first, then Concepts, Configuration, and CLI Reference — an
 * ordering the raw numeric `order` frontmatter doesn't produce on its own (the
 * guides pages carry higher numbers than concepts/cli), so we pin it explicitly
 * here rather than renumbering every page and hoping the numbers stay coherent
 * across sections. Anything not listed (e.g. `getting-started`, or a sectionless
 * doc) falls in behind the explicit entries, ordered by its own `order`.
 */
export const TOP_LEVEL_ORDER = [
  "guides",
  "getting-started",
  "concepts",
  "configuration",
  "cli",
]

/**
 * Sort key for a top-level nav group. Explicitly-ordered directories come
 * first in `TOP_LEVEL_ORDER` sequence; everything else trails them, ranked by
 * its numeric `order` so the changelog (order 99) stays last.
 */
function topLevelRank(key: string, fallbackOrder: number): number {
  const i = TOP_LEVEL_ORDER.indexOf(key)
  return i !== -1 ? i : TOP_LEVEL_ORDER.length + fallbackOrder
}

/**
 * Group docs by section, each section's docs sorted by `order`, and the
 * sections themselves ordered by {@link topLevelRank}. Sectionless docs
 * (placed directly in `docs/`) collapse into a group with `section: ""`.
 *
 * This is the flat view of the nav, consumed by the mobile drawer, the docs
 * index, prev/next, and `llms.txt`. The sidebar's expandable hierarchy is
 * built separately by {@link getDocsTree}; both share `topLevelRank` so the
 * two stay in the same order.
 */
export function getSections(): DocSection[] {
  const bySection = new Map<string, Doc[]>()
  for (const doc of sortedDocs) {
    const group = bySection.get(doc.section) ?? []
    group.push(doc)
    bySection.set(doc.section, group)
  }
  return [...bySection.entries()]
    .map(([section, docs]) => ({ section, docs }))
    .sort(
      (a, b) =>
        topLevelRank(a.section, a.docs[0].order) -
        topLevelRank(b.section, b.docs[0].order),
    )
}

/**
 * A single-page nav entry — renders as a plain link.
 */
export type DocLeaf = {
  kind: "leaf"
  /** Path segment (directory or file stem), used as a stable key. */
  key: string
  title: string
  url: string
  order: number
  doc: Doc
}

/**
 * A multi-page guide: a directory that owns an `index.mdx` overview plus one or
 * more ordered sub-pages. Renders as a collapsible group in the sidebar.
 */
export type DocGuide = {
  kind: "guide"
  key: string
  /** Guide label, taken from the overview page's title. */
  title: string
  /** URL of the overview (`index.mdx`) page. */
  url: string
  order: number
  overview: Doc
  /** Ordered sub-pages (may themselves be nested guides). */
  children: DocNavItem[]
}

/**
 * A titled section: a directory with no `index.mdx` of its own. Renders as an
 * uppercase heading with its items beneath — the items may be plain links or
 * nested guides.
 */
export type DocSectionGroup = {
  kind: "section"
  key: string
  label: string
  order: number
  items: DocNavItem[]
}

/** An entry nested inside a guide or section: a link or a nested guide. */
export type DocNavItem = DocLeaf | DocGuide
/** A top-level nav node: a section, a guide, or a lone link. */
export type DocNavNode = DocNavItem | DocSectionGroup

type RawNode = { seg: string; doc?: Doc; children: Map<string, RawNode> }

/** The `order` a nav node sorts by within its parent. */
function navOrder(node: DocNavNode): number {
  return node.order
}

/**
 * Convert a raw path node into a nav node. The convention is folder-based and
 * composes with contentlayer's `flattenedPath`:
 *
 * - a directory with children AND its own `index.mdx` → a **guide**
 *   (collapsible; the index is the overview, the rest are ordered sub-pages);
 * - a directory with children but no index → a **section** (heading + items);
 * - a path with no children → a **leaf** (plain link).
 *
 * It recurses, so a section may contain guides and a guide may contain nested
 * guides — the model already supports the deeper IA that later content moves
 * will introduce, even though today only `guides/` exercises it.
 */
function toNavNode(node: RawNode): DocNavNode {
  const children = [...node.children.values()]
    .map(toNavNode)
    .sort((a, b) => navOrder(a) - navOrder(b)) as DocNavItem[]

  if (children.length === 0) {
    // A childless node is always a doc (the raw tree only creates a node to
    // hold a doc or to parent one), so `doc` is present here.
    const doc = node.doc as Doc
    return { kind: "leaf", key: node.seg, title: doc.title, url: doc.url, order: doc.order, doc }
  }

  if (node.doc) {
    const doc = node.doc
    return {
      kind: "guide",
      key: node.seg,
      title: doc.title,
      url: doc.url,
      order: doc.order,
      overview: doc,
      children,
    }
  }

  return {
    kind: "section",
    key: node.seg,
    label: humanizeSection(node.seg),
    order: children[0].order,
    items: children,
  }
}

/**
 * Build the hierarchical sidebar nav from every doc's `flattenedPath`. Returns
 * the ordered top-level nodes (sections, guides, and lone links). Top-level
 * ordering matches {@link getSections} via {@link topLevelRank}.
 */
export function getDocsTree(): DocNavNode[] {
  const root: RawNode = { seg: "", children: new Map() }
  for (const doc of allDocs) {
    // Drop the leading `docs` segment; the rest are the nav path.
    const segs = doc._raw.flattenedPath.split("/").slice(1)
    let cur = root
    for (const seg of segs) {
      let child = cur.children.get(seg)
      if (!child) {
        child = { seg, children: new Map() }
        cur.children.set(seg, child)
      }
      cur = child
    }
    cur.doc = doc
  }
  return [...root.children.values()]
    .map(toNavNode)
    .sort((a, b) => topLevelRank(a.key, a.order) - topLevelRank(b.key, b.order))
}

/**
 * Display labels for section directories whose humanized name isn't what the IA
 * wants — `cli` title-cases to "Cli", but the section is "CLI Reference". Keyed
 * by directory name; anything absent falls back to plain title-casing.
 */
const SECTION_LABELS: Record<string, string> = {
  cli: "CLI Reference",
}

/**
 * Display label for a section directory: an explicit {@link SECTION_LABELS}
 * override if one exists, otherwise the kebab name title-cased.
 */
export function humanizeSection(section: string): string {
  const override = SECTION_LABELS[section]
  if (override) return override
  return section
    .split("-")
    .map((word) => (word ? word[0].toUpperCase() + word.slice(1) : word))
    .join(" ")
}

export type Heading = { depth: 2 | 3; text: string; id: string }

/**
 * Extract H2/H3 headings from a doc's raw MDX. IDs are generated with the
 * same `github-slugger` that backs `rehype-slug`, so they match the IDs
 * stamped onto the rendered headings — anchor links and scroll-spy stay
 * in sync.
 */
export function extractHeadings(doc: Doc): Heading[] {
  const slugger = new GithubSlugger()
  const lines = doc.body.raw.split("\n")
  const headings: Heading[] = []
  let inFence = false
  for (const line of lines) {
    if (/^\s{0,3}```/.test(line)) {
      inFence = !inFence
      continue
    }
    if (inFence) continue
    const match = /^(#{2,3})\s+(.+?)\s*$/.exec(line)
    if (!match) continue
    const depth = match[1].length as 2 | 3
    const text = stripInlineMarkdown(match[2])
    if (!text) continue
    headings.push({ depth, text, id: slugger.slug(text) })
  }
  return headings
}

function stripInlineMarkdown(text: string): string {
  return text
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .trim()
}

/**
 * Prev/next siblings for a doc, walking the flat reading order produced by
 * `getSections()` — within a section by `order`, then across sections by
 * `topLevelRank` (the same ordering the sidebar tree uses). A guide's overview
 * (`index.mdx`, lowest `order`) leads its sub-pages, so the flat walk matches
 * the expanded hierarchy. Returns `undefined` at the boundaries.
 */
export function getPrevNext(doc: Doc): { prev?: Doc; next?: Doc } {
  const flat = getSections().flatMap((s) => s.docs)
  const idx = flat.findIndex((d) => d.url === doc.url)
  if (idx === -1) return {}
  return {
    prev: idx > 0 ? flat[idx - 1] : undefined,
    next: idx < flat.length - 1 ? flat[idx + 1] : undefined,
  }
}

const REPO_EDIT_BASE = "https://github.com/jlong/shelbi/edit/main/site/content"

/** Resolve the `edit on GitHub` URL for a doc, pointing at its source file. */
export function getEditUrl(doc: Doc): string {
  return `${REPO_EDIT_BASE}/${doc._raw.sourceFilePath}`
}
