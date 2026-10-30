import type { CSSProperties } from "react"

/**
 * The desktop app's own drawings, copied verbatim from `desktop/assets/icons`
 * so the mockups draw exactly what the app draws: the board's status glyphs
 * (`status/*.svg`), the sidebar's 12pt state glyphs (`glyphs/*.svg`), and the
 * Figma icons that aren't Heroicons (`figma/*.svg`). Every one paints in
 * `currentColor`. Heroicons (the app's vendored 24/outline set, drawn at
 * 20pt) come straight from `@heroicons/react/24/outline`.
 */

type IconProps = { size: string; color?: string; style?: CSSProperties }

function frame({ size, color, style }: IconProps): CSSProperties {
  return { width: size, height: size, flex: "none", color, ...style }
}

// ── Status glyphs (16pt; `assets/icons/status/`) ──────────────────────
const RING =
  "M15.2 8C15.2 11.9764 11.9764 15.2 8 15.2C4.02355 15.2 0.8 11.9764 0.8 8C0.8 4.02355 4.02355 0.8 8 0.8C11.9764 0.8 15.2 4.02355 15.2 8Z"

export type StatusKind = "backlog" | "todo" | "in-progress" | "review" | "done" | "canceled"

export function StatusGlyph({ kind, ...props }: IconProps & { kind: StatusKind }) {
  const stroke = { stroke: "currentColor", strokeWidth: 1.5, strokeLinecap: "round", strokeLinejoin: "round" } as const
  return (
    <svg viewBox="0 0 16 16" fill="none" aria-hidden="true" style={frame(props)}>
      {kind === "backlog" ? (
        <path
          d="M15.1124 6.8733C15.17 7.24039 15.2 7.6167 15.2 8C15.2 8.3833 15.17 8.75961 15.1124 9.1267M13.8254 3.76771C13.381 3.15709 12.8429 2.61902 12.2323 2.17462M9.1267 0.887643C8.75961 0.829952 8.3833 0.8 8 0.8C7.6167 0.8 7.24039 0.829952 6.8733 0.887642M3.76771 2.17462C3.15709 2.61902 2.61902 3.15709 2.17462 3.76771M0.887643 6.8733C0.829952 7.24039 0.8 7.6167 0.8 8C0.8 8.3833 0.829952 8.75961 0.887642 9.1267M2.17462 12.2323C2.61902 12.8429 3.15709 13.381 3.76771 13.8254M6.8733 15.1124C7.24039 15.17 7.6167 15.2 8 15.2C8.3833 15.2 8.75961 15.17 9.1267 15.1124M12.2323 13.8254C12.8429 13.381 13.381 12.8429 13.8254 12.2323"
          {...stroke}
        />
      ) : (
        <path d={RING} {...stroke} />
      )}
      {kind === "in-progress" && (
        <path
          d="M8 13.6C9.48521 13.6 10.9096 13.01 11.9598 11.9598C13.01 10.9096 13.6 9.48521 13.6 8C13.6 6.51479 13.01 5.09041 11.9598 4.0402C10.9096 2.99 9.48521 2.4 8 2.4L8 8L8 13.6Z"
          fill="currentColor"
        />
      )}
      {kind === "review" && (
        <path
          d="M2.4 8C2.4 9.10757 2.72843 10.1903 3.34377 11.1112C3.95911 12.0321 4.83371 12.7499 5.85697 13.1737C6.88024 13.5976 8.00621 13.7085 9.09251 13.4924C10.1788 13.2763 11.1766 12.743 11.9598 11.9598C12.743 11.1766 13.2763 10.1788 13.4924 9.09251C13.7085 8.00621 13.5976 6.88024 13.1737 5.85697C12.7499 4.83371 12.0321 3.95911 11.1112 3.34377C10.1903 2.72843 9.10758 2.4 8 2.4L8 8L2.4 8Z"
          fill="currentColor"
        />
      )}
      {kind === "done" && <circle cx="8" cy="8" r="5.6" fill="currentColor" />}
      {kind === "canceled" && <path d="M5.2 10.8L10.8 5.2" {...stroke} />}
    </svg>
  )
}

// ── Sidebar state glyphs (12pt; `assets/icons/glyphs/`) ───────────────
export type SmallGlyphKind = "play" | "check" | "dot" | "question" | "chevron-down"

