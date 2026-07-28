---
name: update-shelbi-configuration
description: Safely inspect, preview, validate, and apply changes to Shelbi-owned configuration.
---

# Update Shelbi configuration

Use this workflow whenever a user asks to change Shelbi configuration. Shelbi's
CLI is authoritative for the current version's paths, candidate layout,
schemas, validation, and lifecycle ownership. Do not guess configuration paths
or copy schemas from examples.

## 1. Inventory and preserve the starting point

Run `shelbi config inventory --format json`, adding `--project NAME` when the
request is project-specific or `--all` only when the request explicitly spans
all projects. Read the returned inventory JSON. Work only inside its
`staged_dir`; never experiment on live files.

Immediately make a separate, byte-for-byte baseline copy of the entire staged
directory. Keep both directories until the workflow finishes. The baseline is
the inventory-time source snapshot used for previews and race detection. Do
not edit its manifest or files.

Use inventory entries, including `logical_id`, `canonical_path`,
`candidate_path`, `exists`, and `lifecycle_owned`, to decide what is in scope.
Do not add a new surface unless a fresh inventory from the current CLI
describes it.

## 2. Edit, lint, and fix only staged candidates

Make the requested edits in `staged_dir`, preserving unrelated bytes and
comments where the format permits. Run:

`shelbi config lint --staged <staged_dir> --format json`

If lint reports any warning or error, fix only the staged candidates and lint
again. Repeat until lint exits successfully with no diagnostics. Do not
silence, waive, or omit warnings.

## 3. Produce one exact preview

Compare the untouched baseline directory with `staged_dir` and prepare one
combined filesystem diff containing every proposed byte change, addition, and
deletion. Exclude no changed candidate. Do not summarize the diff in place of
showing it.

Alongside that single diff, show one ordered operational command list. Include
the exact official Shelbi lifecycle commands required by changed entries whose
inventory says `lifecycle_owned: true`, plus the final live lint command.
Discover command syntax from the installed `shelbi` CLI help when necessary;
do not rely on version-specific prose in this skill.

Present the exact combined diff and the complete command list together, then
ask for explicit confirmation to apply exactly that preview. A general request
to update configuration is not confirmation. Do not write live files or run
operational commands until the user clearly confirms this exact preview.

## 4. Detect races before applying

After confirmation and immediately before the first live write, compare every
in-scope live `canonical_path` with its inventory-time counterpart in the
untouched baseline:

- an entry that existed must still exist with identical bytes;
- an entry that did not exist must still be absent;
- no inventory identity or canonical target may have changed.

If any source changed, stop without applying anything. Report the changed
logical ids, discard the proposed apply, take a fresh inventory, re-stage and
lint the request, and present a new exact preview for a new confirmation.
Never merge an unreviewed concurrent change into the confirmed proposal.

## 5. Apply exactly the confirmed change

Apply only the confirmed candidate changes to their inventory-provided
canonical paths. Use same-directory temporary files and atomic replacement for
file writes. Preserve existing file permissions where applicable. Do not
rewrite unchanged files, follow candidate symlinks, or touch runner-owned
Claude/Codex configuration.

Run the confirmed official lifecycle commands in order when the inventory
marks affected surfaces as lifecycle-owned. If a command fails, stop the
remaining command list, inspect its output and current live state, and attempt
only a recovery that preserves the confirmed filesystem diff. Re-run the
failed command after that bounded repair. If recovery would change the
confirmed diff or add a new command, show the new exact diff and full revised
command list and require fresh explicit confirmation.

## 6. Validate the live result

Run the live form of `shelbi config lint` with the same project selection used
for inventory and `--format json`. Warnings and errors both mean the update is
not complete. Repair only discrepancies from the confirmed apply, re-run any
required lifecycle command, and lint live configuration again.

Finish by reporting the files changed, operational commands run and their
outcomes, and the clean final lint result. Remove the temporary staged and
baseline directories only after the live configuration is clean or after
reporting an unrecoverable failure.
