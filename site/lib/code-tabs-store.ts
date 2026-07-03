/**
 * Cross-block selection store for {@link CodeTabs}. Every tab set that shares a
 * `group` key (e.g. `"os"`, `"shell"`) tracks a single selected label, so
 * choosing "zsh" in one block switches every other `shell`-group block on the
 * page at once. The choice is mirrored to `localStorage` so it survives
 * navigation, and a `storage` listener keeps sibling browser tabs in sync.
 *
 * Consumed through {@link useCodeTabGroup} via `useSyncExternalStore`; the
 * module-level `Map` is the external source of truth React subscribes to.
 */
import { useCallback, useId, useSyncExternalStore } from "react"

const STORAGE_PREFIX = "shelbi:codetabs:"

// group key → selected label
const selections = new Map<string, string>()
// group key → set of React-supplied re-render callbacks
const listeners = new Map<string, Set<() => void>>()

let storageBound = false

function notify(key: string) {
  listeners.get(key)?.forEach((cb) => cb())
}

/** Lazily pull a group's persisted label into the in-memory map (client only). */
function hydrate(key: string) {
  if (selections.has(key) || typeof window === "undefined") return
  try {
    const stored = window.localStorage.getItem(STORAGE_PREFIX + key)
    if (stored) selections.set(key, stored)
  } catch {
    // localStorage unavailable (private mode, disabled) — stay in-memory.
  }
}

/** Mirror `storage` events so selecting a tab in one browser tab updates others. */
function bindStorage() {
  if (storageBound || typeof window === "undefined") return
  storageBound = true
  window.addEventListener("storage", (event) => {
    if (!event.key?.startsWith(STORAGE_PREFIX) || event.newValue == null) return
    const key = event.key.slice(STORAGE_PREFIX.length)
    selections.set(key, event.newValue)
    notify(key)
  })
}

function subscribe(key: string, callback: () => void) {
  hydrate(key)
  bindStorage()
  let set = listeners.get(key)
  if (!set) {
    set = new Set()
    listeners.set(key, set)
  }
  set.add(callback)
  return () => {
    set.delete(callback)
  }
}

function getSnapshot(key: string) {
  return selections.get(key)
}

function setSelection(key: string, label: string, persist: boolean) {
  selections.set(key, label)
  if (persist && typeof window !== "undefined") {
    try {
      window.localStorage.setItem(STORAGE_PREFIX + key, label)
    } catch {
      // Persistence is best-effort; the in-memory selection still applies.
    }
  }
  notify(key)
}

/**
 * Returns the active label for a tab set and a setter that fans the choice out
 * to every same-group block. `group` is optional: ungrouped tab sets fall back
 * to a per-instance id so they still switch, but skip cross-block sync and
 * persistence (a random id would never round-trip through `localStorage`).
 *
 * `labels` scopes the selection to what this block actually offers — if the
 * persisted label isn't present here, this block shows its first tab without
 * clobbering the shared choice.
 */
export function useCodeTabGroup(
  group: string | undefined,
  labels: string[],
): [string, (label: string) => void] {
  const fallbackId = useId()
  const key = group ?? fallbackId
  const persist = group != null

  const selected = useSyncExternalStore(
    (cb) => subscribe(key, cb),
    () => getSnapshot(key),
    () => undefined,
  )

  const active =
    selected && labels.includes(selected) ? selected : (labels[0] ?? "")

  const select = useCallback(
    (label: string) => setSelection(key, label, persist),
    [key, persist],
  )

  return [active, select]
}
