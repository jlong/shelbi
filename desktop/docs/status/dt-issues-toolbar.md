# dt-issues-toolbar

The Issues board's toolbar restyled to John's redrawn Figma Header
(`Shelbi`, `pu3hxHgWoCDPCzJ4u6zTQK`; the `Header` instance `2251:26177` in
the dark Issues frame `2165:94`, main component `2251:26070` on the
Components page), in both themes, plus a text search on the board.

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-issues-toolbar/`:
`before/` and `after/` (the renders and `figma-diff.sh`'s `diff/`),
`states/` (the undrawn states), `header-dark-ref-after-before.png` (the dark
toolbar: reference, after, before, stacked) and
`header-light-component-vs-after.png` (the Header component in Light over
the light render).

## Where the values came from

Figma MCP, not the PNGs: `get_design_context` and `get_variable_defs` on
`2251:26177`, then read-only `use_figma` scripts that resolved every bound
variable in the `03 Desktop` collection's Dark and Light modes (the instance
resolves to Dark; the Components page renders Light). Checked against the
1x PNG by pixel samples.

| Piece | Figma | Desktop |
|---|---|---|
| Header | 52pt, auto layout 12 / 16 across, 8 down, gap 8; fill `surface/depth-10` (`#acb4cf` at 10% Dark, 8% Light) over `surface/background`; a 1pt `surface/background` edge on its left; no rule under it | `render_header`: `TOOLBAR_H` 52 (was 57 with its rule), new role `toolbar`, `border_l_1` in `canvas` |
| `Filter:` | Small/Medium 13/18, `text/muted` (`Slate/100` at 55% Dark, `Slate/600` at 50% Light) | `form_hint` (the same pair, already a role) |
| Workflow chip, set | Button, Size=Small, Select=true: 28pt, 12 across, 6pt corners, `button/select/background` (`Cyan/400` at 18% / 12%), label `button/select/text` (`#b0e6fb` / `#1989c4`) | `style::filter_chip(.., selected: true, ..)`, `ButtonKind::Select`, roles `select_fill`, `select_fill_hover` (28/22%), `select_fill_active` (40/32%), `select_label` |
| Workspaces chip, unset | Button, Size=Small, Default style, `button/default/background`, `button/default/text` | `filter_chip(.., selected: false, ..)`: `background`, `text_loud` |
| Searchbox | Searchbox, Size=Small: 180 x 28, 6 across, gap 6, 6pt corners, `button/default/background` (hover `-hover`), the 20pt magnifying glass (`#acb4cf` at 90% Dark, the `icon` role), `Search` in Small/Regular `text/muted` | `style::search_box` + `render_search`: `SEARCH_W` 180, the input with its own look off, an overlaid `Search` placeholder in `form_hint` (gpui-kit draws its placeholder in the kit theme's `muted_foreground`, which is `theme.muted`, not this color) |
| New issue | Button, Small, Default, the 20pt plus in its 12pt slot | unchanged (`toolbar_button`): already the same component |

`button/default/background-active` now has a Dark value in Figma (40%, was
derived as 38%): `BACKGROUND_ACTIVE` dark is `#acb4cf66`. It only shows on a
pressed Default button or an open filter.

The unfiltered labels: Figma draws only the frame's state (`task` set,
Workspaces unset). The workspace chip with no filter reads `Workspaces`, so
the workflow chip with no filter reads `Workflows` in the same Default
style (`state-issues-unfiltered-*.png`). A set filter's chip names it
(`task`, `alpha`, `Unassigned`) in the Select style. This replaces the
funnel icon buttons (`style::dropdown`, `icons::FUNNEL_BUTTON` and its 18pt
drawing are gone; nothing else drew them).

## Search

- `BoardFilters.search` (shared, `shelbi-app`): `IssuesModel::build` keeps
  the cards for which `board::card_matches_search(title, id, branch, query)`
  holds: every whitespace-separated word appears, ignoring case, in the
  title, id or branch. It applies on top of the workflow and workspace
  filters. Desktop only (the TUI board has no search; `parity.md` lists the
  field and a Platform differences entry).
- `/` focuses the field (a desktop board key, like `f`, `w`, `n`: no
  default `keys.yaml` binding uses `/` in any mode, and a user binding in
  the kanban map still wins). Typing narrows the board live; if the
  selected card drops out, the selection moves to the first match.
