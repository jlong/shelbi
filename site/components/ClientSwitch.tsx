"use client"

import { CommandLineIcon, ComputerDesktopIcon } from "@heroicons/react/24/outline"
import { CLIENTS, type ClientKind } from "@/lib/client-preference"
import { useClientPreference } from "./ClientPreference"

const ICONS: Record<ClientKind, typeof CommandLineIcon> = {
  tui: CommandLineIcon,
  desktop: ComputerDesktopIcon,
}

/**
 * The TUI | Desktop button tabs: a segmented control that picks which Shelbi
 * client every illustration on the site shows (see `useClientPreference`).
 * Reusable anywhere (the home hero, the docs); `label` names the group for
 * assistive tech.
 *
 * The selected segment's look comes from the `client-tui:` /
 * `client-desktop:` CSS variants, keyed off `<html data-client>`, so the
 * stored choice is drawn correctly before hydration. `aria-pressed` follows
 * the hook, which settles on the stored value right after hydration.
 */
export function ClientSwitch({
  label = "Show Shelbi as",
  className = "",
}: {
  label?: string
  className?: string
}) {
  const [client, setClient] = useClientPreference()
  return (
    <div
      role="group"
      aria-label={label}
      className={`inline-flex items-center gap-0.5 rounded-md border border-gray-4 bg-gray-1 p-0.5 ${className}`}
    >
      {CLIENTS.map(({ value, label: text }) => {
        const Icon = ICONS[value]
        const selected =
          value === "tui"
            ? "client-tui:bg-fg client-tui:text-bg client-tui:hover:bg-fg client-tui:hover:text-bg"
            : "client-desktop:bg-fg client-desktop:text-bg client-desktop:hover:bg-fg client-desktop:hover:text-bg"
        return (
          <button
            key={value}
            type="button"
            aria-pressed={client === value}
            onClick={() => setClient(value)}
            className={`inline-flex items-center gap-1 rounded-sm px-2 py-0.5 font-mono text-xs font-medium text-gray-6 outline-none transition-colors hover:bg-gray-3 hover:text-fg focus-visible:[outline-style:solid] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-fg sm:text-sm ${selected}`}
          >
            <Icon className="h-2 w-2" aria-hidden="true" />
            {text}
          </button>
        )
      })}
    </div>
  )
}
