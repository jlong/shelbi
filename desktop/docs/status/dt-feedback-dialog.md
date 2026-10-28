# dt-feedback-dialog: Contact Us

## The change

The sidebar footer used to put a flexible space first and the Settings cog
on the right. It now reads, left to right, **Settings** and a **Send
Feedback** megaphone in the bottom-left corner (Figma Navigation,
`2220:17016`: the footer Toolbar's two 32pt Toolbar Buttons, 4pt apart),
then the unread-errors badge while there are errors, then the flexible
space. The Zen pill stays below. Settings keeps its tooltip (with the
daemon line when there is one) and its click.

The megaphone, and **Help ▸ Send Feedback…**, open **Contact Us** over the
window, modeled on ContextStore's feedback sheet. It posts to the same
32pixels.co endpoint ContextStore uses.

```
            (📣)            the megaphone on the Primary tint
         Contact Us         Desktop/Heading
  [ Name                  ]
  [ Email                 ]
  [ Bug Report | Feature Request | Other ]   Bug Report by default
  ┌────────────────────────┐
  │ Message                │   5 lines, grows to 9
  └────────────────────────┘
  [✓] Include system info (macOS version, app version)
  Too many submissions. Please try again later.   (an error, when there is one)
  (      Send Message      )   Primary, Base, full width
  (         Cancel         )   Default, Base, full width
```

A send that worked swaps the form for the thank-you: the heart on the
magenta tint, "Thank you!", "Your input helps us build a better app. We'll
follow up if we have questions.", and Close.

## Design

The Figma file has no Contact Us frame yet (the task's dialog link opens
the Navigation component set; asked as `q-001`). The panel is built from
the Components page's parts at the workspace dialog's scale: the opaque
canvas panel (440pt wide, 32pt in, 12pt corners, the divider edge) on the
scrim, centered over the whole window; Textboxes; the Segmented Control
across the form, its items sharing the width; a multi-line Textbox
(`controls::textarea_box`, the Textbox's edge, well and glow around a
`Textarea`); the Checkbox; Base Buttons. Every color is a palette role, so
Light and Dark both follow the theme. Icons are Heroicons v2.2.0 outline
(`megaphone`, `heart`), vendored like the rest and drawn at 20pt through
`heroicon`. ContextStore's switch became the Checkbox, the control Shelbi's
components draw for an on/off choice.

`--visual-figma` saves `state-feedback`, `-error`, `-sending` and
`-thanks` in both themes (`SHELBI_VISUAL_ONLY=feedback`), checked by eye.

## How it works

- `src/platform/feedback.rs`: the request and the answer. `payload` builds
  the JSON body (`appName` `"Shelbi"`, `email`, `type`, `message`,
  `timestamp` in ISO 8601 UTC; `name` only when given; `appVersion` and
  `macOSVersion` only with system info on). `classify` reads the answer:
  2xx sent; 422 the body's `error`; 429 "Too many submissions. Please try
  again later."; anything else "Server error (HTTP n)", with the body's
  message when it's a short line. `FeedbackService` is the seam: the live
  one posts through `/usr/bin/curl` (as the update check does), reads
  `sw_vers` and the clock, and remembers the name and email in
  `~/.shelbi/desktop-feedback.json`. `SHELBI_DESKTOP_FEEDBACK_URL` posts
  elsewhere.
- `src/views/feedback_state.rs`: the toolkit-free form. Validation (email,
  then message, required after trimming; "Please enter your email" / "Please
  enter a message", the keyboard moved to the field), the one send in
  flight (a second press, Esc and Cancel do nothing while it's out), the
  error kept beside the draft for a retry, editing a field clearing it.
- `src/views/feedback.rs`: the view. It runs the send on the background
  executor and hands the result back to the form.
- `ShelbiWindow` holds the open dialog over everything (above Settings'
  dialogs, under an overlay) with the focus to give back when it closes.
  It is modal: the window's keys wait, ⌘W cancels it, and the menus'
  commands wait (Minimize, Zoom and the app-wide ones still run). Opening
  it closes an open palette first. It belongs to no project, so a project
  switch keeps it.

## Keys

Tab and ⇧Tab walk Name, Email, the kind, Message, the checkbox, Send
Message and Cancel, wrapping inside the dialog. Return is Send Message
(on Cancel, Cancel). In Message, Return starts a new line, as in
ContextStore's text editor, and ⌘Return sends. Esc cancels. Space toggles
the checkbox or presses the focused button. ← and → pick the kind on the
segmented control. The focus ring shows on the segmented control, the
checkbox and the buttons once the keyboard is used; the text fields show
their focus edge always. The keyboard starts on Name when no name is
remembered, else Email when no email is, else Message.

## Decisions

- **A Help menu item, not a registry command.** Contact Us is desktop-only
  and needs no daemon, so it is an `AppCommand` like About and Report an
  Issue, not a shared `CommandKind` (which would need a TUI arm). It is not
  in the command palette, which lists registry commands only.
- **Name and email are remembered when a send is attempted** (validation
  passed), not on every keystroke, so cancelling leaves nothing behind.
- **`appVersion` is the app's version alone.** The bundle's build number
  (`CFBundleVersion`) is the same string, so `"0.12.0 (0.12.0)"` would say
  nothing more.
- **A send that's out can't be cancelled.** The request is already on its
  way; closing would hide whether it landed. curl gives up after 20s.
- **Network failures** (no answer at all) show "Couldn't send your message:"
  and curl's reason, and keep the draft like the other failures.

## Tests

`src/app_feedback_tests.rs` (the window: the footer's corner, the
megaphone and the menu item opening it, Esc / Cancel / ⌘W closing it
unsent, the payload with and without system info and with the kind picked,
the keyboard walk, ⌘Return, validation, each failure class shown inline
with the draft kept and a retry, a double press sending once with
"Sending…" meanwhile, the thank-you, the name and email remembered and
prefilled), `views::feedback_state::tests` and `platform::feedback::tests`.
Every test sets its own `FeedbackService` with a fake transport; one that
sets none gets `FeedbackService::offline`, so no test can reach the real
endpoint.
