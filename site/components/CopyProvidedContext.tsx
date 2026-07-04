"use client"

import { createContext, useContext } from "react"

/**
 * Signals that an ancestor component already renders a copy button for the
 * fenced code inside it (e.g. {@link CodeTabs}, which reads the active pane's
 * text). {@link Pre} reads this and skips its own hover copy button so a nested
 * fenced block doesn't render two stacked buttons. Default `false` — standalone
 * fenced blocks still get the single `Pre` hover button.
 */
export const CopyProvidedContext = createContext(false)

export function useCopyProvided() {
  return useContext(CopyProvidedContext)
}
