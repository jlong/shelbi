import type { Metadata } from "next"
import { Analytics } from "@vercel/analytics/react"
import { GeistSans } from "geist/font/sans"
import { GeistMono } from "geist/font/mono"
import { Header } from "@/components/Header"
import { Footer } from "@/components/Footer"
import "./globals.css"

const SITE_URL = "https://shelbi.dev"
const SITE_DESCRIPTION = "Manage a team of coding agents from your terminal."

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
// theme. Reads localStorage['theme'] (set by ThemeToggle); when missing
// it falls back to "system" so SSR, this script, and the React component
// (which also defaults to `preference="system"`) all agree. Resolves the
// preference against `prefers-color-scheme` and toggles `.dark` on <html>.
// On any error (localStorage unavailable, etc.) we add `.dark` — Shelbi
// is terminal-native and the strict-mono dark palette is the safe fallback.
const THEME_BOOT_SCRIPT = `(function(){try{var t=localStorage.getItem('theme');if(t==='light'){return}if(t!=='dark'&&!matchMedia('(prefers-color-scheme:dark)').matches){return}document.documentElement.classList.add('dark')}catch(e){document.documentElement.classList.add('dark')}})()`

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
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
        <Header />
        <div className="flex-1">{children}</div>
        <Footer />
        <Analytics />
      </body>
    </html>
  )
}
