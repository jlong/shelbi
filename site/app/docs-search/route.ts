import { buildSearchRecords } from "@/lib/search"

// `/docs-search` — the client-side search corpus for the ⌘K docs palette,
// generated from contentlayer at build and served static as JSON. The palette
// (`components/DocsSearch.tsx`) fetches this lazily on first open and indexes
// it with Fuse.js in the browser, so there's no external search service and
// the payload never touches the initial page load. The path is intentionally
// extensionless — a `.json` suffix collides with Next's internal data-route
// handling and 404s under `next start`; the JSON content-type is set below.
export const dynamic = "force-static"

export function GET() {
  return new Response(JSON.stringify(buildSearchRecords()), {
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "public, max-age=0, must-revalidate",
    },
  })
}
