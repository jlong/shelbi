"use client"

import { useCallback, useSyncExternalStore } from "react"
import {
  CLIENT_ATTRIBUTE,
  CLIENT_STORAGE_KEY,
  DEFAULT_CLIENT,
  parseClient,
  type ClientKind,
} from "@/lib/client-preference"

/**
 * The site-wide TUI / Desktop preference as a React hook, shared by the home
 * page and the docs. The source of truth after boot is the `<html
 * data-client>` attribute (set before paint by `CLIENT_BOOT_SCRIPT`), so the
 * hook, the CSS variants and every open tab agree:
 *
 * - `setClient` writes localStorage, flips the attribute, and notifies every
 *   subscriber in this tab (a custom event);
 * - a change made in another tab arrives as a `storage` event and is applied
 *   to this tab's attribute too.
 *
 * During SSR and hydration the hook reports the default (`tui`), then settles
 * on the stored value right after. Anything visual should key off the CSS
 * variants (`ClientVariant`, `client-desktop:` / `client-tui:` classes) so it
 * is right from the first frame; the hook is for behavior and ARIA state.
 */

const CHANGE_EVENT = "shelbi-client-change"

function readClient(): ClientKind {
  return parseClient(document.documentElement.getAttribute(CLIENT_ATTRIBUTE))
}

function applyClient(client: ClientKind) {
  const root = document.documentElement
  if (client === DEFAULT_CLIENT) root.removeAttribute(CLIENT_ATTRIBUTE)
  else root.setAttribute(CLIENT_ATTRIBUTE, client)
}

function subscribe(onChange: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key !== CLIENT_STORAGE_KEY) return
    applyClient(parseClient(event.newValue))
    onChange()
  }
  window.addEventListener(CHANGE_EVENT, onChange)
  window.addEventListener("storage", onStorage)
  return () => {
    window.removeEventListener(CHANGE_EVENT, onChange)
    window.removeEventListener("storage", onStorage)
  }
}

/** Set the preference everywhere: storage, the `<html>` attribute, every hook. */
export function setClientPreference(client: ClientKind) {
  try {
    localStorage.setItem(CLIENT_STORAGE_KEY, client)
  } catch {
    // Storage blocked (private mode, a sandboxed frame): the choice still
    // applies to this page, it just won't persist.
  }
  applyClient(client)
  window.dispatchEvent(new Event(CHANGE_EVENT))
}

/** `[client, setClient]` for the site-wide TUI / Desktop choice. */
export function useClientPreference(): [ClientKind, (client: ClientKind) => void] {
  const client = useSyncExternalStore(subscribe, readClient, () => DEFAULT_CLIENT)
  const set = useCallback((next: ClientKind) => setClientPreference(next), [])
  return [client, set]
}
