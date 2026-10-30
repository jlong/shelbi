/**
 * Which Shelbi client the site's illustrations show: the terminal app (TUI)
 * or the native desktop app. One site-wide choice, stored in localStorage and
 * mirrored onto `<html data-client="…">` so CSS can swap variants before
 * React hydrates (no flash of the wrong client, no hydration mismatch).
 *
 * This module is server-safe (no hooks, no `window` at import time): the root
 * layout imports {@link CLIENT_BOOT_SCRIPT} to run it in `<head>`. The React
 * side lives in `components/ClientPreference.tsx` (the `useClientPreference`
 * hook), `components/ClientSwitch.tsx` (the TUI | Desktop button tabs) and
 * `components/ClientVariant.tsx` (render one child per client).
 */

export type ClientKind = "tui" | "desktop"

/** Every client, in the switch's order. TUI is the default. */
export const CLIENTS: { value: ClientKind; label: string }[] = [
  { value: "tui", label: "TUI" },
  { value: "desktop", label: "Desktop" },
]

export const DEFAULT_CLIENT: ClientKind = "tui"

/** The localStorage key holding the choice. */
export const CLIENT_STORAGE_KEY = "shelbi-client"

/** The `<html>` attribute CSS keys off: present (`desktop`) only off the default. */
export const CLIENT_ATTRIBUTE = "data-client"

/** Read a stored value back as a client, falling back to the default. */
export function parseClient(value: string | null | undefined): ClientKind {
  return value === "desktop" ? "desktop" : DEFAULT_CLIENT
}

/**
 * Inline boot script for `<head>`: runs before first paint and marks
 * `<html data-client="desktop">` when that is the stored choice, so the
 * CSS-driven variants (see the `client-desktop` / `client-tui` variants in
 * `app/globals.css`) paint the right client from the first frame. With no
 * stored choice (or storage blocked) it leaves the attribute off: TUI.
 */
export const CLIENT_BOOT_SCRIPT = `(function(){try{if(localStorage.getItem(${JSON.stringify(
  CLIENT_STORAGE_KEY,
)})==='desktop'){document.documentElement.setAttribute(${JSON.stringify(
  CLIENT_ATTRIBUTE,
)},'desktop')}}catch(e){}})()`