export function SmallGlyph({ kind, ...props }: IconProps & { kind: SmallGlyphKind }) {
  const style = frame(props)
  switch (kind) {
    case "play":
      return (
        <svg viewBox="0 0 12 12" fill="currentColor" aria-hidden="true" style={style}>
          <path d="M3.47038 1.64214C2.83181 1.25885 2 1.69577 2 2.4145V9.5855C2 10.3042 2.83181 10.7412 3.47038 10.3579L10.5094 6.77235C11.1635 6.45648 11.1635 5.54345 10.5094 5.22765L3.47038 1.64214Z" />
        </svg>
      )
    case "check":
      return (
        <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth={2.5} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}>
          <path d="M3 5.99077L5.38333 8.4L9.6 3" />
        </svg>
      )
    case "dot":
      return (
        <svg viewBox="0 0 12 12" fill="currentColor" aria-hidden="true" style={style}>
          <circle cx="6" cy="6" r="2" />
        </svg>
      )
    case "question":
      return (
        <svg viewBox="0 0 12 12" fill="currentColor" aria-hidden="true" style={style}>
          <path
            fillRule="evenodd"
            clipRule="evenodd"
            d="M12 6C12 9.31371 9.31371 12 6 12C2.68629 12 0 9.31371 0 6C0 2.68629 2.68629 0 6 0C9.31371 0 12 2.68629 12 6ZM5.2045 3.70448C4.98483 3.92416 4.62868 3.92416 4.40901 3.70448C4.18934 3.48482 4.18934 3.12866 4.40901 2.90899C5.28769 2.03031 6.71231 2.03031 7.59099 2.90899C8.46967 3.78767 8.46967 5.21229 7.59099 6.09097C7.29663 6.38533 6.93964 6.58172 6.5625 6.67881V6.93748C6.5625 7.24814 6.31066 7.49998 6 7.49998C5.68934 7.49998 5.4375 7.24814 5.4375 6.93748V6.56248C5.4375 6.02226 5.86496 5.68369 6.24851 5.59741C6.44867 5.55239 6.6387 5.45227 6.7955 5.29548C7.23483 4.85614 7.23483 4.14382 6.7955 3.70448C6.35616 3.26515 5.64384 3.26515 5.2045 3.70448ZM6 9.75C6.41421 9.75 6.75 9.41421 6.75 9C6.75 8.58579 6.41421 8.25 6 8.25C5.58579 8.25 5.25 8.58579 5.25 9C5.25 9.41421 5.58579 9.75 6 9.75Z"
          />
        </svg>
      )
    case "chevron-down":
      return (
        <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth={1.25} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}>
          <path d="M9.75 4.125L6 7.875L2.25 4.125" />
        </svg>
      )
  }
}

// ── Figma icons (20pt grid; `assets/icons/figma/`) ────────────────────
export type FigmaIconKind = "sidebar" | "toggle-off" | "toggle-on" | "page-curl" | "flag-finish" | "chevron-down-small"

