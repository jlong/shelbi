import * as React from "react"
import {
  WORDMARK_COLS,
  WORDMARK_PIXEL_ROWS,
  WORDMARK_RECTS,
} from "./Wordmark"

/**
 * Dimensions for the OG card. Twitter and Facebook both render
 * 1.91:1 cards at 1200×630.
 */
export const OG_CARD_SIZE = { width: 1200, height: 630 } as const

const BG = "#000000"
const FG = "#FFFFFF"
const DIM = "#999999"

export interface OgCardProps {
  title: string
  section?: string
}

/**
 * Reusable Open Graph card rendered by `@vercel/og` / `next/og`'s
 * Satori engine. JSX uses inline styles (Satori does not run
 * Tailwind) and `display: flex` on every parent with multiple
 * children, per Satori's requirements.
 *
 * Fonts named "Geist" and "Source Code Pro" must be loaded into the
 * `ImageResponse` `fonts` option by the route that uses this
 * component.
 */
export function OgCard({ title, section }: OgCardProps) {
  const wordmarkPixel = 8
  const wordmarkHeight = WORDMARK_PIXEL_ROWS * wordmarkPixel
  const wordmarkWidth = WORDMARK_COLS * wordmarkPixel

  return (
    <div
      style={{
        width: OG_CARD_SIZE.width,
        height: OG_CARD_SIZE.height,
        background: BG,
        color: FG,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 56,
        padding: 72,
        fontFamily: "Geist",
      }}
    >
      <svg
        width={wordmarkWidth}
        height={wordmarkHeight}
        fill={FG}
        shapeRendering="crispEdges"
        style={{ flexShrink: 0 }}
      >
        {WORDMARK_RECTS.map((r, i) => (
          <rect
            key={i}
            x={r.x * wordmarkPixel}
            y={r.y * wordmarkPixel}
            width={r.w * wordmarkPixel}
            height={wordmarkPixel}
          />
        ))}
      </svg>

      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 24,
          flex: 1,
        }}
      >
        {section ? (
          <div
            style={{
              fontFamily: "Source Code Pro",
              fontSize: 22,
              color: DIM,
              textTransform: "uppercase",
              letterSpacing: "0.14em",
            }}
          >
            {section}
          </div>
        ) : null}
        <div
          style={{
            fontSize: 56,
            fontWeight: 600,
            lineHeight: 1.12,
            letterSpacing: "-0.02em",
            color: FG,
          }}
        >
          {title}
        </div>
        <div
          style={{
            fontFamily: "Source Code Pro",
            fontSize: 26,
            color: DIM,
            marginTop: 8,
          }}
        >
          shelbi.dev
        </div>
      </div>
    </div>
  )
}
