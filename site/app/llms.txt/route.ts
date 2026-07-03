import { renderLlmsTxt } from "@/lib/llms"

// `/llms.txt` — the agent-facing index of the docs (title + summary + `.md`
// link per page, grouped by section). Generated from contentlayer at build and
// served static as text/plain. See https://llmstxt.org.
export const dynamic = "force-static"

export function GET() {
  return new Response(renderLlmsTxt(), {
    headers: { "content-type": "text/plain; charset=utf-8" },
  })
}