- Esc in the field clears the search and hands the keyboard back to the
  board; Enter keeps the search and hands the keyboard back (to walk the
  matches with `j`/`k`). Esc on the board with a search active clears it
  too. While the field has focus the board's keys stay out of its way.
- The footer hints are unchanged: the frame's footer doesn't list `/`.
- Focused, the box draws Figma's focus (1pt ring, 4pt glow over it) as
  bands outside it (`focus_bands`): a drop shadow would show through the
  translucent fill as a solid block (`state-issues-search-*.png`).

## Before / after

`cargo run --features visual-check -- --visual-figma <dir>` (new
`SHELBI_VISUAL_ONLY=issues` renders just the board, its states and the
issue detail), then `scripts/figma-diff.sh <dir>`:

| Frame | before | after |
|---|---|---|
| issues-dark | 12.32% | 4.27% |
| issues-light | 3.59% | 8.60% |
| issue-detail-dark | 4.11% | 5.92% |
| issue-detail-light | 5.10% | 5.91% |

**The light and issue-detail rises are stale references, not
regressions.** Only the dark Issues frame was redrawn: `issues-light.png`
and the light Issues frame in Figma (`2220:20975`, still `Board toolbar`
`2220:20978` over a divider) show the old 56pt toolbar and its rule, and
both `issue-detail-*.png` exports predate the new Header (the dark Issue
detail frame in Figma now carries it, `2252:27130`). Against those, the
whole board sits 5pt higher than the reference. Measured directly:

- **Board unchanged**: the board area below the toolbar (1160 x 938) before
  at y 57 and after at y 52 is pixel-identical (`magick compare -metric
  AE`: 0) in both themes. Aligned that way, the light board's diff against
  the reference is the same 7443 pixels before and after.
- **Dark toolbar**: the toolbar strip (1160 x 52 at x 280) against the
  reference: 2.08% of pixels differ (fuzz 10%) before, 0.14% after; what
  remains is glyph edges (Figma's Geist v1.2 vs the bundled 1.7.2, as
  `dt-figma-qa-issues-board` found).
- **Light toolbar**: checked against the Header component rendered in its
  Light mode (`figma-header-component-light.png`; the component sheet has
  placeholder buttons and a segmented control the instance hides): band
  `#f9f9fc` vs render `#f8f9fb`, the Default chip `#f0f1f6` vs `#eff1f6`
  (8% alpha rounds to 0x14), the Select chip's `Cyan/400` tint and
  `#1989c4` label, placeholder and icon colors as drawn.
- **Issue detail**: the modal itself is pixel-identical before and after
  in both themes (the board behind it moved up 5pt and shows the new
  Header, as the current Figma frame does).

Flagged for John: re-export `issues-light.png` (and the issue-detail PNGs)
once the light Issues frame gets the Header, and the light numbers will
follow the dark ones.

## Undrawn states (checked by eye)

- `state-issues-unfiltered`: `Workflows` and `Workspaces`, both Default.
- `state-issues-search`: `/` then `terminal` typed through real keystrokes;
  the board narrows to its two cards (harness check), the box focused.
- `--visual-views`: `board-dropdown` / `board-workspace-dropdown` (the
  chip pressed, the menu 6pt under it), `board-empty` (`alpha` in the
  Select style), `board-drag` (released over the toolbar, nothing moves).

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings
  && cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test`: clean, 510 tests pass.
- Root: `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean (root `Cargo.lock` unchanged); `cargo test -p shelbi-app` passes,
  `parity_doc` included.
- New tests: `board::tests::search_matches_every_word_in_title_id_or_branch_ignoring_case`,
  `view::tests::issues_board_search_narrows_cards_on_top_of_the_other_filters`
  (shelbi-app); `board_state::tests::search_narrows_the_cards_and_combines_with_the_filters`,
  `board_state::tests::slash_focuses_the_search_and_esc_clears_it`
  (desktop). The view's wiring (`/`, typing, the board narrowing) is
  checked end to end by the `--visual-figma` harness through simulated
  keystrokes.
- `--visual-figma` (issues, both themes) and `--visual-views`: 0 checks
  failed. Keyboard navigation, drag and drop, the detail modal and New
  issue: their tests pass and their renders are unchanged apart from the
  5pt shift.
