import { defineDocumentType, makeSource } from "contentlayer2/source-files"
import rehypePrettyCode, { type Options as RehypePrettyCodeOptions } from "rehype-pretty-code"

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

const rehypePrettyCodeOptions: RehypePrettyCodeOptions = {
  theme: "vesper",
  keepBackground: true,
}

export default makeSource({
  contentDirPath: "content",
  documentTypes: [Doc],
  // The `contentlayer/generated` alias is provided via tsconfig `paths` (no
  // `baseUrl` needed under `moduleResolution: bundler`), so silence the heuristic.
  disableImportAliasWarning: true,
  // Treat missing/mistyped frontmatter as a build failure rather than silently
  // dropping the document — required fields are enforced at build time.
  onMissingOrIncompatibleData: "fail",
  mdx: {
    rehypePlugins: [[rehypePrettyCode, rehypePrettyCodeOptions]],
  },
})
