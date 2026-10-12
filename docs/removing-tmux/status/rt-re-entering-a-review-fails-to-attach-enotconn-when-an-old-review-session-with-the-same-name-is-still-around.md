# rt-re-entering-a-review-fails-to-attach-enotconn-when-an-old-review-session-with-the-same-name-is-still-around

Done. When several sessions share a name, every name lookup now picks a *usable*
one (lock held and socket accepting), preferring the most recently launched and
never a refusing zombie; the supervisor reaps a zombie left beside its live
replacement; and the TUI connect path treats ENOTCONN/EPIPE/ECONNRESET like a
refused socket (retry, then a clear terminal error).

Notes:
- New shared helpers `shelbi_client::choose_session` / `zombies_to_reap` back the
  TUI `LiveConnector`, the backend `find_live` / `find_any` / `live_session_names`,
  and the poller reap. They probe sockets only when a name has more than one live
  candidate, so the common one-session case costs nothing extra.
- `Meta` gained a `pid` field (`#[serde(default)]`, so older `meta.json` still
  parses) so a supervisor can SIGTERM an unreachable zombie out of band. A
  pre-field session (pid 0) is still dropped from discovery but can't be signaled.
- No `Cargo.lock` change (no new deps), so no MSRV step. `meta.json` is ephemeral
  session state, not a config surface, so no config-upgrade sniffer is needed.
