"use client"

import { Children, isValidElement, useId, useRef } from "react"
import type { KeyboardEvent, ReactElement, ReactNode } from "react"
import type { ClientKind } from "@/lib/client-preference"
import { useCodeTabGroup } from "@/lib/code-tabs-store"
import { useClientPreference } from "./ClientPreference"
import { CopyButton } from "./CopyButton"
import { CopyProvidedContext } from "./CopyProvidedContext"

type CodeTabProps = {
  /** Tab strip label (e.g. `bash`, `zsh`, `brew`). Also the sync key value. */
  label: string
  /**
   * Open on this tab while the site-wide TUI | Desktop switch is on `client`
   * and the reader hasn't picked a tab in this group (e.g. the install guide's
   * "macOS" tab for `tui`, "macOS desktop app" for `desktop`). Without one for
   * `desktop` (the site default), the first tab is its default; without one
   * for `tui`, the TUI opens on the desktop default.
   */
  client?: ClientKind
  children: ReactNode
}

/**
 * A single pane inside {@link CodeTabs}. Purely declarative — {@link CodeTabs}
 * reads its `label` and children directly, so this renders standalone only as a
 * fallback (its highlighted code) if used outside a `CodeTabs` wrapper.
 */
export function CodeTab({ children }: CodeTabProps) {
  return <>{children}</>
}

type CodeTabsProps = {
  /**
   * Sync key. Every `CodeTabs` sharing a group switches together and persists
   * the choice across navigation (e.g. `group="os"`, `group="shell"`). Omit for
   * a standalone, page-local switcher.
   */
  group?: string
  children: ReactNode
}

function isCodeTab(node: ReactNode): node is ReactElement<CodeTabProps> {
  return (
    isValidElement(node) &&
    typeof (node.props as Partial<CodeTabProps>).label === "string"
  )
}

/**
 * Tabbed code blocks with an OS/shell-style switcher, styled to the monochrome
 * docs system. Selection syncs across same-`group` blocks on the page and
 * persists in `localStorage` (see `lib/code-tabs-store.ts`). Each pane keeps its
 * rehype-pretty-code highlighting and a copy button that reads the pane's plain
 * text from the DOM at click time.
 *
 * Until the reader picks a tab, the default follows the site-wide TUI | Desktop
 * switch through each tab's optional `client`. Like `ClientVariant`, that
 * default is drawn by CSS (the `client-tui:` variant), so it is right from
 * the first frame and follows the switch live; the hook only moves the ARIA
 * state and roving tabindex once hydrated.
 *
 * Tabs follow the WAI-ARIA tabs pattern: roving tabindex plus arrow/Home/End
 * keyboard navigation. Registered in `mdx-components.tsx`; authored in MDX as:
 *
 * ```mdx
 * <CodeTabs group="shell">
 *   <CodeTab label="bash">
 *     ```bash
 *     export PATH="$HOME/.shelbi/bin:$PATH"
 *     ```
 *   </CodeTab>
 *   <CodeTab label="fish">
 *     ```fish
 *     fish_add_path "$HOME/.shelbi/bin"
 *     ```
 *   </CodeTab>
 * </CodeTabs>
 * ```
 */
