"use client"

import { BOARD_BEATS, boardHold, boardStateAt } from "../BoardAnimation"
import { HERO_KEYFRAMES, HERO_STATIC_STATE, heroHold } from "../HeroAnimation"
import { useAutoplay } from "../useAutoplay"
import { DesktopAppMockup, desktopScale } from "./DesktopMockup"

/**
 * The Desktop variants of the home page's two animations. Each plays the TUI
 * animation's own timeline (the same keyframes and beats, imported from
 * `HeroAnimation` / `BoardAnimation`), so both clients tell one story, and
 * draws every frame through `DesktopAppMockup` instead of the terminal
 * renderer. Reduced motion and the off-screen pause come from the same
 * `useAutoplay` hook.
 */

// The hero window: wide enough for four full board columns with DONE
// peeking in at the right edge, as the app shows a five-column board at this
// size. One point is 0.75px at most (the window then stands ~640px tall, the
// TUI hero's height), shrinking to fit narrower viewports.
const HERO_W = 1500
const HERO_H = 850

/** The hero story in the desktop app: Orchestrator chat → board → palette → alpha's workspace. */
export function DesktopHeroAnimation() {
  const { index, reduced, ref } = useAutoplay(HERO_KEYFRAMES.length, heroHold)
  const kf = reduced ? { state: HERO_STATIC_STATE, opacity: undefined } : HERO_KEYFRAMES[index]
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      <div
        ref={ref}
        className="mx-auto w-fit max-w-full"
        style={desktopScale({ width: HERO_W, max: 0.75, min: 0.2, gutter: 48 })}
      >
        <DesktopAppMockup
          state={kf.state}
          width={HERO_W}
          height={HERO_H}
          contentOpacity={kf.opacity}
          label="The Shelbi desktop app: the orchestrator breaks a plan into issues, the board dispatches them to workspaces, and a worker's session runs"
        />
      </div>
    </section>
  )
}

// The flow board: sidebar + the collapsed BACKLOG strip + four columns, at
// its full natural width (the value-prop row lets it bleed off the page).
const BOARD_W = 280 + 12 + 24 + 12 + 4 * 272 + 3 * 12 + 12
const BOARD_H = 900

/** Work flowing one card at a time through the desktop app's Issues board. */
export function DesktopBoardAnimation() {
  const { index: beat, reduced, ref } = useAutoplay(BOARD_BEATS, boardHold)
  return (
    <div ref={ref} style={desktopScale({ width: 1600, max: 0.7, min: 0.5, gutter: 0 })}>
      <DesktopAppMockup
        state={boardStateAt(reduced ? 0 : beat)}
        width={BOARD_W}
        height={BOARD_H}
        label="The Shelbi desktop app's Issues board, with work moving from To Do through In Progress and Review to Done"
      />
    </div>
  )
}
