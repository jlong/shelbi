import type { Metadata } from "next"
import type { SVGProps } from "react"

export const metadata: Metadata = {
  title: "Join the Shelbi Discord — Shelbi",
  description:
    "Join the Shelbi Discord to ask questions, share what you are building with Shelbi, and follow development.",
}

// Discord invite is read from the environment so it can be rotated
// without a redeploy; falls back to the current public invite.
const DISCORD_INVITE =
  process.env.NEXT_PUBLIC_DISCORD_INVITE ?? "https://discord.gg/VJ8CNFHtHy"

// Discord's official brand glyph, inlined so no icon dependency is
// needed. Uses currentColor so it inherits the button's text color.
function DiscordIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 127.14 96.36" fill="currentColor" {...props}>
      <path d="M107.7 8.07A105.15 105.15 0 0 0 81.47 0a72.06 72.06 0 0 0-3.36 6.83 97.68 97.68 0 0 0-29.11 0A72.37 72.37 0 0 0 45.64 0a105.89 105.89 0 0 0-26.25 8.09C2.79 32.65-1.71 56.6.54 80.21a105.73 105.73 0 0 0 32.17 16.15 77.7 77.7 0 0 0 6.89-11.11 68.42 68.42 0 0 1-10.85-5.18c.91-.66 1.8-1.34 2.66-2a75.57 75.57 0 0 0 64.32 0c.87.71 1.76 1.39 2.66 2a68.68 68.68 0 0 1-10.87 5.19 77 77 0 0 0 6.89 11.1 105.25 105.25 0 0 0 32.19-16.14c2.64-27.38-4.51-51.11-18.9-72.15ZM42.45 65.69C36.18 65.69 31 60 31 53s5-12.74 11.43-12.74S54 46 53.89 53s-5.05 12.69-11.44 12.69Zm42.24 0C78.41 65.69 73.25 60 73.25 53s5-12.74 11.44-12.74S96.23 46 96.12 53s-5.04 12.69-11.43 12.69Z" />
    </svg>
  )
}

export default function DiscordPage() {
  return (
    <main className="mx-auto w-full max-w-3xl px-3 py-4 md:py-6 lg:px-4">
      <p className="mb-1.5 font-mono text-sm font-medium uppercase tracking-[0.2em] text-accent">
        Community
      </p>
      <h1 className="text-3xl font-semibold tracking-tight text-fg">
        Join the Shelbi community
      </h1>
      <p className="mt-2 max-w-xl text-gray-7">
        The Shelbi Discord is where Shelbi work happens in the open. Ask
        questions, share what you are building, and follow development as it
        lands. Low key and developer-first.
      </p>

      <a
        href={DISCORD_INVITE}
        target="_blank"
        rel="noopener noreferrer"
        className="mt-6 inline-flex items-center gap-2 rounded-sm border border-accent px-3 py-1.5 font-sans text-sm font-medium text-accent transition-colors hover:bg-accent hover:text-bg"
      >
        <DiscordIcon className="h-4 w-4" aria-hidden="true" />
        Join the Shelbi Discord
      </a>
    </main>
  )
}