export function CodeTabs({ group, children }: CodeTabsProps) {
  const tabs = Children.toArray(children).filter(isCodeTab)
  const labels = tabs.map((tab) => tab.props.label)

  const [chosen, select] = useCodeTabGroup(group, labels)
  const [client] = useClientPreference()
  const desktopDefault =
    tabs.find((tab) => tab.props.client === "desktop")?.props.label ?? labels[0]
  const tuiDefault = tabs.find((tab) => tab.props.client === "tui")?.props.label ?? desktopDefault
  const active = chosen ?? (client === "tui" ? tuiDefault : desktopDefault)
  // With no pick yet and a per-client default, CSS shows the default tab, so
  // the server render and every frame before hydration already match the
  // chosen client.
  const byClient = chosen === undefined && tuiDefault !== desktopDefault
  const baseId = useId()
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([])
  const paneRefs = useRef<(HTMLDivElement | null)[]>([])

  if (tabs.length === 0) return null

  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let next: number | null = null
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        next = (index + 1) % tabs.length
        break
      case "ArrowLeft":
      case "ArrowUp":
        next = (index - 1 + tabs.length) % tabs.length
        break
      case "Home":
        next = 0
        break
      case "End":
        next = tabs.length - 1
        break
      default:
        return
    }
    event.preventDefault()
    select(labels[next])
    tabRefs.current[next]?.focus()
  }

  return (
    <CopyProvidedContext.Provider value={true}>
      <div className="my-3 overflow-hidden rounded-md border border-gray-4 bg-gray-1">
        <div
          role="tablist"
          aria-orientation="horizontal"
          className="flex overflow-x-auto border-b border-gray-4"
        >
          {tabs.map((tab, index) => {
            const isActive = tab.props.label === active
            return (
              <button
                key={tab.props.label}
                ref={(el) => {
                  tabRefs.current[index] = el
                }}
                type="button"
                role="tab"
                id={`${baseId}-tab-${index}`}
                aria-selected={isActive}
                aria-controls={`${baseId}-panel-${index}`}
                tabIndex={isActive ? 0 : -1}
                onClick={() => select(tab.props.label)}
                onKeyDown={(event) => handleKeyDown(event, index)}
                className={`-mb-px border-b px-3 py-2 font-mono text-xs transition-colors focus:outline-none focus-visible:text-fg ${tabLook(
                  tab.props.label,
                  byClient ? { tuiDefault, desktopDefault } : { active },
                )}`}
              >
                {tab.props.label}
              </button>
            )
          })}
        </div>
        {tabs.map((tab, index) => {
          const isActive = tab.props.label === active
          return (
            <div
              key={tab.props.label}
              role="tabpanel"
              id={`${baseId}-panel-${index}`}
              aria-labelledby={`${baseId}-tab-${index}`}
              hidden={byClient ? undefined : !isActive}
              tabIndex={0}
              className={`relative focus:outline-none ${
                byClient ? paneShownByClient(tab.props.label, tuiDefault, desktopDefault) : ""
              }`}
            >
              {/* Neutralize the MDX `pre`/`figure` chrome so the highlighted code
                  sits flush inside the tab shell instead of nesting a second
                  bordered card. */}
              <div
                ref={(el) => {
                  paneRefs.current[index] = el
                }}
                className="overflow-x-auto [&_figure]:!my-0 [&_pre]:!my-0 [&_pre]:!rounded-none [&_pre]:!border-0"
              >
                {tab.props.children}
              </div>
              <CopyButton getText={() => paneRefs.current[index]?.textContent ?? ""} />
            </div>
          )
        })}
      </div>
    </CopyProvidedContext.Provider>
  )
}

const TAB_ACTIVE = "border-fg text-fg"
const TAB_INACTIVE = "border-transparent text-gray-6 hover:text-fg"

/**
 * A tab's look: active or not by the reader's pick (`active`), or, with no
 * pick, by the chosen client (the desktop default is the base; the two
 * defaults swap under `client-tui:`).
 */
function tabLook(
  label: string,
  by: { active: string } | { tuiDefault: string; desktopDefault: string },
): string {
  if ("active" in by) return label === by.active ? TAB_ACTIVE : TAB_INACTIVE
  if (label === by.desktopDefault) {
    return `${TAB_ACTIVE} client-tui:border-transparent client-tui:text-gray-6 client-tui:hover:text-fg`
  }
  if (label === by.tuiDefault) {
    return `${TAB_INACTIVE} client-tui:border-fg client-tui:text-fg`
  }
  return TAB_INACTIVE
}

/**
 * With no pick, the pane each client opens on is shown by CSS rather than the
 * `hidden` attribute (which preflight forces to `display: none`): the desktop
 * default unless the TUI is chosen, the TUI default only then.
 */
function paneShownByClient(label: string, tuiDefault: string, desktopDefault: string): string {
  if (label === desktopDefault) return "client-tui:hidden"
  if (label === tuiDefault) return "hidden client-tui:block"
  return "hidden"
}
