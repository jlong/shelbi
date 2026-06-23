"use client"

import { useEffect, useState, useCallback } from "react"
import {
  SunIcon,
  MoonIcon,
  ComputerDesktopIcon,
  CheckIcon,
} from "@heroicons/react/24/outline"
import { Menu, MenuTrigger, MenuContent, MenuButton } from "@/components/Menu"

type Preference = "light" | "dark" | "system"

function resolveTheme(pref: Preference): boolean {
  if (pref === "dark") return true
  if (pref === "light") return false
  return matchMedia("(prefers-color-scheme: dark)").matches
}

function applyTheme(dark: boolean) {
  document.documentElement.classList.toggle("dark", dark)
}

const options: { value: Preference; label: string; icon: typeof SunIcon }[] = [
  { value: "light", label: "Light", icon: SunIcon },
  { value: "dark", label: "Dark", icon: MoonIcon },
  { value: "system", label: "System", icon: ComputerDesktopIcon },
]

export function ThemeToggle() {
  const [preference, setPreference] = useState<Preference>("system")
  const [dark, setDark] = useState(true)
  const [mounted, setMounted] = useState(false)

  useEffect(() => {
    const stored = localStorage.getItem("theme") as Preference | null
    const pref =
      stored === "light" || stored === "dark" || stored === "system"
        ? stored
        : "system"
    const resolved = resolveTheme(pref)
    // localStorage and matchMedia are browser-only, so we have to read them
    // post-mount and reconcile our render state. React 19 batches these into
    // one render — the eslint rule warns about the *shape*, not the cost.
    /* eslint-disable react-hooks/set-state-in-effect */
    setPreference(pref)
    setDark(resolved)
    applyTheme(resolved)
    setMounted(true)
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [])

  useEffect(() => {
    if (!mounted) return
    const mq = matchMedia("(prefers-color-scheme: dark)")
    const handler = () => {
      if (preference !== "system") return
      const resolved = mq.matches
      setDark(resolved)
      applyTheme(resolved)
    }
    mq.addEventListener("change", handler)
    return () => mq.removeEventListener("change", handler)
  }, [mounted, preference])

  const select = useCallback((pref: Preference) => {
    const resolved = resolveTheme(pref)
    setPreference(pref)
    setDark(resolved)
    applyTheme(resolved)
    localStorage.setItem("theme", pref)
  }, [])

  // Reserve space identical to the rendered trigger so the surrounding
  // header layout doesn't shift between SSR and hydration.
  if (!mounted) {
    return <div className="w-3 h-3" aria-hidden="true" />
  }

  return (
    <Menu>
      <MenuTrigger>
        <span
          className="w-3 h-3 flex items-center justify-center text-gray-7 hover:text-fg transition-colors"
          aria-label="Theme"
        >
          {dark ? (
            <SunIcon className="w-2.5 h-2.5" />
          ) : (
            <MoonIcon className="w-2.5 h-2.5" />
          )}
        </span>
      </MenuTrigger>
      <MenuContent align="end">
        {options.map((opt) => {
          const Icon = opt.icon
          return (
            <MenuButton key={opt.value} onClick={() => select(opt.value)}>
              <Icon className="w-2 h-2" />
              <span className="flex-1 text-left">{opt.label}</span>
              {preference === opt.value && (
                <CheckIcon className="w-2 h-2 text-fg" />
              )}
            </MenuButton>
          )
        })}
      </MenuContent>
    </Menu>
  )
}
