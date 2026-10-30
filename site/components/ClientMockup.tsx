"use client"

import type { CSSProperties } from "react"
import { ClientVariant } from "./ClientVariant"
import { AppMockup, PRESETS, type AppState, type PresetName } from "./KanbanMockup"
import { DesktopAppMockup } from "./desktop/DesktopMockup"

/**
 * Docs MDX: one `AppState` scenario drawn by whichever Shelbi client the
 * reader chose (the docs' TUI | Desktop switch): the terminal dashboard
 * (`AppMockup`) or the desktop app (`DesktopAppMockup`). Takes `AppMockup`'s
 * props (`preset` plus field overrides), so a page swaps `<AppMockup …>` for
 * `<ClientMockup …>` and both clients tell the same story. `desktop` overrides
 * fields for the desktop drawing only (a wider board, say), and
 * `desktopWidth` / `desktopHeight` size its window in app points.
 *
 * A client component: it reads the scenario presets (`PRESETS`), which live in
 * the client-only `KanbanMockup` module and are only references on the server.
 *
 * The desktop window scales with the article column through a container query
 * unit (one point is at most 0.65px, the full column's width), so it fits at
 * every viewport without a layout pass.
 */
export function ClientMockup({
  state,
  preset = "default",
  desktop,
  desktopWidth = 1180,
  desktopHeight = 640,
  label = "The Shelbi desktop app",
  ...overrides
}: {
  state?: AppState
  preset?: PresetName
  desktop?: Partial<AppState>
  desktopWidth?: number
  desktopHeight?: number
  label?: string
} & Partial<AppState>) {
  const merged: AppState = { ...(state ?? PRESETS[preset]), ...overrides }
  const scale = {
    ["--u" as string]: `clamp(0.2px, calc(100cqw / ${desktopWidth}), 0.65px)`,
  } as CSSProperties
  return (
    <ClientVariant
      tui={<AppMockup state={merged} />}
      desktop={
        <section className="border-b border-gray-4 py-6 sm:py-10" style={{ containerType: "inline-size" }}>
          <div className="mx-auto w-fit max-w-full" style={scale}>
            <DesktopAppMockup
              state={{ ...merged, ...desktop }}
              width={desktopWidth}
              height={desktopHeight}
              label={label}
            />
          </div>
        </section>
      }
    />
  )
}
