import type { Metadata } from "next"
import { Analytics } from "@vercel/analytics/next"
import { GeistSans } from "geist/font/sans"
import localFont from "next/font/local"
import { Header } from "@/components/Header"
import { Footer } from "@/components/Footer"
import { getSections, humanizeSection } from "@/lib/docs"
import { CLIENT_BOOT_SCRIPT, DEFAULT_CLIENT } from "@/lib/client-preference"
import "./globals.css"

// Mono family for code, CLI hints, and the hero mockup, exposed as the
// `--font-source-code-pro` variable that `--font-mono` (globals.css) points at.
// Vendored so a build never has to reach Google Fonts: this is the variable
// latin subset Google serves, weights 400 (body/code), 500 (medium labels), and
// 600 (semibold, e.g. Footer headings). `next/font/google` also declared
// cyrillic, greek, vietnamese, and latin-ext faces; no page uses those glyphs.
const sourceCodePro = localFont({
  src: "./fonts/SourceCodePro-Latin-Variable.woff2",
  weight: "400 600",
  style: "normal",
  variable: "--font-source-code-pro",
  // The Arial fallback is declared in globals.css with pinned metrics.
  adjustFontFallback: false,
})

const SITE_URL = "https://shelbi.dev"
const SITE_TAGLINE =
  "An open-source, personal agent orchestrator for your desktop and terminal."
// Home page title: brand plus positioning line so the tab, OG title, and
// search snippet all lead with the tagline. Subpages override with their
// own `title` string.
const SITE_TITLE = `Shelbi: ${SITE_TAGLINE}`
const SITE_DESCRIPTION =
  "An open-source, personal agent orchestrator for your desktop and terminal. Dispatch tasks to a team of agents running in parallel across machines, locally or over SSH."

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: SITE_TITLE,
  description: SITE_DESCRIPTION,
  openGraph: {
    type: "website",
    siteName: "Shelbi",
    url: SITE_URL,
    title: SITE_TITLE,
    description: SITE_DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    title: SITE_TITLE,
    description: SITE_DESCRIPTION,
  },
}

// Inline boot script — runs before paint to avoid a flash of the wrong
// theme. Reads localStorage['theme'] (set by ThemeToggle); when missing,
// defaults to dark — Shelbi is terminal-native and the strict-mono dark
// palette is the on-brand default. The OS `prefers-color-scheme` only
// matters when the user has explicitly chosen "system". The React
// component's initial state defaults to "dark" to match.
const THEME_BOOT_SCRIPT = `(function(){try{var t=localStorage.getItem('theme');if(t==='light'){return}if(t==='system'&&!matchMedia('(prefers-color-scheme:dark)').matches){return}document.documentElement.classList.add('dark')}catch(e){document.documentElement.classList.add('dark')}})()`

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  const docsSections = getSections().map(({ section, docs }) => ({
    label: section ? humanizeSection(section) : "",
    items: docs.map((doc) => ({ label: doc.title, href: doc.url })),
  }))
  return (
    <html
      lang="en"
      // The TUI / Desktop choice (lib/client-preference.ts): the SSR HTML
      // carries the default so it paints with JS off; the boot script below
      // rewrites it to a stored choice (hence suppressHydrationWarning).
      data-client={DEFAULT_CLIENT}
      suppressHydrationWarning
      className={`${GeistSans.variable} ${sourceCodePro.variable} antialiased`}
    >
      <head>
        <script
          suppressHydrationWarning
          // The theme, then the TUI / Desktop illustration choice
          // (lib/client-preference.ts), both applied before first paint.
          dangerouslySetInnerHTML={{ __html: `${THEME_BOOT_SCRIPT};${CLIENT_BOOT_SCRIPT}` }}
        />
      </head>
      <body className="bg-bg text-fg flex min-h-screen flex-col font-sans">
        <Header docsSections={docsSections} />
        <div className="flex-1">{children}</div>
        <Footer />
        <Analytics />
      </body>
    </html>
  )
}
