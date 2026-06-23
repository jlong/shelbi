import type { Metadata } from "next"
import Link from "next/link"
import { Wordmark } from "@/components/Wordmark"

const README_URL = "https://github.com/jlong/shelbi#readme"

export const metadata: Metadata = {
  title: "Docs — shelbi",
  description: "Docs are coming soon.",
  robots: "noindex",
}

export default function DocsPlaceholderPage() {
  return (
    <main className="flex min-h-screen items-center justify-center bg-bg p-3 font-mono text-fg">
      <div className="flex max-w-md flex-col items-center gap-3 text-center">
        <Wordmark as="h1" size="md" title="shelbi" />
        <p className="text-gray-7">
          Docs are coming soon. Until then, see the{" "}
          <Link
            href={README_URL}
            className="text-fg underline underline-offset-4 transition-colors hover:text-gray-7"
          >
            README
          </Link>
          .
        </p>
      </div>
    </main>
  )
}