export function FigmaIcon({ kind, ...props }: IconProps & { kind: FigmaIconKind }) {
  const style = frame(props)
  const stroke = { fill: "none", stroke: "currentColor", strokeWidth: 1.25, strokeLinecap: "round", strokeLinejoin: "round" } as const
  switch (kind) {
    case "sidebar":
      return (
        <svg viewBox="0 0 20 20" aria-hidden="true" style={style}>
          <path
            {...stroke}
            d="M7.5 3.75H3.75C2.71447 3.75 1.875 4.58947 1.875 5.625V14.375C1.875 15.4105 2.71447 16.25 3.75 16.25H7.5M7.5 3.75H16.25C17.2855 3.75 18.125 4.58947 18.125 5.625V14.375C18.125 15.4105 17.2855 16.25 16.25 16.25H7.5M7.5 3.75V16.25"
          />
        </svg>
      )
    case "toggle-off":
    case "toggle-on":
      return (
        <svg viewBox="0 0 20 20" aria-hidden="true" style={style}>
          <rect {...stroke} x="1.625" y="5.625" width="16.75" height="9.75" rx="4.875" />
          <circle fill="currentColor" cx={kind === "toggle-on" ? 13.5 : 6.5} cy="10.5" r="3.5" />
        </svg>
      )
    case "page-curl":
      return (
        <svg viewBox="0 0 20 20" aria-hidden="true" style={style}>
          <path
            {...stroke}
            d="M15 14V3C15 2.44772 14.5523 2 14 2H3C2.44772 2 2 2.44772 2 3V16C2 17.1046 2.89543 18 4 18M15 14H6.99981C6.44753 14 5.99981 14.4477 5.99981 15V16.0002C5.99981 17.1047 5.10447 18 4 18M15 14H16.9998C17.5521 14 17.9998 14.4477 17.9998 15V16C17.9998 17.1046 17.1044 18 15.9998 18H4M5 6H12M5 10H12"
          />
        </svg>
      )
    case "flag-finish":
      return (
        <svg viewBox="0 0 20 20" aria-hidden="true" style={style}>
          <path
            fill="currentColor"
            d="M2.5 1.25C2.91421 1.25 3.25 1.58579 3.25 2V2.49121L4.59277 2.08887C5.03171 1.95719 5.4773 1.8693 5.9248 1.82422C7.4562 1.66995 8.99542 2.0227 10.3672 2.8457L10.3857 2.85645L10.457 2.89941C11.5475 3.55371 12.7477 3.8287 13.9307 3.71973C14.2678 3.68864 14.6054 3.62655 14.9395 3.53223L17.5303 2.7998C17.7678 2.73273 18.0239 2.78766 18.2139 2.94531C18.4036 3.10298 18.5036 3.34417 18.4814 3.58984C18.3769 4.74859 18.3071 5.92 18.2734 7.10254C18.2579 7.64925 18.25 8.19869 18.25 8.75C18.25 9.35529 18.2596 9.95789 18.2783 10.5576C18.314 11.7002 18.3834 12.8329 18.4854 13.9531C18.518 14.3127 18.2899 14.645 17.9424 14.7432L15.3467 15.4756C14.9251 15.5946 14.498 15.6733 14.0693 15.7129C12.5565 15.8524 11.0389 15.4976 9.68555 14.6855L9.61426 14.6436L9.5957 14.6318C8.49038 13.9687 7.27293 13.6958 6.0752 13.8164C5.7234 13.8518 5.37139 13.921 5.02344 14.0254L3.25 14.5576V18C3.25 18.4142 2.91421 18.75 2.5 18.75C2.08579 18.75 1.75 18.4142 1.75 18V2C1.75 1.58579 2.08579 1.25 2.5 1.25ZM6 3.3252C5.67329 3.36258 5.34658 3.42845 5.02344 3.52539L3.25 4.05762V6.77441L4.80859 6.30762C5.202 6.1896 5.60039 6.11057 6 6.07031V9.57129C5.60041 9.61154 5.20198 9.69059 4.80859 9.80859L3.25 10.2754V12.9912L4.59277 12.5889C5.03171 12.4572 5.4773 12.3693 5.9248 12.3242C5.94986 12.3217 5.97494 12.3196 6 12.3174V9.57129C7.3646 9.43383 8.74288 9.7471 9.98145 10.4902L10 10.501V13.1396C10.1235 13.2049 10.2462 13.2731 10.3672 13.3457L10.3857 13.3564L10.457 13.3994C11.5475 14.0537 12.7477 14.3287 13.9307 14.2197C13.9538 14.2176 13.9768 14.2134 14 14.2109V11.4678C14.3831 11.4325 14.7656 11.3616 15.1436 11.2549L16.7861 10.79C16.784 10.7282 16.7812 10.6664 16.7793 10.6045C16.7601 9.98915 16.75 9.37092 16.75 8.75C16.75 8.26328 16.7566 7.77824 16.7686 7.29492L15.1436 7.75391C14.7656 7.86062 14.3831 7.93148 14 7.9668V5.21777C12.626 5.33077 11.2511 5.03604 10 4.3623V7L9.98145 6.98926C8.74287 6.24611 7.36462 5.93285 6 6.07031V3.3252ZM10.0713 7.04297C11.2933 7.77615 12.6521 8.09108 14 7.9668V11.4678C12.652 11.5921 11.2933 11.2772 10.0713 10.5439L10 10.501V7L10.0713 7.04297Z"
          />
        </svg>
      )
    case "chevron-down-small":
      return (
        <svg viewBox="0 0 20 20" aria-hidden="true" style={style}>
          <path {...stroke} d="M5.5968 7.725L10.0001 12.1283L14.4033 7.725" />
        </svg>
      )
  }
}
