# rt-make-the-sidebar-search-box-match-the-figma-design-taller-visible-fill-padding

Status: **ready for review**

Restyled the single-process TUI sidebar search box to match the Figma
`2072:1168` design: a 2-row-tall box drawn with the nav selection bar's
half-block bleed (`▄` / label / `▀`), half-row blank margins above and below,
a `🔍 Search` label with a right-aligned `⌃P`-style palette chord, and a fill
that reads clearly on dark terminal backgrounds.

Notes:
- `SEARCH_BG` nudged from the Figma `color/search` #292929 to #333333 so the
  box stays visibly lighter than a ~#1c1c1c terminal default (Ghostty) while
  remaining a quieter fill than the #3f3f3f selection highlight. The design
  gives search and selection distinct tokens, so they stay distinct.
- `HEADER_H` 4 → 6 (title, margin, 3 box rows, margin); nav and all click
  targets below shift down accordingly, driven by the shared geometry.
- Palette chord already formatted `⌃P` via `DisplayStyle::Mac`; left intact.
- Only Rust source touched (no shipped template/config), so no config-upgrade
  sniffer is required.
