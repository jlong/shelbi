import Link from "next/link"
import { ArrowRightIcon } from "@heroicons/react/24/outline"
import { CodeBlock } from "./CodeBlock"

const INSTALL_COMMAND = "curl -fsSL https://shelbi.dev/install.sh | sh"

export function InstallCloser() {
  return (
    <section className="border-t border-gray-4 px-3 py-8">
      <div className="mx-auto flex max-w-3xl flex-col gap-4">
        <h2 className="font-sans text-4xl font-semibold tracking-tight text-fg">
          Get started
        </h2>
        <CodeBlock code={INSTALL_COMMAND} lang="bash" />
        <Link
          href="/docs/getting-started/install#from-source"
          className="inline-flex items-center gap-1 self-start font-mono text-sm text-gray-7 transition-colors hover:text-fg"
        >
          <span>Build from source</span>
          <ArrowRightIcon className="h-2 w-2" />
        </Link>
      </div>
    </section>
  )
}
