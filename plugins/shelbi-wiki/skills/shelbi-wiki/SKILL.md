---
name: shelbi-wiki
description: Read and write the project's wiki with `shelbi wiki`. Use it to find project context, plans, designs and decisions before starting work, and to write any plan, design doc, spec, research note or decision record (those go in the wiki, never in a repo file).
---

# The project wiki

Every Shelbi project can have a wiki: a folder of Markdown documents that the
people and agents on the project share. It holds the long-form writing that
doesn't belong in the code: plans, designs, specs, research, decisions, and
notes about how the project works. The `shelbi wiki` command reads and writes
it from any agent session.

The hub keeps the wiki. It commits a few seconds after each change, pulls and
pushes on its own, and tells the desktop app and the TUI, so a document you
write shows up for everyone right away. Usually the wiki lives on an orphan
branch of the project repo (`docs/planning` by default), checked out by the hub
outside every workspace. It may instead be a folder of the main repo (`docs/`
by default), in which case your edits ride a rolling pull request.

## When to read it

- **Before starting a task.** If the task names a plan (`Plans/<name>.md`, a
  `wiki:Plans/<name>.md` link, "see the plan"), read it. Otherwise look for
  one: `shelbi wiki search "<feature or keyword>"`.
- **When you need project context**: architecture, conventions, earlier
  decisions and why they were made. `shelbi wiki tree --documents` shows what
  exists.
- **Before writing a new plan or design**, to extend an existing doc rather
  than start a second one on the same subject.

## When to write it

- **A plan or design doc is a wiki doc, never a repo file.** Don't add
  `PLAN.md`, `docs/design-*.md`, `notes/` or similar to the code tree; write
  `Plans/<name>.md` in the wiki instead. (A project whose wiki is a repo folder
  is the one exception, and the wiki command still does the writing.)
- **When a decision is made** (by you, or the user answering your question),
  record it in the doc it belongs to, with the reason.
- **When a plan changes as you build it**, update the plan so it stays true.
  Check off its tasks as they land.
- **Research or findings worth keeping** go in the wiki too (`Research/`).

Don't write task status or progress logs there: the board and the task's
messages carry those.

## Rules

- **Only through `shelbi wiki`.** Never edit, create or delete files in the
  wiki's checkout directly, and never commit or push it yourself. The hub is
  the wiki's single writer; it commits and syncs every change.
- **Paths are relative to the wiki's root**: `Plans/auth.md`, not a file
  system path. A path ending in `.md` is a document; any other path is a
  folder.
- **Edit sections, not whole documents**, when you change part of a doc:
  `--section "## Heading"` (the heading as written) keeps everyone else's
  edits to the rest of it.
- **Use `--agent`** when you parse the output: JSON, no color. Errors come back
  as `{"error": {"kind", "message"}}` on stderr.
- **No wiki yet?** If a command says the project has no wiki, tell the user (or
  mention it in your handoff) rather than writing the doc into the repo.
  Setting a wiki up is the user's call.

## Commands

The project comes from `--project <name>`, else `$SHELBI_PROJECT`, else the
current directory, so inside a workspace you rarely need the flag. Content
comes from `--content` or stdin. `shelbi wiki --help` and `shelbi wiki
<command> --help` have every flag.

### Find and read

```sh
shelbi wiki tree --documents                  # every folder and document
shelbi wiki tree Plans -L 1                   # one level under Plans
shelbi wiki ls Plans --stats                  # one folder, with line/word counts
shelbi wiki ls Plans/auth.md                  # a document's headings
shelbi wiki read Plans/auth.md                # the whole document
shelbi wiki read Plans/auth.md --section "## Decisions"
shelbi wiki read Plans/auth.md --head 40      # the first 40 lines
shelbi wiki cat Plans/ --headers              # every document in a folder
shelbi wiki search "rate limit"               # case-insensitive substring
shelbi wiki search "PIX-\d+" --regex --folder Plans
shelbi wiki log Plans/auth.md -n 5            # who changed it, and when
```

### Write

```sh
# A new plan. The H1 comes from --title (or the file name).
shelbi wiki new Plans/auth.md --title "Auth" --content "## Context

Why we're doing this.

## Design

## Tasks
- [ ] Token refresh
- [ ] Session expiry"

# Long content: pipe it in.
cat draft.md | shelbi wiki new Plans/auth.md

# Replace one section's body (its heading stays).
shelbi wiki write Plans/auth.md --section "## Design" --content "Use short-lived tokens."

# Add to the end of a section, or of the document.
shelbi wiki append Plans/auth.md --section "## Decisions" \
  --content "- Tokens expire after 15 minutes (agreed with the user, 2026-11-27)."

# Replace the whole text (frontmatter and H1 kept unless the content has its own).
cat auth.md | shelbi wiki write Plans/auth.md

shelbi wiki new Research                      # a folder
shelbi wiki mv Plans/auth.md Archive/         # into a folder that exists
shelbi wiki mv Plans/a.md Plans/b.md          # rename
shelbi wiki rm Plans/old.md                   # stays in the history
```

`new` refuses a path that exists, and `write` needs one that does: read first,
then pick the command.

### Task lists

```sh
shelbi wiki tasks Plans/auth.md --pending     # open checkboxes
shelbi wiki tasks Plans/                      # across a folder
shelbi wiki check Plans/auth.md "Token refresh"
shelbi wiki uncheck Plans/auth.md "Token refresh"
```

`check` and `uncheck` match the task's text (case-insensitive); a match on
more than one task fails, so pass enough of the text to be unique.

### Sync

```sh
shelbi wiki sync                              # status: synced, pending, syncing, offline, conflict
shelbi wiki sync now                          # commit, pull and push now, and wait
shelbi wiki diff                              # the change not yet committed
shelbi wiki resolve Plans/auth.md             # a held document's hunks (ours vs theirs)
shelbi wiki resolve Plans/auth.md --theirs    # settle it: --ours keeps this machine's
```

You don't need to sync after writing; the hub does it within seconds. Run
`sync now` only when someone else must see the change before you go on (on
another machine, say). On `conflict`, `sync` lists the held documents. Don't
pick a side yourself unless the user asked you to: tell them, and they
resolve it in the app (side by side, hunk by hunk) or with `shelbi wiki
resolve`.

## Exit codes

0 ok, 1 error (including a project with no wiki), 2 not found, 3 invalid (bad
arguments, a path that already exists, a `check` that matched more than one
task), 4 conflict (the document is held by a sync conflict, or changed while
you edited it: read it again, or tell the user).
