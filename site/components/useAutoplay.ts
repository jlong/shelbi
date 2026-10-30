"use client"

import { useEffect, useRef, useState } from "react"

/**
 * The looping-timeline plumbing the home page's animations share (the hero
 * story and the board flow, in both their TUI and Desktop variants): step
 * through `length` frames, holding each for `holdFor(index)` ms, and wrap.
 *
 * - `prefers-reduced-motion: reduce` stops the loop (`reduced` is true and
 *   the caller renders a single static frame, with no timers);
 * - an IntersectionObserver on `ref` pauses it while the element is
 *   off-screen, which also covers a `display: none` ancestor (a
 *   `ClientVariant` that isn't showing), so only the visible client animates.
 */
export function useAutoplay(length: number, holdFor: (index: number) => number) {
  const [index, setIndex] = useState(0)
  const [visible, setVisible] = useState(true)
  const [reduced, setReduced] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)")
    const update = () => setReduced(mq.matches)
    update()
    mq.addEventListener("change", update)
    return () => mq.removeEventListener("change", update)
  }, [])

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const obs = new IntersectionObserver(([entry]) => setVisible(entry.isIntersecting), {
      threshold: 0.15,
    })
    obs.observe(el)
    return () => obs.disconnect()
  }, [])

  // Schedule the next frame after the current hold; pausing (cleanup clears
  // the pending timer) and resuming fall out of the dependencies.
  useEffect(() => {
    if (reduced || !visible) return
    const t = window.setTimeout(() => setIndex((i) => (i + 1) % length), holdFor(index))
    return () => window.clearTimeout(t)
  }, [index, visible, reduced, length, holdFor])

  return { index, reduced, ref }
}
