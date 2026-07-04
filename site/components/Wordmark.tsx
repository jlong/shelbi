import * as React from "react"

/**
 * Source of truth for the SHELBI wordmark. Mirrors the half-block ASCII
 * banner printed by `crates/shelbi-cli/src/wizard.rs` (constant `BANNER`)
 * and the top of `README.md`. Keep all three in sync — the brand is the
 * exact same artwork in every surface.
 */
export const BANNER_LINES = [
  "   ▄▀▀▀▀▀▄   ▀▀    ▀▀  ▀▀▀▀▀▀▀   ▀▀   ▀▀▀▀▀▀▀▀▀▀▄   ▀▀▀▀▀",
  "  ▀▀        ▀▀    ▀▀  ▀▀        ▀▀        ▀▀    ▀▀   ▀▀",
  "  ▀▀▀▀▄    ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀    ▀▀      ▀▀▀▀▀▀▀▀▄    ▀▀",
  "▄     ▀▀  ▀▀    ▀▀  ▀▀        ▀▀        ▀▀     ▀▀  ▀▀",
  " ▀▀▀▀▀▀  ▀▀    ▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀",
] as const

type Rect = { x: number; y: number; w: number }

/**
 * Convert the half-block art into 1×1 pixel rectangles. Each character
 * cell is 1 unit wide × 2 units tall: `▀` fills the top half, `▄` fills
 * the bottom. Adjacent same-half runs collapse into wider rects so we
 * emit ~40 rects instead of ~200.
 */
function buildRects(lines: readonly string[], colStart = 0, colEnd?: number): Rect[] {
  const rects: Rect[] = []
  lines.forEach((line, rowIdx) => {
    const chars = [...line]
    const end = colEnd ?? chars.length
    const slice = chars.slice(colStart, end)
    for (const offset of [0, 1] as const) {
      const target = offset === 0 ? "▀" : "▄"
      const y = rowIdx * 2 + offset
      let runStart = -1
      for (let i = 0; i <= slice.length; i++) {
        const filled = slice[i] === target
        if (filled && runStart < 0) runStart = i
        else if (!filled && runStart >= 0) {
          rects.push({ x: runStart, y, w: i - runStart })
          runStart = -1
        }
      }
    }
  })
  return rects
}

export const WORDMARK_COLS = Math.max(...BANNER_LINES.map((l) => [...l].length))
export const WORDMARK_PIXEL_ROWS = BANNER_LINES.length * 2
export const WORDMARK_RECTS = buildRects(BANNER_LINES)

/**
 * Height (px) per size token. The wordmark scales by height; width
 * follows from the 57:10 viewBox aspect via `preserveAspectRatio`.
 *
 * - `sm`  — small nav / footer / inline (28px)
 * - `md`  — default body usage (48px)
 * - `lg`  — section dividers (80px)
 * - `hero` — landing hero (120px)
 */
const WORDMARK_HEIGHTS = {
  sm: 28,
  md: 48,
  lg: 80,
  hero: 120,
} as const

export type WordmarkSize = keyof typeof WORDMARK_HEIGHTS

type SemanticElement = "h1" | "h2" | "h3" | "p" | "div" | "span"

export interface WordmarkProps extends Omit<React.HTMLAttributes<HTMLElement>, "title"> {
  size?: WordmarkSize
  /** Wrapping element. Use `h1`/`h2` to carry semantic weight on a page. */
  as?: SemanticElement
  /** Accessible name. The visible glyphs are the same characters; this
   *  text is what screen readers announce and what SEO indexes. */
  title?: string
}

export function Wordmark({
  size = "md",
  as: Tag = "div",
  title = "Shelbi",
  className,
  ...rest
}: WordmarkProps) {
  const height = WORDMARK_HEIGHTS[size]
  return (
    <Tag className={className} {...rest}>
      <span className="sr-only">{title}</span>
      <svg
        viewBox={`0 0 ${WORDMARK_COLS} ${WORDMARK_PIXEL_ROWS}`}
        height={height}
        width={(height * WORDMARK_COLS) / WORDMARK_PIXEL_ROWS}
        fill="currentColor"
        shapeRendering="crispEdges"
        aria-hidden="true"
        focusable="false"
      >
        {WORDMARK_RECTS.map((r, i) => (
          <rect key={i} x={r.x} y={r.y} width={r.w} height={1} />
        ))}
      </svg>
    </Tag>
  )
}

