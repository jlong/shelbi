import type { Metadata } from "next"
import { Analytics } from "@vercel/analytics/react"
import { GeistSans } from "geist/font/sans"
import { GeistMono } from "geist/font/mono"
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

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html
      lang="en"
      className={`${GeistSans.variable} ${GeistMono.variable} antialiased`}
    >
      <body className="bg-bg text-fg min-h-screen font-sans">
        {children}
        <Analytics />
      </body>
    </html>
  )
}
