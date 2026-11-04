#!/bin/sh
# SessionStart hook (matcher "compact"): after a compaction, points the orchestrator back to its brief.
# In-session subagents inherit the orchestrator's session id, so the note is conditional on the
# role the summary records, and it is skipped when the hook input names a subagent.
# Never fails the session.

root="${CLAUDE_PROJECT_DIR:-.}"
brief="$root/.agents/orchestrator.md"
[ -f "$brief" ] || exit 0
[ -n "$CLAUDE_CODE_REMOTE_SESSION_ID" ] || exit 0

input="$(cat 2>/dev/null || true)"
case "$input" in
  *'"agent_id"'*) exit 0 ;;
esac

current="${CLAUDE_CODE_REMOTE_SESSION_ID#*_}"
declared="$(sed -n 's/^Orchestrator session: *[a-z]*_\([A-Za-z0-9]*\).*/\1/p' "$brief" | head -n 1)"
[ -n "$declared" ] && [ "$current" = "$declared" ] || exit 0

cat <<'NOTE'
You were just compacted. If your summary says you are the orchestrating session, reread `.agents/orchestrator.md` now (it is binding: you plan, delegate, decide and merge and never do the work yourself) and update its "In flight" section if it is stale. If you are a worker, subagent or reviewer, ignore this note and continue your task brief.
NOTE
exit 0
