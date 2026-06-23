import { defineDocumentType, makeSource } from "contentlayer2/source-files"
import rehypePrettyCode, { type Options as RehypePrettyCodeOptions } from "rehype-pretty-code"
import rehypeSlug from "rehype-slug"
import remarkGfm from "remark-gfm"

import { shelbiMonoDark } from "./lib/shiki-mono-dark"

/**
 * Docs live under `content/docs/<section>/<slug>.mdx`. The directory directly
 * beneath `docs/` is the section; the flattened path (minus the `docs/` prefix)
 * is the public slug. A doc placed directly in `docs/` has an empty section.
 */
export const Doc = defineDocumentType(() => ({
  name: "Doc",
  filePathPattern: "docs/**/*.mdx",
  contentType: "mdx",
  fields: {
    title: { type: "string", required: true },
    order: { type: "number", required: true },
    summary: { type: "string", required: true },
  },
  computedFields: {
    url: {
      type: "string",
      // flattenedPath already includes the `docs/` prefix → `/docs/...`.
      resolve: (doc) => `/${doc._raw.flattenedPath}`,
    },
    section: {
      type: "string",
      resolve: (doc) => {
        const parts = doc._raw.flattenedPath.split("/")
        // ["docs", "<section>", "<slug>"] → section; top-level docs have none.
        return parts.length > 2 ? parts[1] : ""
      },
    },
  },
}))

/**
 * Head-to-head comparisons live under `content/vs/<slug>.mdx`. One MDX per
 * competitor. `slug` controls the public path (`/vs/<slug>`); files whose
 * slug begins with `_` (e.g. `_template`) are filtered out of listings and
 * routes by `lib/comparisons.ts` but still validate against this schema so
 * the template documents the required shape.
 */
export const Comparison = defineDocumentType(() => ({
  name: "Comparison",
  filePathPattern: "vs/**/*.mdx",
  contentType: "mdx",
  fields: {
    competitor: { type: "string", required: true },
    slug: { type: "string", required: true },
    competitorUrl: { type: "string", required: true },
    summary: { type: "string", required: true },
    researchUrl: { type: "string", required: false },
  },
  computedFields: {
    url: { type: "string", resolve: (doc) => `/vs/${doc.slug}` },
  },
}))

// Strict-monochrome theme — vesper highlighted bash with vivid cyan-teal
// (`#99FFE4`) and warm peach (`#FFC799`) accents that read as color on the
// no-hue surface from §3 of the plan. See `lib/shiki-mono-dark.ts`.
const rehypePrettyCodeOptions: RehypePrettyCodeOptions = {
  theme: shelbiMonoDark,
  keepBackground: true,
}

export default makeSource({
  contentDirPath: "content",
  documentTypes: [Doc, Comparison],
  // The `contentlayer/generated` alias is provided via tsconfig `paths` (no
  // `baseUrl` needed under `moduleResolution: bundler`), so silence the heuristic.
  disableImportAliasWarning: true,
  // Treat missing/mistyped frontmatter as a build failure rather than silently
  // dropping the document — required fields are enforced at build time.
  onMissingOrIncompatibleData: "fail",
  mdx: {
    // remark-gfm enables GitHub-flavored markdown: pipe tables, strikethrough,
    // autolinks, and task lists — none of which the default remark parser handles.
    remarkPlugins: [remarkGfm],
    // rehype-slug attaches stable IDs to headings so the on-this-page rail and
    // in-document anchor links resolve to the same slugs we extract from the
    // raw MDX in `lib/docs.ts`.
    rehypePlugins: [rehypeSlug, [rehypePrettyCode, rehypePrettyCodeOptions]],
  },
})
