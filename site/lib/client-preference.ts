/**
 * Which Shelbi client the site's illustrations show: the native desktop app or
 * the terminal app (TUI). One site-wide choice, stored in localStorage and
 * mirrored onto `<html data-client="…">` so CSS can swap variants before
 * React hydrates (no flash of the wrong client, no hydration mismatch).
 *
 * The attribute is always present and always names a client: the root layout
 * server-renders `data-client="desktop"` (the default), and the boot script
 * rewrites it to the stored choice before first paint. CSS keys both variants
 * explicitly (`[data-client="tui"]` / `[data-client="desktop"]`), so neither
 * client is "no attribute".
 *
 * This module is server-safe (no hooks, no `window` at import time): the root
 * layout imports {@link CLIENT_BOOT_SCRIPT} to run it in `<head>`. The React
 * side lives in `components/ClientPreference.tsx` (the `useClientPreference`
 * hook), `components/ClientSwitch.tsx` (the TUI | Desktop button tabs) and
 * `components/ClientVariant.tsx` (render one child per client).
 */

export type ClientKind = "tui" | "desktop"

/** Every client, in the switch's order. Desktop is the default. */
export const CLIENTS: { value: ClientKind; label: string }[] = [
  { value: "tui", label: "TUI" },
  { value: "desktop", label: "Desktop" },
]

/** The client shown with no stored choice (first visit, storage blocked). */
export const DEFAULT_CLIENT: ClientKind = "desktop"

/** The localStorage key holding the choice. */
export const CLIENT_STORAGE_KEY = "shelbi-client"

/**
 * The `<html>` attribute CSS keys off. Always set: server-rendered as
 * {@link DEFAULT_CLIENT}, then rewritten to the stored choice.
 */
export const CLIENT_ATTRIBUTE = "data-client"

/** Read a stored value back as a client, falling back to the default. */
export function parseClient(value: string | null | undefined): ClientKind {
  return value === "tui" || value === "desktop" ? value : DEFAULT_CLIENT
}

/**
 * Inline boot script for `<head>`: runs before first paint and rewrites
 * `<html data-client>` (server-rendered as the default, desktop) to the stored
 * choice, so the CSS-driven variants (see the `client-desktop` / `client-tui`
 * variants in `app/globals.css`) paint the right client from the first frame.
 * With no stored choice (or storage blocked) it leaves the server's default.
 */
export const CLIENT_BOOT_SCRIPT = `(function(){try{var c=localStorage.getItem(${JSON.stringify(
  CLIENT_STORAGE_KEY,
)});if(c==='tui'||c==='desktop'){document.documentElement.setAttribute(${JSON.stringify(
  CLIENT_ATTRIBUTE,
)},c)}}catch(e){}})()`
