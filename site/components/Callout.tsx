import type { ReactNode } from "react"
import {
  ExclamationCircleIcon,
  ExclamationTriangleIcon,
  InformationCircleIcon,
  LightBulbIcon,
  PencilSquareIcon,
} from "@heroicons/react/24/outline"

type CalloutType =
  | "note"
  | "info"
  | "warning"
  | "danger"
  | "error"
  | "tip"

type CalloutProps = {
  /** Admonition flavor. Governs the icon, hue, and title label. */
  type?: CalloutType
  /** Optional heading; falls back to the type's default label. */
  title?: string
  /** Body content — arbitrary MDX. */
  children: ReactNode
}

/**
 * Docs admonition (Note / Info / Warning / Danger / Tip) for gotchas and
 * critical rules — especially around agent orchestration and destructive
 * commands.
 *
 * Deliberate exception to the strict-monochrome rule in `site/AGENTS.md`:
 * callouts carry hue BY TYPE so readers can triage flavor at a glance —
 * note/info → blue, warning → yellow, danger/error → red, tip → green. Each
 * variant pairs a distinct icon with a tinted background, saturated border,
 * and readable title/body text. Colors resolve through `var()` tokens defined
 * in `app/globals.css`, so light and dark mode follow automatically.
 */
const VARIANTS: Record<
  CalloutType,
  {
    label: string
    Icon: typeof InformationCircleIcon
    /** Container tint + border + left rule (all one hue). */
    box: string
    /** Icon + title shade. */
    title: string
    /** Body text shade. */
    body: string
  }
> = {
  note: {
    label: "Note",
    Icon: PencilSquareIcon,
    box: "border-callout-info-border bg-callout-info-bg",
    title: "text-callout-info-title",
    body: "text-callout-info-body",
  },
  info: {
    label: "Info",
    Icon: InformationCircleIcon,
    box: "border-callout-info-border bg-callout-info-bg",
    title: "text-callout-info-title",
    body: "text-callout-info-body",
  },
  tip: {
    label: "Tip",
    Icon: LightBulbIcon,
    box: "border-callout-tip-border bg-callout-tip-bg",
    title: "text-callout-tip-title",
    body: "text-callout-tip-body",
  },
  warning: {
    label: "Warning",
    Icon: ExclamationTriangleIcon,
    box: "border-callout-warning-border bg-callout-warning-bg",
    title: "text-callout-warning-title",
    body: "text-callout-warning-body",
  },
  danger: {
    label: "Danger",
    Icon: ExclamationCircleIcon,
    box: "border-callout-danger-border bg-callout-danger-bg",
    title: "text-callout-danger-title",
    body: "text-callout-danger-body",
  },
  error: {
    label: "Error",
    Icon: ExclamationCircleIcon,
    box: "border-callout-danger-border bg-callout-danger-bg",
    title: "text-callout-danger-title",
    body: "text-callout-danger-body",
  },
}

export function Callout({ type = "note", title, children }: CalloutProps) {
  const { label, Icon, box, title: titleColor, body } = VARIANTS[type]
  const heading = title ?? label

  return (
    <div
      role="note"
      className={`my-3 flex gap-2 rounded-md border border-l-4 ${box} p-3`}
    >
      <Icon
        className={`mt-0.5 h-3 w-3 shrink-0 ${titleColor}`}
        aria-hidden="true"
      />
      <div className="min-w-0 space-y-1">
        <p className={`text-sm font-semibold tracking-wide ${titleColor}`}>
          {heading}
        </p>
        <div
          className={`space-y-2 text-sm leading-relaxed ${body} [&>:first-child]:mt-0 [&>:last-child]:mb-0 [&_li]:text-inherit [&_ol]:text-inherit [&_p]:text-inherit [&_strong]:text-inherit [&_ul]:text-inherit`}
        >
          {children}
        </div>
      </div>
    </div>
  )
}
