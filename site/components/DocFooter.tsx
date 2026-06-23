import { ArrowLeftIcon, ArrowRightIcon, PencilSquareIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import type { Doc } from "@/lib/docs"

type DocFooterProps = {
  prev?: Doc
  next?: Doc
  editUrl: string
}

/**
 * Foot-of-page navigation for a doc — prev/next within the reading order
 * produced by `getPrevNext()` (across section boundaries) and an
 * `Edit this page on GitHub` link.
 */
export function DocFooter({ prev, next, editUrl }: DocFooterProps) {
  return (
    <footer className="mt-8 border-t border-gray-4 pt-4">
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
        {prev ? (
          <Link
            href={prev.url}
            className="group flex flex-col gap-0.5 rounded-md border border-gray-4 bg-gray-1 px-3 py-2 transition-colors hover:border-gray-5"
          >
            <span className="inline-flex items-center gap-1 font-mono text-xs tracking-wider text-gray-6 uppercase">
              <ArrowLeftIcon className="h-2 w-2" aria-hidden="true" />
              Previous
            </span>
            <span className="font-medium text-fg group-hover:underline">
              {prev.title}
            </span>
          </Link>
        ) : (
          <div aria-hidden="true" />
        )}
        {next ? (
          <Link
            href={next.url}
            className="group flex flex-col gap-0.5 rounded-md border border-gray-4 bg-gray-1 px-3 py-2 text-right transition-colors hover:border-gray-5 sm:col-start-2"
          >
            <span className="inline-flex items-center justify-end gap-1 font-mono text-xs tracking-wider text-gray-6 uppercase">
              Next
              <ArrowRightIcon className="h-2 w-2" aria-hidden="true" />
            </span>
            <span className="font-medium text-fg group-hover:underline">
              {next.title}
            </span>
          </Link>
        ) : null}
      </div>

      <p className="mt-3 text-sm text-gray-7">
        <Link
          href={editUrl}
          className="inline-flex items-center gap-1 text-gray-7 transition-colors hover:text-fg"
          target="_blank"
          rel="noreferrer"
        >
          <PencilSquareIcon className="h-2 w-2" aria-hidden="true" />
          Edit this page on GitHub
        </Link>
      </p>
    </footer>
  )
}
