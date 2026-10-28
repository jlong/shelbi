# tui-feedback-dialog: Contact Us in the TUI

## The change

The TUI's footer `📣` and the palette's Send Feedback only said "Contact Us is
in the desktop app". They now open the desktop's Contact Us dialog, drawn in
the TUI's half-block language, and a report sent from it says it came from
the TUI.

## Shared (`shelbi-app`)

- `shelbi_app::feedback` (was the desktop's `platform/feedback.rs`): the
  request body, the answer's classification, the remembered name and email,
  the curl transport and the `FeedbackService` seam. New: `Client`
  (`Desktop` sends `appName` `"Shelbi"` as before, `Tui` sends
  `"Shelbi TUI"`; the endpoint shows `appName` as the issue's App), and `Os`
  (`macOSVersion` on a Mac; elsewhere `osVersion` from `uname -sr`, which
  the endpoint doesn't show yet). curl is `/usr/bin/curl` on a Mac, `curl` on
  the PATH elsewhere.
- `shelbi_app::feedback::form` (was `views/feedback_state.rs`): the form's
  state machine, unchanged, plus the dialog's words (title, placeholders,
  buttons, the thank-you, `system_info_label()`, which says "OS version" off
  a Mac) so both clients show the same ones.
- The remembered name and email moved to `~/.shelbi/feedback.json` (shared by
  both clients; the desktop's `desktop-feedback.json` never shipped in a
  release), and `SHELBI_DESKTOP_FEEDBACK_URL` became `SHELBI_FEEDBACK_URL`.
- Theme tokens: `dialog` (the panel), `dialog_control` (a field, the
  segmented track, a secondary button at rest), `segment` (the picked
  segment's chip: the canvas, as the desktop's), `primary` /
  `primary_focus` / `primary_label` (Send Message, the desktop's primary tint
  over the panel), `magenta_tint` (the heart's badge). Contrast is tested in
  both themes.

## Desktop

`platform/feedback.rs` re-exports the shared module and adds the
`FeedbackGlobal` test seam (a gpui global can't be implemented on a foreign
type). The view reads its words from `form`. Behavior is unchanged; its tests
pass as before.

## TUI

- `overlay::feedback::FeedbackDialog` over the shared form, as
  `ActiveOverlay::Feedback`, centered over the whole window. The panel is a
  `dialog` fill with a `▄` row above and a `▀` row below. In the desktop's
  order: the megaphone badge and "Contact Us"; Name and Email (placeholders,
  no labels); Bug Report / Feature Request / Other; Message (5 to 9 lines,
  then it scrolls with its caret); "Include system info"; Send Message and
  Cancel full width. The thank-you: the heart, "Thank you!", its line,
  Close.
- Short terminals drop the badge, then the panel's padding rows; past that
  the fields scroll under the pinned error and buttons (`modal_form`, moved
  to the crate root to share with Settings' modals).
- Keys as the desktop's, with Ctrl+Return beside ⌘Return (a terminal rarely
  delivers ⌘). Clicks on every control (a segment, the checkbox, each box's
  three rows, the buttons). A bracketed paste goes into the text field being
  typed in (on one line in Name and Email).
- The send runs on its own thread through `ShellState::feedback_service`
  (offline in tests); `poll_feedback_job` hands the answer back.

## Tests

`overlay::feedback::tests` (layout order, fills and focus in both themes,
cramped sizes, Tab order, typing and paste, validation, clicks, sending /
failure / thank-you, the message's scroll), `shell::tests::feedback` (footer
and palette open it, a send end to end through a fake service with the
payload, the remembered file and a failure with a retry), and the shared
module's own tests.
