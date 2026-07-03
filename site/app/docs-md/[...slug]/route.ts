import { notFound } from "next/navigation"
import { allDocs, getDocBySlug } from "@/lib/docs"
import { docToMarkdown } from "@/lib/llms"

// Per-page markdown endpoint. Reached as `<docs-url>.md` via the rewrite in
// `next.config.ts` (`/docs/:slug.md` → `/docs-md/:slug`), so appending `.md`
// to any docs URL returns that page's clean markdown source as `text/markdown`
// for handing straight to a coding agent. Prerendered at build for every doc.
export const dynamic = "force-static"

export function generateStaticParams() {
  return allDocs.map((doc) => ({
    slug: doc._raw.flattenedPath.replace(/^docs\//, "").split("/"),
  }))
}

type RouteProps = {
  params: Promise<{ slug: string[] }>
}

export async function GET(_request: Request, { params }: RouteProps) {
  const { slug } = await params
  const doc = getDocBySlug(slug)
  if (!doc) notFound()

  return new Response(docToMarkdown(doc), {
    headers: {
      "content-type": "text/markdown; charset=utf-8",
      "cache-control": "public, max-age=0, must-revalidate",
    },
  })
}
