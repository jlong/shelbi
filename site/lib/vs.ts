import { allVs as rawVs, type Vs } from "contentlayer/generated"

export type { Vs }

/**
 * Published comparison pages. Authoring templates (`content/vs/_*.mdx`) match
 * the contentlayer pattern too, so drop any underscore-prefixed slug here —
 * the template is a starting point for new pages, not a route.
 */
export const allVs: Vs[] = rawVs.filter((vs) => !vs.slug.startsWith("_"))

/** Comparison pages sorted by their `order` frontmatter, ascending. */
export const sortedVs: Vs[] = [...allVs].sort((a, b) => a.order - b.order)

/** Resolve a published comparison page from its `[slug]` route segment. */
export function getVsBySlug(slug: string): Vs | undefined {
  return allVs.find((vs) => vs.slug === slug)
}
