#!/bin/sh
# Claude Code's SessionStart hook. Claude Code on the web starts each session on a fresh
# machine, so this installs what the build and CI's checks need before the agent starts.
# Local sessions use the machine as it is: this does nothing there.
[ "${CLAUDE_CODE_REMOTE:-}" = true ] || exit 0

# What this prints goes into the agent's context: the summary, not apt's output.
log=${TMPDIR:-/tmp}/arakun-setup-dev.log
if "$CLAUDE_PROJECT_DIR/tools/setup-dev.sh" --yes --relay >"$log" 2>&1; then
  echo "tools/setup-dev.sh prepared this machine for the build and the checks (log: $log)."
else
  echo "tools/setup-dev.sh failed (log: $log). The end of it:"
  tail -n 20 "$log"
fi
# A failed setup should not block the session; the agent can read the log and retry.
exit 0
