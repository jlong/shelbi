import Link from "next/link"

const REPO_URL = "https://github.com/jlong/shelbi"
const LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`

const LINKS = [
  { label: "Docs", href: "/docs" },
  { label: "Changelog", href: "/docs/changelog" },
  { label: "GitHub", href: REPO_URL },
] as const

export function Footer() {
  return (
    <footer className="border-t border-gray-4 bg-bg font-mono text-xs text-gray-6">
      <div className="mx-auto flex max-w-4xl flex-col gap-3 px-3 py-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1">
          <Link
            href={REPO_URL}
            className="text-gray-7 transition-colors hover:text-fg"
          >
            github.com/jlong/shelbi
          </Link>
          <p className="text-gray-5">
            <Link
              href={LICENSE_URL}
              className="transition-colors hover:text-fg"
            >
              MIT License
            </Link>
            {" · "}
            <span>&copy; {new Date().getFullYear()} shelbi</span>
          </p>
        </div>
        <nav aria-label="Footer" className="flex items-center gap-3">
          {LINKS.map((link) => (
            <Link
              key={link.href}
              href={link.href}
              className="text-gray-7 transition-colors hover:text-fg"
            >
              {link.label}
            </Link>
          ))}
        </nav>
      </div>
    </footer>
  )
}
