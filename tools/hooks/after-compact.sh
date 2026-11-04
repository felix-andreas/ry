#!/bin/sh
# SessionStart hook (matcher "compact"): re-injects the orchestrator brief after a compaction.
# Only the session named on the "Orchestrator session:" line of orchestrator.md gets it.
# Never fails the session.

root="${CLAUDE_PROJECT_DIR:-.}"
brief="$root/.agents/orchestrator.md"
[ -f "$brief" ] || exit 0
[ -n "$CLAUDE_CODE_REMOTE_SESSION_ID" ] || exit 0

current="${CLAUDE_CODE_REMOTE_SESSION_ID#*_}"
declared="$(sed -n 's/^Orchestrator session: *[a-z]*_\([A-Za-z0-9]*\).*/\1/p' "$brief" | head -n 1)"
[ -n "$declared" ] && [ "$current" = "$declared" ] || exit 0

cat <<'HEADER'
You were just compacted. You are the orchestrator, and the brief below is binding: you plan, delegate, read reports, decide and merge, and you never do the work yourself.

HEADER
cat "$brief"
cat <<'FOOTER'

Before anything else, check that the "In flight" section matches reality (running sessions, pending merges, scheduled tasks, user decisions since the last update), and commit an update to it if it is stale.
FOOTER
exit 0
