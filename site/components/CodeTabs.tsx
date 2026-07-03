"use client"

import { Children, isValidElement, useId, useRef } from "react"
import type { KeyboardEvent, ReactElement, ReactNode } from "react"
import { useCodeTabGroup } from "@/lib/code-tabs-store"
import { CopyButton } from "./CopyButton"

type CodeTabProps = {
  /** Tab strip label (e.g. `bash`, `zsh`, `brew`). Also the sync key value. */
  label: string
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

  const [active, select] = useCodeTabGroup(group, labels)
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
              className={`-mb-px border-b px-3 py-2 font-mono text-xs transition-colors focus:outline-none focus-visible:text-fg ${
                isActive
                  ? "border-fg text-fg"
                  : "border-transparent text-gray-6 hover:text-fg"
              }`}
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
            hidden={!isActive}
            tabIndex={0}
            className="relative focus:outline-none"
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
  )
}
