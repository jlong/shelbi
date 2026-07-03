import Link from "next/link"
import { ArrowRightIcon } from "@heroicons/react/24/outline"
import { InstallCommand } from "./InstallCommand"

export function InstallCloser() {
  return (
    <section className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <div className="flex flex-col items-start gap-4">
          <h2 className="font-sans text-4xl font-semibold tracking-tight text-fg">
            Get started
          </h2>
          <div className="w-full max-w-3xl">
            <InstallCommand />
          </div>
          <Link
            href="/docs/guides/getting-started/install#build-from-source"
            className="inline-flex items-center gap-1 self-start font-mono text-sm text-gray-7 transition-colors hover:text-fg"
          >
            <span>Build from source</span>
            <ArrowRightIcon className="h-2 w-2" />
          </Link>
        </div>
      </div>
    </section>
  )
}
