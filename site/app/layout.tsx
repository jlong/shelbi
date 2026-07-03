import type { Metadata } from "next"
import { Analytics } from "@vercel/analytics/next"
import { GeistSans } from "geist/font/sans"
import { GeistMono } from "geist/font/mono"
import { Header } from "@/components/Header"
import { Footer } from "@/components/Footer"
import { getSections, humanizeSection } from "@/lib/docs"
import "./globals.css"

const SITE_URL = "https://shelbi.dev"
const SITE_DESCRIPTION =
  "Open-source agent orchestrator for the terminal — a Kanban board that dispatches coding tasks to a named pool of workspaces, locally or over SSH."

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: "Shelbi",
  description: SITE_DESCRIPTION,
  openGraph: {
    type: "website",
    siteName: "Shelbi",
    url: SITE_URL,
    title: "Shelbi",
    description: SITE_DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    title: "Shelbi",
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
      suppressHydrationWarning
      className={`${GeistSans.variable} ${GeistMono.variable} antialiased`}
    >
      <head>
        <script
          suppressHydrationWarning
          dangerouslySetInnerHTML={{ __html: THEME_BOOT_SCRIPT }}
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