/**
 * Horizontal divider built from the same pixel grid as the wordmark.
 * A row of equally-spaced solid blocks, fills container width.
 *
 * - `cells` — number of blocks across (default 32).
 * - `height` — px height of each block (default 8 — one wordmark "pixel"
 *   at sm size).
 * - `gapRatio` — gap-to-block width ratio (default 0.5 → gaps are half
 *   a block wide).
 */
export interface BlockDividerProps extends React.HTMLAttributes<HTMLDivElement> {
  cells?: number
  height?: number
  gapRatio?: number
}

export function BlockDivider({
  cells = 32,
  height = 8,
  gapRatio = 0.5,
  className,
  ...rest
}: BlockDividerProps) {
  const block = 1
  const gap = gapRatio
  const total = cells * block + (cells - 1) * gap
  return (
    <div className={className} {...rest}>
      <svg
        viewBox={`0 0 ${total} 1`}
        preserveAspectRatio="none"
        width="100%"
        height={height}
        fill="currentColor"
        shapeRendering="crispEdges"
        aria-hidden="true"
        focusable="false"
      >
        {Array.from({ length: cells }, (_, i) => (
          <rect key={i} x={i * (block + gap)} y={0} width={block} height={1} />
        ))}
      </svg>
    </div>
  )
}

/**
 * Source of truth for the SVG wordmark asset and its native aspect.
 * The actual artwork lives at `public/wordmark.svg`; we reference it
 * here so the Header / Hero stay in sync if the path or art moves.
 *
 * The SVG itself uses `fill="currentColor"`, but `<img>` strips that
 * because the file is rendered as a separate document. We render the
 * mark via a CSS `mask` so the visible color comes from the host
 * element's `background-color: currentColor` — themes keep working.
 */
export const WORDMARK_SVG_PATH = "/wordmark.svg"
export const WORDMARK_SVG_ASPECT = "684 / 108"

export interface WordmarkSvgProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Accessible name for the wordmark (`<span class="sr-only">`). */
  title?: string
}

/**
 * Renders `public/wordmark.svg` as a CSS mask so the visible color
 * comes from the host's text color (theme-aware). Width comes from
 * the SVG's 684:108 aspect ratio applied to whatever `height` /
 * `width` the caller sizes the span to — pass sizing via `className`
 * or `style`, no defaults baked in.
 */
export function WordmarkSvg({
  title = "Shelbi",
  className,
  style,
  ...rest
}: WordmarkSvgProps) {
  return (
    <span
      role="img"
      aria-label={title}
      className={className}
      style={{
        display: "block",
        backgroundColor: "currentColor",
        aspectRatio: WORDMARK_SVG_ASPECT,
        WebkitMaskImage: `url(${WORDMARK_SVG_PATH})`,
        maskImage: `url(${WORDMARK_SVG_PATH})`,
        WebkitMaskRepeat: "no-repeat",
        maskRepeat: "no-repeat",
        WebkitMaskSize: "contain",
        maskSize: "contain",
        WebkitMaskPosition: "center",
        maskPosition: "center",
        ...style,
      }}
      {...rest}
    />
  )
}

/**
 * Square block-character mark. Renders just the "S" letter of the
 * wordmark in a 10×10 viewBox — a reusable favicon-ready glyph. Inherits
 * currentColor like the wordmark.
 */
export interface BlockMarkProps extends React.SVGAttributes<SVGSVGElement> {
  size?: number
  title?: string
}

const BLOCK_MARK_RECTS: Rect[] = [
  { x: 4, y: 0, w: 5 },
  { x: 3, y: 1, w: 1 },
  { x: 9, y: 1, w: 1 },
  { x: 2, y: 2, w: 2 },
  { x: 2, y: 4, w: 4 },
  { x: 6, y: 5, w: 1 },
  { x: 6, y: 6, w: 2 },
  { x: 0, y: 7, w: 1 },
  { x: 1, y: 8, w: 6 },
]

export function BlockMark({ size = 32, title = "Shelbi", ...rest }: BlockMarkProps) {
  return (
    <svg
      viewBox="0 0 10 10"
      width={size}
      height={size}
      fill="currentColor"
      shapeRendering="crispEdges"
      role="img"
      aria-label={title}
      focusable="false"
      {...rest}
    >
      <title>{title}</title>
      {BLOCK_MARK_RECTS.map((r, i) => (
        <rect key={i} x={r.x} y={r.y} width={r.w} height={1} />
      ))}
    </svg>
  )
}
