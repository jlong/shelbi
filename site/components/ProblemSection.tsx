/**
 * The homepage "problem" beat: the mess of terminal tabs, one agent per tab,
 * you as the router. The reader is a developer already living this, so the
 * section only names what they recognize: no argument that parallel agents are
 * good, no reassurance. Five beats, each a bold lead-in plus its copy, rendered
 * as a hairline-divided numbered list so it reads terminal-flavored and
 * restrained, matching the site's `gap-px bg-gray-4` grid language.
 */

const beats: { lead: string; body: string }[] = [
  {
    lead: "Replied to the wrong tab.",
    body: "You typed instructions for one agent into another one's session.",
  },
  {
    lead: "The forgotten paused agent.",
    body: "An agent asked you a question two hours ago. It's still waiting, in a tab you forgot.",
  },
  {
    lead: "You are the scheduler.",
    body: "Who's done? Who's stuck? Who needs you? You've become a human scheduler for your own tools.",
  },
  {
    lead: "The backlog lives in your head.",
    body: "New work occurs to you while every agent is mid-task. Interrupting one derails it, so you carry the idea instead.",
  },
  {
    lead: "The review queue is you.",
    body: "Every new agent makes the pile taller, and you trust it less.",
  },
]

export function ProblemSection() {
  return (
    <section className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <h2 className="max-w-3xl font-sans text-4xl font-semibold tracking-tight text-fg">
          One agent per tab. You&apos;re the router.
        </h2>

        <ol className="mt-6 max-w-3xl border-t border-gray-4">
          {beats.map((beat, i) => (
            <li
              key={beat.lead}
              className="grid grid-cols-[2rem_1fr] items-baseline gap-4 border-b border-gray-4 py-4 sm:gap-6"
            >
              <span
                className="font-mono text-sm tabular-nums text-gray-6"
                aria-hidden="true"
              >
                {String(i + 1).padStart(2, "0")}
              </span>
              <p className="text-base leading-relaxed">
                <span className="font-semibold text-fg">{beat.lead}</span>{" "}
                <span className="text-gray-7">{beat.body}</span>
              </p>
            </li>
          ))}
        </ol>
      </div>
    </section>
  )
}
