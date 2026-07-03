import Link from "next/link"
import { Wordmark } from "./Wordmark"

const REPO_URL = "https://github.com/jlong/shelbi"

const PRODUCT = [
  { label: "Install", href: "/docs/guides/getting-started/install" },
  { label: "Changelog", href: "/docs/changelog" },
] as const

const DOCS = [
  { label: "Getting Started", href: "/docs/guides/getting-started/install" },
  { label: "Concepts", href: "/docs/concepts/agents" },
  { label: "CLI Reference", href: "/docs/cli/task" },
  { label: "Changelog", href: "/docs/changelog" },
] as const

const COMPARE = [
  { label: "Conductor", href: "/vs/conductor" },
  { label: "Herd", href: "/vs/herd" },
  { label: "OpenHands", href: "/vs/openhands" },
  { label: "Devin", href: "/vs/devin" },
  { label: "Sketch.dev", href: "/vs/sketch" },
  { label: "Cursor Background Agents", href: "/vs/cursor-background-agents" },
] as const

const RESOURCES = [
  { label: "GitHub", href: REPO_URL },
  { label: "License", href: `${REPO_URL}/blob/main/LICENSE` },
  { label: "Issues", href: `${REPO_URL}/issues` },
] as const

function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width={14}
      height={14}
      fill="currentColor"
      aria-hidden="true"
      focusable="false"
      className={className}
    >
      <path
        fillRule="evenodd"
        d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"
      />
    </svg>
  )
}

function ColumnHeader({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="font-mono text-xs font-semibold uppercase tracking-wide text-gray-7">
      {children}
    </h3>
  )
}

function NavList({
  items,
}: {
  items: readonly { label: string; href: string }[]
}) {
  return (
    <ul className="mt-3 flex flex-col gap-2">
      {items.map((item) => (
        <li key={item.href}>
          <Link
            href={item.href}
            className="text-sm text-gray-7 transition-colors hover:text-fg"
          >
            {item.label}
          </Link>
        </li>
      ))}
    </ul>
  )
}

export function Footer() {
  return (
    <footer className="border-t border-gray-4 bg-bg">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <div className="grid grid-cols-1 gap-8 sm:grid-cols-2 lg:grid-cols-4">
          <div className="flex flex-col gap-3">
            <Link
              href="/"
              aria-label="Shelbi home"
              className="inline-flex w-fit text-fg transition-colors hover:text-gray-7"
            >
              <Wordmark size="sm" />
            </Link>
            <p className="max-w-xs text-sm leading-relaxed text-gray-7">
              An open-source agent orchestrator for the terminal.
            </p>
            <Link
              href={REPO_URL}
              className="inline-flex w-fit items-center gap-1.5 text-sm text-gray-7 transition-colors hover:text-fg"
            >
              <GitHubIcon />
              <span>github.com/jlong/shelbi</span>
            </Link>
          </div>

          <div className="flex flex-col gap-6">
            <div>
              <ColumnHeader>Product</ColumnHeader>
              <NavList items={PRODUCT} />
            </div>
            <div>
              <ColumnHeader>Resources</ColumnHeader>
              <NavList items={RESOURCES} />
            </div>
          </div>

          <div>
            <ColumnHeader>Docs</ColumnHeader>
            <NavList items={DOCS} />
          </div>

          <div>
            <ColumnHeader>Compare</ColumnHeader>
            <NavList items={COMPARE} />
          </div>
        </div>

        <div className="mt-8 border-t border-gray-4 pt-4">
          <p className="font-mono text-xs text-gray-6">
            &copy; {new Date().getFullYear()}{" "}
            <Link
              href="https://32pixels.co"
              className="transition-colors hover:text-fg"
            >
              32pixels, LLC
            </Link>
            <span className="px-2 text-gray-5">&middot;</span>
            MIT License
          </p>
        </div>
      </div>
    </footer>
  )
}
