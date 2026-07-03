import { renderLlmsFullTxt } from "@/lib/llms"

// `/llms-full.txt` — the entire docs corpus concatenated into one clean
// markdown file for RAG ingestion or pasting a whole knowledge base into an
// agent. Generated from contentlayer at build and served static as text/plain.
export const dynamic = "force-static"

export function GET() {
  return new Response(renderLlmsFullTxt(), {
    headers: { "content-type": "text/plain; charset=utf-8" },
  })
}
