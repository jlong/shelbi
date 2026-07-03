import type { ReactNode } from "react"
import {
  ExclamationTriangleIcon,
  InformationCircleIcon,
  LightBulbIcon,
  PencilSquareIcon,
} from "@heroicons/react/24/outline"

type CalloutType = "note" | "warning" | "info" | "tip"

type CalloutProps = {
  /** Admonition flavor. Governs the icon, left-rule weight, and gray ramp. */
  type?: CalloutType
  /** Optional heading; falls back to the type's default label. */
  title?: string
  /** Body content — arbitrary MDX. */
  children: ReactNode
}

/**
 * Docs admonition (Note / Warning / Info / Tip) for gotchas and critical rules
 * — especially around agent orchestration and destructive commands.
 *
 * Strict-monochrome per `site/AGENTS.md`: variants are distinguished WITHOUT
 * hue. Each carries a distinct icon, a left-rule weight that scales with
 * urgency (note → warning), and a position on the gray ramp for the icon and
 * title. Colors resolve through `var()` tokens, so light and dark mode follow
 * automatically.
 */
const VARIANTS: Record<
  CalloutType,
  {
    label: string
    Icon: typeof InformationCircleIcon
    /** Left-rule weight — heavier as urgency climbs. */
    rule: string
    /** Icon + title shade — darker (closer to `fg`) as urgency climbs. */
    accent: string
  }
> = {
  note: {
    label: "Note",
    Icon: PencilSquareIcon,
    rule: "border-l-2 border-gray-4",
    accent: "text-gray-6",
  },
  info: {
    label: "Info",
    Icon: InformationCircleIcon,
    rule: "border-l-2 border-gray-5",
    accent: "text-gray-6",
  },
  tip: {
    label: "Tip",
    Icon: LightBulbIcon,
    rule: "border-l-4 border-gray-5",
    accent: "text-gray-7",
  },
  warning: {
    label: "Warning",
    Icon: ExclamationTriangleIcon,
    rule: "border-l-4 border-gray-7",
    accent: "text-fg",
  },
}

export function Callout({ type = "note", title, children }: CalloutProps) {
  const { label, Icon, rule, accent } = VARIANTS[type]
  const heading = title ?? label

  return (
    <div
      role="note"
      className={`my-3 flex gap-2 rounded-md border border-gray-4 ${rule} bg-gray-1 p-3`}
    >
      <Icon className={`mt-0.5 h-3 w-3 shrink-0 ${accent}`} aria-hidden="true" />
      <div className="min-w-0 space-y-1">
        <p className={`text-sm font-semibold tracking-wide ${accent}`}>
          {heading}
        </p>
        <div className="space-y-2 text-sm leading-relaxed text-gray-7 [&>:first-child]:mt-0 [&>:last-child]:mb-0">
          {children}
        </div>
      </div>
    </div>
  )
}
