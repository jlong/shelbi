import {
  Children,
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
} from "react"

type StepProps = {
  /** Heading rendered next to the step's number. */
  title: string
  /** Prose and code that make up the step body. */
  children?: ReactNode
  /**
   * Injected by {@link Steps}; consumers never pass these. `number` is the
   * 1-based position and `isLast` suppresses the connecting rule on the final
   * step so the sequence terminates cleanly.
   */
  number?: number
  isLast?: boolean
}

/**
 * One item in a {@link Steps} sequence: a numbered marker joined to the next
 * step by a vertical rule, paired with a heading and arbitrary MDX children.
 * Strict-monochrome per `site/AGENTS.md` — order reads from the numeral, the
 * connecting rule, and indentation, never hue.
 */
export function Step({ title, children, number, isLast }: StepProps) {
  return (
    <div className="relative flex gap-3 pb-6 last:pb-0">
      {/* `mt-0.5` drops the marker column so the circle's center lines up with
          the optical center of the headline's first line rather than the row
          top. The rule lives inside this column, so its `top-3` start stays
          pinned just below the circle without adjustment. */}
      <div className="relative mt-0.5 flex w-3 shrink-0 justify-center">
        {/* Connecting rule: runs from just below this marker toward the next
            step's marker. The `-bottom-0.5` cancels the column's `mt-0.5` so the
            reach past this row is unchanged from before the shift; hidden on the
            last step. */}
        {!isLast && (
          <div
            className="absolute top-3 -bottom-0.5 left-1/2 w-px -translate-x-1/2 bg-gray-4"
            aria-hidden="true"
          />
        )}
        <div className="relative z-10 flex h-3 w-3 items-center justify-center rounded-full border border-gray-5 bg-bg font-mono text-xs text-fg">
          {number}
        </div>
      </div>
      <div className="min-w-0 flex-1">
        <h3 className="mt-0.5 mb-1 scroll-mt-11 text-lg font-semibold tracking-tight text-fg">
          {title}
        </h3>
        <div className="text-gray-7">{children}</div>
      </div>
    </div>
  )
}

type StepsProps = {
  children?: ReactNode
}

/**
 * Vertically-numbered, visually-connected sequence for multi-step guides
 * (install → configure → run). Wraps a set of {@link Step} children and
 * injects each one's 1-based number and whether it is the last, so authors
 * write `<Steps><Step title="…">…</Step></Steps>` without tracking indices.
 */
export function Steps({ children }: StepsProps) {
  const steps = Children.toArray(children).filter(isValidElement)
  return (
    <div className="my-4">
      {steps.map((step, index) =>
        cloneElement(step as ReactElement<StepProps>, {
          number: index + 1,
          isLast: index === steps.length - 1,
        }),
      )}
    </div>
  )
}
