# pane-side-props-stacked

A follow-up to [`pane-responsive-props`](pane-responsive-props.md). The
side column a wide task pane puts its properties in has height to spare
but little width, and it kept the frame's label-beside-value rows with an
84pt label column. That left the values 144pt, which cut off the branch
and the PR line's state and CI.

## What landed

- **Label over value in the split layout** (`task_detail::property_at`).
  Each property is its label on its own line, then its value, both the
  column's full content width (239pt). The label sits 8pt in, on the
  values' text edge, 2pt over its value (`PROPERTY_OVER_GAP`), and
  properties sit 12pt apart, so each pair reads as one. The stacked layout
  under the title keeps the frame's rows and their 6pt gaps.
- **The pieces** (`floating_pane`). `property_label`, `property_value`,
  `property_beside` (the frame's row; `property_row` for the job and
  reminder sheets is this) and `property_over`. `property_row_at` and
  `SIDE_LABEL_W` are gone.
- **Values use the width.** The Owner and Depends on values wrap in the
  column (`wrapping_value_text`). The PR line wraps its CI under the number
  and state when they don't fit on one line together. The branch still
  truncates if it's too long even for the full width: it's selectable, and
  More copies it.
- **Selectors.** Every property has `pane-row-<slug>`, `pane-label-<slug>`
  and `pane-value-<slug>` (slugs: `status`, `workflow`, `priority`,
  `estimate`, `branch`, `owner`, `deps`, `pr`).
- Pickers, Tab order and the keyboard property focus don't change. The
  pickers anchor under their buttons as before.

## Tests

- `issues::tests::the_side_column_puts_each_label_over_a_full_width_value`:
  for a task with a branch, a dependency and a merged PR with passing CI,
  the stacked rows (label in its 108pt column, level with its value, 6pt
  apart), then in the column (each label over its value, both the column's
  width, pairs further apart than a label and its value), the branch and the
  PR's state and CI as wide as they draw stacked, each picker's button under
  its label, and stacked again after.
- The visual check's `state-task-pane-wide` shot shows the new column.
